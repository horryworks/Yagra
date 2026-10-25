// SPDX-License-Identifier: AGPL-3.0-only
//! The SNMP v2c exchange: one GET or one GETBULK, sent and answered (ADR-213 decision 4).
//!
//! The bytes are `snmp2`'s — `pdu::build` encodes a request and `Pdu::from_bytes` decodes a reply,
//! the same codec the v3 client and the trap receiver use. **The exchange is ours.** `snmp2`'s own
//! `AsyncSession` receives once and checks the request-id afterwards, so after a timeout the late
//! reply is read as the answer to the next request (ADR-161). The column walk in `snmp.rs` sends a
//! timed-out page again from the same cursor (ADR-110 Increment 9), which on that session would be
//! answered by the stale reply every time. Here a reply is accepted only from the connected agent
//! with the request-id that was sent, and anything else is read past until the deadline — which is
//! what `csnmp`, the client this replaced, did.
//!
//! What a reply has to look like is also `csnmp`'s rule, so that a device is judged exactly as
//! before: a GET answers one binding with the name that was asked; a GETBULK answers names greater
//! than the cursor, none twice; `endOfMibView` marks the end; `noSuchObject`, `noSuchInstance` or
//! an empty value is a failed binding — an answer, never silence.

use std::collections::BTreeMap;
use std::fmt;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::atomic::{AtomicI32, Ordering};
use std::time::Duration;

use snmp2::{pdu, snmp, MessageType, Pdu, Value, Version};
use tokio::net::UdpSocket;

/// Largest datagram read. A v2c reply cannot be larger than a UDP payload.
const RECV_BUFFER: usize = 65_535;

/// An object identifier, owned. Ordered the way SNMP orders OIDs: arc by arc, a prefix first.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct ObjectIdentifier(Vec<u32>);

impl ObjectIdentifier {
    /// The arcs after `base`, if `base` is a prefix of (or equal to) this OID — empty when equal.
    pub(crate) fn relative_to(&self, base: &Self) -> Option<&[u32]> {
        self.0.strip_prefix(base.0.as_slice())
    }

    /// Whether `other` lies in this OID's subtree (this OID itself included).
    pub(crate) fn is_prefix_of_or_equal(&self, other: &Self) -> bool {
        other.0.starts_with(&self.0)
    }

    fn to_wire(&self) -> Option<snmp2::Oid<'static>> {
        let arcs: Vec<u64> = self.0.iter().map(|&a| u64::from(a)).collect();
        snmp2::Oid::from(arcs.as_slice()).ok()
    }

    fn from_wire(oid: &snmp2::Oid<'_>) -> Option<Self> {
        oid.iter()?
            .map(|a| u32::try_from(a).ok())
            .collect::<Option<Vec<u32>>>()
            .map(Self)
    }
}

impl TryFrom<&[u32]> for ObjectIdentifier {
    type Error = ();

    /// Refuses what cannot be encoded: fewer than two arcs, or a first arc above 2 (X.690).
    fn try_from(arcs: &[u32]) -> Result<Self, ()> {
        let oid = Self(arcs.to_vec());
        match arcs {
            [first, _, ..] if *first <= 2 && oid.to_wire().is_some() => Ok(oid),
            _ => Err(()),
        }
    }
}

impl fmt::Display for ObjectIdentifier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut first = true;
        for arc in &self.0 {
            if !first {
                f.write_str(".")?;
            }
            write!(f, "{arc}")?;
            first = false;
        }
        Ok(())
    }
}

/// A value an agent can answer with, owned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ObjectValue {
    Integer(i64),
    Counter32(u32),
    Unsigned32(u32),
    TimeTicks(u32),
    Counter64(u64),
    String(Vec<u8>),
    Opaque(Vec<u8>),
    IpAddress(Ipv4Addr),
    ObjectId(ObjectIdentifier),
}

/// What sits in a binding instead of a value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BindingValue {
    NoSuchObject,
    NoSuchInstance,
    EndOfMibView,
    /// `NULL` — what an agent leaves in a binding it could not fill.
    Unspecified,
}

/// One binding of a reply.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Binding {
    Value(ObjectValue),
    Exception(BindingValue),
}

