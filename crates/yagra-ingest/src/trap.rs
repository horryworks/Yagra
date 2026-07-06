//! SNMP trap / inform normalization over the vendored `snmp2` decode types.
//!
//! Accepts v1 `Trap-PDU` (mapped to a v2c trap OID per RFC 3584 §3.1) and v2c
//! `SNMPv2-Trap-PDU` / `InformRequest-PDU`. **SNMPv3 traps are out of scope for this
//! release**: receiving them would require the poller to hold USM users/keys per sending
//! engine, which conflicts with ADR-020 (pollers only ever receive decrypted secrets
//! inlined per job over the bus) — revisit when a listener-config channel exists.
//!
//! This is the first server-side use of `snmp2`'s decode path with attacker-controlled
//! datagrams: everything returns `Result`, nothing panics (hostile-input tests below),
//! and outputs are capped ([`MAX_VARBINDS`], [`MAX_VALUE_CHARS`]).

use crate::clip_chars;
use snmp2::{MessageType, Pdu, Value};
use thiserror::Error;

/// Cap on the number of varbinds kept from one trap.
pub const MAX_VARBINDS: usize = 32;
/// Cap on one rendered varbind value, in characters.
pub const MAX_VALUE_CHARS: usize = 256;

/// snmpTrapOID.0 — identifies the trap in v2c (RFC 3416).
const SNMP_TRAP_OID_0: &str = "1.3.6.1.6.3.1.1.4.1.0";
/// sysUpTime.0 — the first varbind of a v2c trap.
const SYS_UPTIME_0: &str = "1.3.6.1.2.1.1.3.0";

/// Errors normalizing a trap datagram.
#[derive(Debug, Error)]
pub enum TrapError {
    /// The datagram is not a parseable SNMP message.
    #[error("malformed SNMP datagram: {0}")]
    Malformed(String),
    /// Parsed fine but is not a trap/inform (e.g. a stray GET hitting the port).
    #[error("not a trap or inform PDU")]
    NotATrap,
    /// SNMPv3 (or other unsupported version) — out of scope for the listener.
    #[error("unsupported SNMP version for trap reception")]
    UnsupportedVersion,
}

/// A normalized trap or inform.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrapEvent {
    /// SNMP version: 1 or 2.
    pub version: u8,
    /// Community string (lossy UTF-8). Checked by the caller against the configured
    /// community; **never logged**.
    pub community: String,
    /// The trap identity OID (v2c: snmpTrapOID.0; v1: mapped per RFC 3584).
    pub trap_oid: String,
    /// sysUpTime at the sender, in timeticks, if present.
    pub uptime_ticks: Option<u64>,
    /// Varbinds as dotted-OID → rendered-value pairs (≤ [`MAX_VARBINDS`] entries,
    /// values clipped to [`MAX_VALUE_CHARS`] chars).
    pub varbinds: Vec<(String, String)>,
    /// Whether this is an inform (the caller must send back the Response PDU —
    /// see [`build_inform_response`]).
    pub is_inform: bool,
}

impl TrapEvent {
    /// Render the single matchable text for rules: `"<trap_oid> oid=value; …"`.
    /// The identity varbinds (sysUpTime.0 / snmpTrapOID.0) are skipped — they are
    /// already carried by `trap_oid`/`uptime_ticks` and would only add noise.
    #[must_use]
    pub fn render_message(&self) -> String {
        let mut out = self.trap_oid.clone();
        for (oid, value) in &self.varbinds {
            if oid == SNMP_TRAP_OID_0 || oid == SYS_UPTIME_0 {
                continue;
            }
            out.push(' ');
            out.push_str(oid);
            out.push('=');
            out.push_str(value);
            out.push(';');
        }
        out
    }
}

/// Parse a trap/inform datagram (v1/v2c). Returns a typed error on anything else;
/// never panics on malformed input.
pub fn parse_trap(bytes: &[u8]) -> Result<TrapEvent, TrapError> {
    let pdu = Pdu::from_bytes(bytes).map_err(classify_parse_error)?;

    let version = match pdu.version() {
        Ok(snmp2::Version::V1) => 1,
        Ok(snmp2::Version::V2C) => 2,
        _ => return Err(TrapError::UnsupportedVersion),
    };

    let is_inform = match pdu.message_type {
        MessageType::Trap | MessageType::TrapV1 => false,
        MessageType::InformRequest => true,
        _ => return Err(TrapError::NotATrap),
    };

    let community = String::from_utf8_lossy(pdu.community).into_owned();

    let mut varbinds = Vec::new();
    let mut trap_oid = None;
    let mut uptime_ticks = None;
    for (oid, value) in pdu.varbinds.clone() {
        let oid_str = oid.to_string();
        if oid_str == SNMP_TRAP_OID_0 {
            if let Value::ObjectIdentifier(ref id) = value {
                trap_oid = Some(id.to_string());
            }
        } else if oid_str == SYS_UPTIME_0 {
            if let Value::Timeticks(t) = value {
                uptime_ticks = Some(u64::from(t));
            }
        }
        if varbinds.len() < MAX_VARBINDS {
            varbinds.push((oid_str, render_value(&value)));
        }
    }

    // v1: derive the trap identity from the trap header per RFC 3584 §3.1.
    if pdu.message_type == MessageType::TrapV1 {
        let info = pdu.v1_trap_info.as_ref().ok_or(TrapError::NotATrap)?;
        trap_oid = Some(match info.generic_trap {
            // coldStart(0)..egpNeighborLoss(5) → 1.3.6.1.6.3.1.1.5.<generic+1>
            g @ 0..=5 => format!("1.3.6.1.6.3.1.1.5.{}", g + 1),
            // enterpriseSpecific(6) → <enterprise>.0.<specific>
            _ => format!("{}.0.{}", info.enterprise, info.specific_trap),
        });
        uptime_ticks = Some(u64::from(info.timestamp));
    }

    Ok(TrapEvent {
        version,
        community,
        trap_oid: trap_oid.unwrap_or_default(),
        uptime_ticks,
        varbinds,
        is_inform,
    })
}