/// Why an exchange produced no usable answer. The split that matters is in `snmp.rs`'s
/// `outcome_of`: the first four mean the agent said nothing; the rest are answers.
#[derive(Debug)]
pub(crate) enum SnmpClientError {
    /// Nothing from the agent with this request-id inside the timeout.
    TimedOut,
    /// The socket could not be opened or connected.
    Socket(std::io::Error),
    /// The request could not be sent.
    Sending(std::io::Error),
    /// The socket failed while waiting — on Linux this is also how an ICMP port-unreachable
    /// arrives on a connected UDP socket.
    Receiving(std::io::Error),
    /// The request could not be encoded (an OID the codec refuses).
    Encoding,
    /// The reply could not be decoded, or carried a value type a v2c GET/GETBULK never answers.
    Decoding,
    /// A PDU that is not a Response.
    NotAResponse,
    /// The agent set a non-zero error-status.
    ErrorStatus(u32),
    /// A binding held an exception instead of a value.
    FailedBinding {
        oid: ObjectIdentifier,
        value: BindingValue,
    },
    /// The bindings were not the ones asked for: the wrong count or name for a GET; a name not
    /// past the cursor, or repeated, for a GETBULK.
    UnexpectedBindings,
}

impl fmt::Display for SnmpClientError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TimedOut => f.write_str("timed out"),
            Self::Socket(e) => write!(f, "socket: {e}"),
            Self::Sending(e) => write!(f, "send: {e}"),
            Self::Receiving(e) => write!(f, "receive: {e}"),
            Self::Encoding => f.write_str("request could not be encoded"),
            Self::Decoding => f.write_str("reply could not be decoded"),
            Self::NotAResponse => f.write_str("reply is not a Response PDU"),
            Self::ErrorStatus(s) => write!(f, "error-status {s}"),
            Self::FailedBinding { oid, value } => write!(f, "{oid}: {value:?}"),
            Self::UnexpectedBindings => f.write_str("reply bindings do not match the request"),
        }
    }
}

/// One GETBULK answer.
#[derive(Debug, Default)]
pub(crate) struct BulkResult {
    /// The values, in OID order.
    pub(crate) values: BTreeMap<ObjectIdentifier, ObjectValue>,
    /// The agent signalled `endOfMibView` somewhere in this page.
    pub(crate) end_of_mib_view: bool,
}

/// A v2c client for one agent: a UDP socket connected to it, so the kernel drops datagrams from any
/// other address before they reach us.
pub(crate) struct Snmp2cClient {
    socket: UdpSocket,
    community: Vec<u8>,
    timeout: Duration,
    next_request_id: AtomicI32,
}

impl Snmp2cClient {
    /// Open a socket to `target`. Speaks nothing yet, so a silent device does not fail here.
    pub(crate) async fn new(
        target: SocketAddr,
        community: Vec<u8>,
        timeout: Duration,
    ) -> Result<Self, SnmpClientError> {
        let bind: SocketAddr = match target.ip() {
            IpAddr::V4(_) => (Ipv4Addr::UNSPECIFIED, 0).into(),
            IpAddr::V6(_) => (std::net::Ipv6Addr::UNSPECIFIED, 0).into(),
        };
        let socket = UdpSocket::bind(bind)
            .await
            .map_err(SnmpClientError::Socket)?;
        socket
            .connect(target)
            .await
            .map_err(SnmpClientError::Socket)?;
        Ok(Self {
            socket,
            community,
            timeout,
            // Random, so an off-path sender that forges the agent's address must also guess it, not
            // only our port. csnmp started at 0; nothing reads the value but the match below.
            next_request_id: AtomicI32::new(
                i32::from_ne_bytes(rand::random::<[u8; 4]>()) & 0x3fff_ffff,
            ),
        })
    }

    /// GET one OID.
    pub(crate) async fn get(&self, oid: &ObjectIdentifier) -> Result<ObjectValue, SnmpClientError> {
        let mut bindings = self.exchange(snmp::MSG_GET, oid, 0, 0).await?;
        if bindings.len() != 1 {
            return Err(SnmpClientError::UnexpectedBindings);
        }
        let (name, binding) = bindings.remove(0);
        if &name != oid {
            return Err(SnmpClientError::UnexpectedBindings);
        }
        match binding {
            Binding::Value(v) => Ok(v),
            Binding::Exception(value) => Err(SnmpClientError::FailedBinding { oid: name, value }),
        }
    }

    /// GETBULK after `cursor`, with no non-repeaters.
    pub(crate) async fn get_bulk(
        &self,
        cursor: &ObjectIdentifier,
        max_repetitions: u32,
    ) -> Result<BulkResult, SnmpClientError> {
        let bindings = self
            .exchange(snmp::MSG_GET_BULK, cursor, 0, max_repetitions)
            .await?;
        bulk_result(cursor, bindings)
    }

    /// Send one request and wait for its reply.
    async fn exchange(
        &self,
        ident: u8,
        oid: &ObjectIdentifier,
        non_repeaters: u32,
        max_repetitions: u32,
    ) -> Result<Vec<(ObjectIdentifier, Binding)>, SnmpClientError> {
        let request_id = self.next_request_id.fetch_add(1, Ordering::Relaxed);
        let wire_oid = oid.to_wire().ok_or(SnmpClientError::Encoding)?;
        let mut request = Box::<pdu::Buf>::default();
        pdu::build(
            Version::V2C,
            &self.community,
            ident,
            request_id,
            &[(&wire_oid, Value::Null)],
            non_repeaters,
            max_repetitions,
            &mut request,
            None,
        )
        .map_err(|_| SnmpClientError::Encoding)?;
        self.socket
            .send(&request)
            .await
            .map_err(SnmpClientError::Sending)?;

        let deadline = tokio::time::Instant::now() + self.timeout;
        let mut buf = vec![0u8; RECV_BUFFER];
        loop {
            let len = match tokio::time::timeout_at(deadline, self.socket.recv(&mut buf)).await {
                Err(_) => return Err(SnmpClientError::TimedOut),
                Ok(Err(e)) => return Err(SnmpClientError::Receiving(e)),
                Ok(Ok(len)) => len,
            };
            // A datagram that does not decode ends the exchange as an answer rather than being read
            // past: that is csnmp's judgement, and the walk budget's silence rule rests on it — a
            // device sending malformed replies is there, and must not be scored as one that is not.
            match decode_reply(&buf[..len], request_id)? {
                Some(bindings) => return Ok(bindings),
                // A reply to an earlier request — read past it, as csnmp did.
                None => continue,
            }
        }
    }
}

/// Decode one datagram. `Ok(None)` when it answers a different request-id.
fn decode_reply(
    bytes: &[u8],
    request_id: i32,
) -> Result<Option<Vec<(ObjectIdentifier, Binding)>>, SnmpClientError> {
    let reply = Pdu::from_bytes(bytes).map_err(|_| SnmpClientError::Decoding)?;
    if reply.req_id != request_id {
        return Ok(None);
    }
    if reply.message_type != MessageType::Response {
        return Err(SnmpClientError::NotAResponse);
    }
    if reply.error_status != 0 {
        return Err(SnmpClientError::ErrorStatus(reply.error_status));
    }
    let mut bindings = Vec::new();
    for (name, value) in reply.varbinds {
        let name = ObjectIdentifier::from_wire(&name).ok_or(SnmpClientError::Decoding)?;
        bindings.push((name, binding_of(&value)?));
    }
    Ok(Some(bindings))
}