/// Build the Response PDU acknowledging an inform, echoing its request-id and varbinds
/// (RFC 3416 §4.2.7). Returns `None` if the datagram doesn't re-parse as an inform —
/// callers only invoke this after [`parse_trap`] said `is_inform`, so `None` is a
/// should-not-happen guard, not a flow.
#[must_use]
pub fn build_inform_response(bytes: &[u8]) -> Option<Vec<u8>> {
    let mut pdu = Pdu::from_bytes(bytes).ok()?;
    if pdu.message_type != MessageType::InformRequest {
        return None;
    }
    // Same PDU (version/community/req_id/varbinds echoed), retagged as a Response.
    pdu.message_type = MessageType::Response;
    pdu.to_bytes().ok()
}

/// Map an `snmp2` parse error to ours. v3 datagrams surface as auth/version errors
/// (no USM security context is supplied) — classified as unsupported, not malformed.
fn classify_parse_error(err: snmp2::Error) -> TrapError {
    match err {
        snmp2::Error::UnsupportedVersion | snmp2::Error::AuthFailure(_) => {
            TrapError::UnsupportedVersion
        }
        other => TrapError::Malformed(other.to_string()),
    }
}

/// Render one varbind value as display text (clipped to [`MAX_VALUE_CHARS`]).
fn render_value(value: &Value) -> String {
    let rendered = match value {
        Value::Boolean(b) => b.to_string(),
        Value::Null => "null".to_owned(),
        Value::Integer(n) => n.to_string(),
        Value::OctetString(s) => String::from_utf8_lossy(s).into_owned(),
        Value::ObjectIdentifier(oid) => oid.to_string(),
        Value::IpAddress(ip) => format!("{}.{}.{}.{}", ip[0], ip[1], ip[2], ip[3]),
        Value::Counter32(n) => n.to_string(),
        Value::Unsigned32(n) => n.to_string(),
        Value::Timeticks(n) => n.to_string(),
        Value::Counter64(n) => n.to_string(),
        Value::Opaque(b) => {
            let hex: String = b
                .iter()
                .take(MAX_VALUE_CHARS / 2)
                .map(|x| format!("{x:02x}"))
                .collect();
            format!("0x{hex}")
        }
        Value::EndOfMibView => "endOfMibView".to_owned(),
        Value::NoSuchObject => "noSuchObject".to_owned(),
        Value::NoSuchInstance => "noSuchInstance".to_owned(),
        _ => "(unsupported)".to_owned(),
    };
    clip_chars(&rendered, MAX_VALUE_CHARS).0
}

#[cfg(test)]
mod tests {
    use super::*;
    use snmp2::{pdu, snmp, Oid, Version};

    /// Build a v2c trap datagram with the standard identity varbinds + extras.
    fn v2c_trap_bytes(trap_oid: &[u64], extras: Vec<(&Oid, Value)>) -> Vec<u8> {
        let uptime_oid = Oid::from(&[1, 3, 6, 1, 2, 1, 1, 3, 0]).unwrap();
        let trapoid_oid = Oid::from(&[1, 3, 6, 1, 6, 3, 1, 1, 4, 1, 0]).unwrap();
        let identity = Oid::from(trap_oid).unwrap();
        let mut varbinds: Vec<(&Oid, Value)> = vec![
            (&uptime_oid, Value::Timeticks(12345)),
            (&trapoid_oid, Value::ObjectIdentifier(identity.clone())),
        ];
        varbinds.extend(extras);

        let mut buf = pdu::Buf::default();
        // The trailing None is snmp2's v3 security param (its `v3` feature is always on
        // in this workspace).
        pdu::build(
            Version::V2C,
            b"public",
            snmp::MSG_TRAP,
            42,
            &varbinds,
            0,
            0,
            &mut buf,
            None,
        )
        .unwrap();
        buf[..].to_vec()
    }