/// One binding's value, owned. A type a v2c agent never answers a GET with is a decode failure.
fn binding_of(value: &Value<'_>) -> Result<Binding, SnmpClientError> {
    Ok(match value {
        Value::Integer(i) => Binding::Value(ObjectValue::Integer(*i)),
        Value::Counter32(c) => Binding::Value(ObjectValue::Counter32(*c)),
        Value::Unsigned32(u) => Binding::Value(ObjectValue::Unsigned32(*u)),
        Value::Timeticks(t) => Binding::Value(ObjectValue::TimeTicks(*t)),
        Value::Counter64(c) => Binding::Value(ObjectValue::Counter64(*c)),
        Value::OctetString(b) => Binding::Value(ObjectValue::String(b.to_vec())),
        Value::Opaque(b) => Binding::Value(ObjectValue::Opaque(b.to_vec())),
        Value::IpAddress(o) => Binding::Value(ObjectValue::IpAddress(Ipv4Addr::from(*o))),
        Value::ObjectIdentifier(oid) => Binding::Value(ObjectValue::ObjectId(
            ObjectIdentifier::from_wire(oid).ok_or(SnmpClientError::Decoding)?,
        )),
        Value::NoSuchObject => Binding::Exception(BindingValue::NoSuchObject),
        Value::NoSuchInstance => Binding::Exception(BindingValue::NoSuchInstance),
        Value::EndOfMibView => Binding::Exception(BindingValue::EndOfMibView),
        Value::Null => Binding::Exception(BindingValue::Unspecified),
        // Listed so a variant added to the codec is a compile error here, not a silent mapping.
        Value::Boolean(_)
        | Value::Sequence(_)
        | Value::Set(_)
        | Value::Constructed(..)
        | Value::GetRequest(_)
        | Value::GetNextRequest(_)
        | Value::GetBulkRequest(_)
        | Value::Response(_)
        | Value::SetRequest(_)
        | Value::InformRequest(_)
        | Value::Trap(_)
        | Value::Report(_) => return Err(SnmpClientError::Decoding),
    })
}