    #[test]
    fn v2c_trap_parses_identity_and_varbinds() {
        let if_descr = Oid::from(&[1, 3, 6, 1, 2, 1, 2, 2, 1, 2, 4]).unwrap();
        // linkDown
        let bytes = v2c_trap_bytes(
            &[1, 3, 6, 1, 6, 3, 1, 1, 5, 3],
            vec![(&if_descr, Value::OctetString(b"ge-0/0/1"))],
        );
        let trap = parse_trap(&bytes).unwrap();
        assert_eq!(trap.version, 2);
        assert_eq!(trap.community, "public");
        assert_eq!(trap.trap_oid, "1.3.6.1.6.3.1.1.5.3");
        assert_eq!(trap.uptime_ticks, Some(12345));
        assert!(!trap.is_inform);
        assert!(trap
            .varbinds
            .iter()
            .any(|(o, v)| o == "1.3.6.1.2.1.2.2.1.2.4" && v == "ge-0/0/1"));
    }

    #[test]
    fn rendered_message_has_trap_oid_then_varbinds_without_identity_noise() {
        let if_descr = Oid::from(&[1, 3, 6, 1, 2, 1, 2, 2, 1, 2, 4]).unwrap();
        let bytes = v2c_trap_bytes(
            &[1, 3, 6, 1, 6, 3, 1, 1, 5, 3],
            vec![(&if_descr, Value::OctetString(b"ge-0/0/1"))],
        );
        let trap = parse_trap(&bytes).unwrap();
        assert_eq!(
            trap.render_message(),
            "1.3.6.1.6.3.1.1.5.3 1.3.6.1.2.1.2.2.1.2.4=ge-0/0/1;"
        );
    }

    #[test]
    fn inform_is_flagged_and_gets_a_response() {
        let uptime_oid = Oid::from(&[1, 3, 6, 1, 2, 1, 1, 3, 0]).unwrap();
        let mut buf = pdu::Buf::default();
        pdu::build(
            Version::V2C,
            b"public",
            snmp::MSG_INFORM,
            777,
            &[(&uptime_oid, Value::Timeticks(1))],
            0,
            0,
            &mut buf,
            None,
        )
        .unwrap();
        let bytes = buf[..].to_vec();

        let trap = parse_trap(&bytes).unwrap();
        assert!(trap.is_inform);

        let response = build_inform_response(&bytes).expect("response for inform");
        let parsed = Pdu::from_bytes(&response).unwrap();
        assert_eq!(parsed.message_type, MessageType::Response);
        assert_eq!(parsed.req_id, 777);
        assert_eq!(parsed.community, b"public");
    }

    #[test]
    fn plain_get_request_is_not_a_trap() {
        let oid = Oid::from(&[1, 3, 6, 1, 2, 1, 1, 1, 0]).unwrap();
        let mut buf = pdu::Buf::default();
        pdu::build(
            Version::V2C,
            b"public",
            snmp::MSG_GET,
            1,
            &[(&oid, Value::Null)],
            0,
            0,
            &mut buf,
            None,
        )
        .unwrap();
        assert!(matches!(parse_trap(&buf[..]), Err(TrapError::NotATrap)));
        assert!(build_inform_response(&buf[..]).is_none());
    }

    #[test]
    fn hostile_inputs_error_and_never_panic() {
        let cases: Vec<Vec<u8>> = vec![
            vec![],                       // empty
            vec![0x30],                   // bare sequence tag
            vec![0x30, 0x82, 0xFF, 0xFF], // huge declared length
            vec![0xFF; 64],               // garbage
            {
                // A valid trap, truncated mid-PDU.
                let full = v2c_trap_bytes(&[1, 3, 6, 1, 6, 3, 1, 1, 5, 3], vec![]);
                full[..full.len() / 2].to_vec()
            },
            vec![0x30, 0x03, 0x02, 0x01, 0x63], // version=99, nothing else
        ];
        for bytes in cases {
            let result = parse_trap(&bytes);
            assert!(
                result.is_err(),
                "hostile input must not parse: {bytes:0>2x?}"
            );
        }
    }

    #[test]
    fn oversized_octetstring_value_is_clipped() {
        let big = vec![b'a'; 4000];
        let oid = Oid::from(&[1, 3, 6, 1, 4, 1, 1]).unwrap();
        let bytes = v2c_trap_bytes(
            &[1, 3, 6, 1, 6, 3, 1, 1, 5, 3],
            vec![(&oid, Value::OctetString(&big))],
        );
        let trap = parse_trap(&bytes).unwrap();
        let (_, value) = trap
            .varbinds
            .iter()
            .find(|(o, _)| o == "1.3.6.1.4.1.1")
            .unwrap();
        assert_eq!(value.chars().count(), MAX_VALUE_CHARS);
    }
}