/// A GETBULK reply's bindings as a page, by csnmp's rules (see the module doc).
fn bulk_result(
    cursor: &ObjectIdentifier,
    bindings: Vec<(ObjectIdentifier, Binding)>,
) -> Result<BulkResult, SnmpClientError> {
    let mut page = BulkResult::default();
    for (name, binding) in bindings {
        if &name <= cursor {
            return Err(SnmpClientError::UnexpectedBindings);
        }
        match binding {
            Binding::Value(v) => {
                if page.values.insert(name, v).is_some() {
                    return Err(SnmpClientError::UnexpectedBindings);
                }
            }
            Binding::Exception(BindingValue::EndOfMibView) => page.end_of_mib_view = true,
            Binding::Exception(value) => {
                return Err(SnmpClientError::FailedBinding { oid: name, value })
            }
        }
    }
    Ok(page)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn oid(s: &str) -> ObjectIdentifier {
        let arcs: Vec<u32> = s.split('.').map(|p| p.parse().unwrap()).collect();
        ObjectIdentifier::try_from(arcs.as_slice()).unwrap()
    }

    /// A Response PDU as an agent would send it, built with the same codec.
    fn response(request_id: i32, error_status: u32, bindings: Vec<(&str, Value<'_>)>) -> Vec<u8> {
        let names: Vec<snmp2::Oid<'static>> = bindings
            .iter()
            .map(|(o, _)| oid(o).to_wire().unwrap())
            .collect();
        let refs: Vec<(&snmp2::Oid<'_>, Value<'_>)> = names
            .iter()
            .zip(bindings.into_iter().map(|(_, v)| v))
            .collect();
        let mut buf = Box::<pdu::Buf>::default();
        // non_repeaters / max_repetitions occupy error-status / error-index in a Response.
        pdu::build(
            Version::V2C,
            b"public",
            snmp::MSG_RESPONSE,
            request_id,
            &refs,
            error_status,
            0,
            &mut buf,
            None,
        )
        .unwrap();
        buf.to_vec()
    }

    #[test]
    fn oids_order_arc_by_arc_with_the_prefix_first() {
        assert!(oid("1.3.6.1.2.1.2") < oid("1.3.6.1.2.1.2.1"));
        assert!(
            oid("1.3.6.1.2.1.2.9") < oid("1.3.6.1.2.1.2.10"),
            "numeric, not text"
        );
        let base = oid("1.3.6.1.2.1.2.2.1.8");
        assert_eq!(
            oid("1.3.6.1.2.1.2.2.1.8.7").relative_to(&base),
            Some(&[7][..])
        );
        assert_eq!(base.relative_to(&base), Some(&[][..]));
        assert_eq!(oid("1.3.6.1.2.1.2.2.1.9.7").relative_to(&base), None);
        assert!(base.is_prefix_of_or_equal(&base));
        assert_eq!(base.to_string(), "1.3.6.1.2.1.2.2.1.8");
    }

    #[test]
    fn an_oid_the_codec_cannot_carry_is_refused() {
        assert!(ObjectIdentifier::try_from(&[1u32][..]).is_err());
        assert!(ObjectIdentifier::try_from(&[3u32, 1][..]).is_err());
        assert!(ObjectIdentifier::try_from(&[][..]).is_err());
        assert!(ObjectIdentifier::try_from(&[1u32, 3, 6, u32::MAX][..]).is_ok());
    }

    #[test]
    fn a_reply_decodes_every_value_type_and_round_trips_its_names() {
        let bytes = response(
            7,
            0,
            vec![
                ("1.3.6.1.2.1.1.3.0", Value::Timeticks(42)),
                ("1.3.6.1.2.1.2.2.1.10.1", Value::Counter32(7)),
                ("1.3.6.1.2.1.31.1.1.1.6.1", Value::Counter64(u64::MAX)),
                ("1.3.6.1.2.1.1.5.0", Value::OctetString(b"sw-01")),
                (
                    "1.3.6.1.2.1.4.20.1.3.1",
                    Value::IpAddress([255, 255, 255, 0]),
                ),
                ("1.3.6.1.2.1.2.2.1.8.1", Value::Integer(-5)),
                (
                    "1.3.6.1.2.1.1.2.0",
                    Value::ObjectIdentifier(oid("1.3.6.1.4.1.9").to_wire().unwrap()),
                ),
            ],
        );
        let got = decode_reply(&bytes, 7).unwrap().unwrap();
        let values: Vec<Binding> = got.iter().map(|(_, b)| b.clone()).collect();
        assert_eq!(
            values,
            vec![
                Binding::Value(ObjectValue::TimeTicks(42)),
                Binding::Value(ObjectValue::Counter32(7)),
                Binding::Value(ObjectValue::Counter64(u64::MAX)),
                Binding::Value(ObjectValue::String(b"sw-01".to_vec())),
                Binding::Value(ObjectValue::IpAddress(Ipv4Addr::new(255, 255, 255, 0))),
                Binding::Value(ObjectValue::Integer(-5)),
                Binding::Value(ObjectValue::ObjectId(oid("1.3.6.1.4.1.9"))),
            ]
        );
        assert_eq!(got[0].0, oid("1.3.6.1.2.1.1.3.0"));
    }

    /// 🚨 **The property this module exists for**: a reply to an earlier request is not an answer.
    #[test]
    fn a_reply_to_another_request_is_read_past_not_accepted() {
        let late = response(6, 0, vec![("1.3.6.1.2.1.1.3.0", Value::Timeticks(1))]);
        assert_eq!(decode_reply(&late, 7).unwrap(), None);
    }

    #[test]
    fn an_error_status_and_garbage_are_answers_that_failed() {
        let gen_err = response(7, 5, vec![("1.3.6.1.2.1.1.3.0", Value::Null)]);
        assert!(matches!(
            decode_reply(&gen_err, 7),
            Err(SnmpClientError::ErrorStatus(5))
        ));
        assert!(matches!(
            decode_reply(b"\x30\x03junk", 7),
            Err(SnmpClientError::Decoding)
        ));
    }

    fn bulk(cursor: &str, entries: &[(&str, Binding)]) -> Result<BulkResult, SnmpClientError> {
        bulk_result(
            &oid(cursor),
            entries.iter().map(|(o, b)| (oid(o), b.clone())).collect(),
        )
    }

    #[test]
    fn a_bulk_page_is_sorted_and_end_of_mib_is_a_mark_not_a_row() {
        let v = |i| Binding::Value(ObjectValue::Integer(i));
        let page = bulk(
            "1.3.6.1.2.1.2.2.1.8",
            &[
                ("1.3.6.1.2.1.2.2.1.8.2", v(2)),
                ("1.3.6.1.2.1.2.2.1.8.1", v(1)),
                (
                    "1.3.6.1.2.1.2.2.1.8.3",
                    Binding::Exception(BindingValue::EndOfMibView),
                ),
            ],
        )
        .unwrap();
        let names: Vec<String> = page.values.keys().map(ToString::to_string).collect();
        assert_eq!(names, ["1.3.6.1.2.1.2.2.1.8.1", "1.3.6.1.2.1.2.2.1.8.2"]);
        assert!(page.end_of_mib_view);
    }

    #[test]
    fn a_bulk_page_that_goes_backwards_repeats_or_holds_an_exception_is_refused() {
        let v = Binding::Value(ObjectValue::Integer(1));
        let cursor = "1.3.6.1.2.1.2.2.1.8.5";
        assert!(matches!(
            bulk(cursor, &[("1.3.6.1.2.1.2.2.1.8.5", v.clone())]),
            Err(SnmpClientError::UnexpectedBindings)
        ));
        assert!(matches!(
            bulk(
                cursor,
                &[
                    ("1.3.6.1.2.1.2.2.1.8.6", v.clone()),
                    ("1.3.6.1.2.1.2.2.1.8.6", v.clone())
                ]
            ),
            Err(SnmpClientError::UnexpectedBindings)
        ));
        assert!(matches!(
            bulk(
                cursor,
                &[(
                    "1.3.6.1.2.1.2.2.1.8.6",
                    Binding::Exception(BindingValue::NoSuchObject)
                )]
            ),
            Err(SnmpClientError::FailedBinding {
                value: BindingValue::NoSuchObject,
                ..
            })
        ));
    }

    /// The whole exchange on real sockets, against a scripted agent that first answers with a
    /// stale request-id and then with the right one: the stale datagram is skipped.
    #[tokio::test]
    async fn a_get_skips_a_stale_reply_and_takes_the_matching_one() {
        let agent = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let addr = agent.local_addr().unwrap();
        let script = tokio::spawn(async move {
            let mut buf = vec![0u8; RECV_BUFFER];
            let (len, from) = agent.recv_from(&mut buf).await.unwrap();
            let asked = Pdu::from_bytes(&buf[..len]).unwrap();
            assert!(matches!(asked.message_type, MessageType::GetRequest));
            let id = asked.req_id;
            let stale = response(id - 1, 0, vec![("1.3.6.1.2.1.1.3.0", Value::Timeticks(1))]);
            let right = response(id, 0, vec![("1.3.6.1.2.1.1.3.0", Value::Timeticks(99))]);
            agent.send_to(&stale, from).await.unwrap();
            agent.send_to(&right, from).await.unwrap();
        });
        let client = Snmp2cClient::new(addr, b"public".to_vec(), Duration::from_secs(2))
            .await
            .unwrap();
        let v = client.get(&oid("1.3.6.1.2.1.1.3.0")).await.unwrap();
        assert_eq!(v, ObjectValue::TimeTicks(99));
        script.await.unwrap();
    }

    #[tokio::test]
    async fn a_silent_agent_times_out_and_a_no_such_object_is_a_failed_binding() {
        let agent = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let addr = agent.local_addr().unwrap();
        let client = Snmp2cClient::new(addr, b"public".to_vec(), Duration::from_millis(100))
            .await
            .unwrap();
        assert!(matches!(
            client.get(&oid("1.3.6.1.2.1.1.3.0")).await,
            Err(SnmpClientError::TimedOut)
        ));

        let script = tokio::spawn(async move {
            let mut buf = vec![0u8; RECV_BUFFER];
            // The timed-out request arrives first; answer only the second.
            let _ = agent.recv_from(&mut buf).await.unwrap();
            let (len, from) = agent.recv_from(&mut buf).await.unwrap();
            let id = Pdu::from_bytes(&buf[..len]).unwrap().req_id;
            let reply = response(id, 0, vec![("1.3.6.1.2.1.1.9.0", Value::NoSuchObject)]);
            agent.send_to(&reply, from).await.unwrap();
        });
        let client = Snmp2cClient::new(addr, b"public".to_vec(), Duration::from_secs(2))
            .await
            .unwrap();
        assert!(matches!(
            client.get(&oid("1.3.6.1.2.1.1.9.0")).await,
            Err(SnmpClientError::FailedBinding {
                value: BindingValue::NoSuchObject,
                ..
            })
        ));
        script.await.unwrap();
    }
}
