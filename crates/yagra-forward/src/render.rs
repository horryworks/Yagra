// SPDX-License-Identifier: AGPL-3.0-only
//! Wire renderers for forwarded events (ADR-034).
//!
//! These build a datagram from an [`EventMsg`]'s **parsed** fields. They are the fallback for when
//! the original bytes are unavailable — an inbound webhook was never a datagram, and an N-1 poller
//! predates [`EventMsg::raw`] — and the deliberate choice for a collector that wants normalized
//! RFC 5424 instead of whatever the device emitted. When the raw payload *is* present and the
//! destination pairing supports it, core forwards those bytes and never calls these functions.
//!
//! **What rendering loses**, and why verbatim is the default:
//!
//! * syslog — RFC 5424 STRUCTURED-DATA, PROCID and MSGID are not carried on the bus and become
//!   NILVALUE; the reception timestamp replaces the sender's; a body that was not valid UTF-8 was
//!   already replaced with U+FFFD at parse time; a body over 4096 characters was already clipped.
//! * traps — varbind **types** are gone (the bus carries rendered text), so every non-identity
//!   varbind is re-encoded as an OctetString; only the first 32 varbinds survived reception, each
//!   clipped to 256 characters.
//!
//! None of that is recoverable here — it was discarded before the message reached core. The
//! renderers are honest about it rather than pretending to reproduce the original.

use chrono::{TimeZone, Utc};
use snmp2::{pdu, snmp, Oid, Value, Version};
use yagra_bus::{EventKind, EventMsg};

/// Community used for a re-encoded trap when the destination configures none.
pub const DEFAULT_TRAP_COMMUNITY: &str = "public";

/// Facility used when the event carries none (traps, webhooks, syslog with an unparseable PRI).
/// `local7` is the conventional catch-all for relayed device output.
const DEFAULT_FACILITY: u8 = 23;
/// Severity for a syslog event whose PRI did not decode: `informational`.
const DEFAULT_SYSLOG_SEVERITY: u8 = 6;
/// Severity used when rendering a non-syslog event as syslog: `notice`.
const NON_SYSLOG_SEVERITY: u8 = 5;

/// RFC 5424 §6 length limits for the header fields we populate.
const MAX_HOSTNAME: usize = 255;
const MAX_APP_NAME: usize = 48;

/// sysUpTime.0 — first varbind of a v2c trap.
const SYS_UPTIME_0: &str = "1.3.6.1.2.1.1.3.0";
/// snmpTrapOID.0 — identifies the trap in v2c.
const SNMP_TRAP_OID_0: &str = "1.3.6.1.6.3.1.1.4.1.0";

/// Render `ev` as an RFC 5424 syslog message.
///
/// ```text
/// <PRI>1 TIMESTAMP HOSTNAME APP-NAME PROCID MSGID STRUCTURED-DATA MSG
/// ```
///
/// Header fields are sanitized to printable ASCII without spaces and clipped to their RFC lengths,
/// so a hostile device string can never break framing on the receiving collector. A non-ASCII body
/// is prefixed with a BOM, which is how RFC 5424 §6.4 marks MSG as UTF-8.
#[must_use]
pub fn render_syslog_5424(ev: &EventMsg) -> Vec<u8> {
    let facility = ev.facility.filter(|f| *f <= 23).unwrap_or(DEFAULT_FACILITY);
    let severity = ev.syslog_severity.filter(|s| *s <= 7).unwrap_or({
        if ev.kind == EventKind::Syslog {
            DEFAULT_SYSLOG_SEVERITY
        } else {
            NON_SYSLOG_SEVERITY
        }
    });
    let pri = u16::from(facility) * 8 + u16::from(severity);

    let timestamp = Utc
        .timestamp_millis_opt(ev.at_unix_ms)
        .single()
        .map_or_else(
            || "-".to_owned(),
            |t| t.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string(),
        );

    // Falling back to the source address keeps the origin identifiable when the device sent no
    // HOSTNAME — otherwise every relayed line would look like it came from nowhere.
    let hostname = header_field(
        ev.hostname
            .as_deref()
            .map(str::to_owned)
            .or_else(|| ev.source_ip.map(|ip| ip.to_string()))
            .as_deref(),
        MAX_HOSTNAME,
    );
    let app_name = header_field(
        ev.app_name.as_deref().or(match ev.kind {
            EventKind::Trap => Some("snmp-trap"),
            EventKind::Webhook => Some("webhook"),
            EventKind::Syslog => None,
        }),
        MAX_APP_NAME,
    );

    let mut out = format!("<{pri}>1 {timestamp} {hostname} {app_name} - - - ").into_bytes();
    if !ev.message.is_ascii() {
        out.extend_from_slice("\u{feff}".as_bytes());
    }
    out.extend_from_slice(ev.message.as_bytes());
    out
}

/// Render `ev` as an SNMPv2c trap PDU, or `None` when it carries no usable trap identity OID.
///
/// The identity varbinds are rebuilt with their proper types (`sysUpTime.0` as Timeticks,
/// `snmpTrapOID.0` as an OID); **every other varbind becomes an OctetString**, because the bus
/// carries only each value's rendered text. A collector that switches on varbind type will see
/// strings where the device sent integers — which is precisely why byte-exact forwarding is the
/// default and this path is the fallback.
#[must_use]
pub fn render_trap_v2c(ev: &EventMsg, community: &str) -> Option<Vec<u8>> {
    let identity = parse_oid(ev.trap_oid.as_deref()?)?;

    // sysUpTime.0 is echoed from the received varbind when it was numeric; a trap that arrived
    // without one (or with an unparseable one) is sent as 0 rather than dropped.
    let uptime = ev
        .varbinds
        .iter()
        .find(|(oid, _)| oid == SYS_UPTIME_0)
        .and_then(|(_, value)| value.parse::<u32>().ok())
        .unwrap_or(0);

    let uptime_oid = parse_oid(SYS_UPTIME_0)?;
    let trapoid_oid = parse_oid(SNMP_TRAP_OID_0)?;

    // The `Value::ObjectIdentifier`/`OctetString` borrows must outlive the `build` call, so the
    // owned OIDs and byte strings are materialized before the varbind slice is assembled.
    let extras: Vec<(Oid<'static>, &[u8])> = ev
        .varbinds
        .iter()
        .filter(|(oid, _)| oid != SYS_UPTIME_0 && oid != SNMP_TRAP_OID_0)
        .filter_map(|(oid, value)| Some((parse_oid(oid)?, value.as_bytes())))
        .collect();

    let mut varbinds: Vec<(&Oid, Value)> = Vec::with_capacity(extras.len() + 2);
    varbinds.push((&uptime_oid, Value::Timeticks(uptime)));
    varbinds.push((&trapoid_oid, Value::ObjectIdentifier(identity)));
    for (oid, value) in &extras {
        varbinds.push((oid, Value::OctetString(value)));
    }

    let mut buf = pdu::Buf::default();
    // The request-id is not meaningful for an unacknowledged trap; deriving it from the reception
    // time keeps it varying without needing a counter. The trailing `None` is snmp2's v3 security
    // parameter (its `v3` feature is always on in this workspace).
    let req_id = i32::try_from(ev.at_unix_ms & 0x7fff_ffff).unwrap_or(0);
    pdu::build(
        Version::V2C,
        community.as_bytes(),
        snmp::MSG_TRAP,
        req_id,
        &varbinds,
        0,
        0,
        &mut buf,
        None,
    )
    .ok()?;
    Some(buf[..].to_vec())
}

/// Parse a dotted OID into `snmp2`'s owned form. Returns `None` on anything that is not a
/// dot-separated list of unsigned integers — device-supplied text reaches here, so it must not
/// panic.
fn parse_oid(dotted: &str) -> Option<Oid<'static>> {
    let arcs = dotted
        .split('.')
        .map(|arc| arc.parse::<u64>().ok())
        .collect::<Option<Vec<_>>>()?;
    if arcs.len() < 2 {
        return None;
    }
    // `Oid::from` encodes into an owned buffer, so the result is already `'static`.
    Oid::from(&arcs).ok()
}

/// Coerce one RFC 5424 header field: NILVALUE when absent/empty, otherwise printable ASCII with
/// every disallowed byte (including spaces) replaced by `-`, clipped to `max`.
fn header_field(value: Option<&str>, max: usize) -> String {
    let Some(value) = value.map(str::trim).filter(|v| !v.is_empty()) else {
        return "-".to_owned();
    };
    let cleaned: String = value
        .chars()
        .take(max)
        .map(|c| if c.is_ascii_graphic() { c } else { '-' })
        .collect();
    if cleaned.is_empty() {
        "-".to_owned()
    } else {
        cleaned
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;
    use yagra_bus::BUS_SCHEMA_VERSION;
    use yagra_ingest::{parse_syslog, parse_trap, SyslogFormat};

    fn event(kind: EventKind) -> EventMsg {
        EventMsg {
            schema_version: BUS_SCHEMA_VERSION,
            event_id: Uuid::nil(),
            kind,
            at_unix_ms: 1_700_000_000_123,
            source_ip: Some("192.168.1.10".parse().unwrap()),
            pool: Some("tokyo".into()),
            message: "Line protocol on ge-0/0/1 changed state to down".into(),
            facility: (kind == EventKind::Syslog).then_some(23),
            syslog_severity: (kind == EventKind::Syslog).then_some(4),
            hostname: (kind == EventKind::Syslog).then(|| "edge-sw1".to_owned()),
            app_name: (kind == EventKind::Syslog).then(|| "chassisd".to_owned()),
            trap_oid: (kind == EventKind::Trap).then(|| "1.3.6.1.6.3.1.1.5.3".to_owned()),
            varbinds: if kind == EventKind::Trap {
                vec![
                    (SYS_UPTIME_0.to_owned(), "12345".to_owned()),
                    ("1.3.6.1.2.1.2.2.1.2.4".to_owned(), "ge-0/0/1".to_owned()),
                ]
            } else {
                Vec::new()
            },
            truncated: false,
            raw: None,
            src_port: Some(51_000),
        }
    }

    #[test]
    fn rendered_syslog_round_trips_through_the_receive_parser() {
        let ev = event(EventKind::Syslog);
        let wire = render_syslog_5424(&ev);
        let text = String::from_utf8(wire.clone()).unwrap();
        assert!(
            text.starts_with("<188>1 2023-11-14T22:13:20.123Z edge-sw1 chassisd - - - "),
            "{text}"
        );

        // Feeding our own output back through the receiver must reproduce the same fields — that
        // is the contract a downstream collector relies on.
        let back = parse_syslog(&wire);
        assert_eq!(back.format, SyslogFormat::Rfc5424);
        assert_eq!(back.facility, ev.facility);
        assert_eq!(back.severity, ev.syslog_severity);
        assert_eq!(back.hostname, ev.hostname);
        assert_eq!(back.app_name, ev.app_name);
        assert_eq!(back.message, ev.message);
    }

    #[test]
    fn trap_rendered_as_syslog_gets_a_marker_app_name_and_notice_severity() {
        let ev = event(EventKind::Trap);
        let wire = render_syslog_5424(&ev);
        let back = parse_syslog(&wire);
        assert_eq!(back.app_name.as_deref(), Some("snmp-trap"));
        assert_eq!(back.facility, Some(DEFAULT_FACILITY));
        assert_eq!(back.severity, Some(NON_SYSLOG_SEVERITY));
        // No HOSTNAME on a trap — the source address stands in so the origin stays identifiable.
        assert_eq!(back.hostname.as_deref(), Some("192.168.1.10"));
    }

    #[test]
    fn header_fields_are_sanitized_so_a_hostile_device_string_cannot_break_framing() {
        let mut ev = event(EventKind::Syslog);
        ev.hostname = Some("evil host\nwith newline".into());
        ev.app_name = Some("  ".into());
        let wire = render_syslog_5424(&ev);
        let text = String::from_utf8(wire.clone()).unwrap();
        assert!(!text.contains('\n'), "{text}");
        // The injected space/newline become dashes, so the header keeps its field count and the
        // message body is still parsed as the body.
        let back = parse_syslog(&wire);
        assert_eq!(back.hostname.as_deref(), Some("evil-host-with-newline"));
        assert_eq!(back.app_name, None); // blank → NILVALUE
        assert_eq!(back.message, ev.message);
    }

    #[test]
    fn non_ascii_body_is_marked_with_a_bom_and_survives_the_round_trip() {
        let mut ev = event(EventKind::Syslog);
        ev.message = "リンクがダウンしました ge-0/0/1".into();
        let wire = render_syslog_5424(&ev);
        let back = parse_syslog(&wire);
        assert_eq!(back.message, ev.message, "BOM must be stripped on receive");
    }

    #[test]
    fn missing_or_out_of_range_pri_parts_fall_back_to_defaults() {
        let mut ev = event(EventKind::Syslog);
        ev.facility = Some(99); // not a real facility
        ev.syslog_severity = None;
        let back = parse_syslog(&render_syslog_5424(&ev));
        assert_eq!(back.facility, Some(DEFAULT_FACILITY));
        assert_eq!(back.severity, Some(DEFAULT_SYSLOG_SEVERITY));
    }

    #[test]
    fn rendered_trap_round_trips_through_the_receive_parser() {
        let ev = event(EventKind::Trap);
        let wire = render_trap_v2c(&ev, "private").unwrap();
        let back = parse_trap(&wire).unwrap();
        assert_eq!(back.version, 2);
        assert_eq!(back.community, "private");
        assert_eq!(back.trap_oid, "1.3.6.1.6.3.1.1.5.3");
        assert_eq!(back.uptime_ticks, Some(12345));
        assert!(!back.is_inform);
        assert!(
            back.varbinds
                .iter()
                .any(|(oid, value)| oid == "1.3.6.1.2.1.2.2.1.2.4" && value == "ge-0/0/1"),
            "{:?}",
            back.varbinds
        );
    }

    #[test]
    fn trap_without_a_usable_identity_oid_is_not_rendered() {
        let mut ev = event(EventKind::Trap);
        ev.trap_oid = None;
        assert!(render_trap_v2c(&ev, DEFAULT_TRAP_COMMUNITY).is_none());
        // Device-supplied text reaches parse_oid; it must reject rather than panic.
        ev.trap_oid = Some("not.an.oid".into());
        assert!(render_trap_v2c(&ev, DEFAULT_TRAP_COMMUNITY).is_none());
        ev.trap_oid = Some("1".into()); // a single arc is not a legal OID
        assert!(render_trap_v2c(&ev, DEFAULT_TRAP_COMMUNITY).is_none());
    }

    #[test]
    fn unparseable_varbind_oids_are_skipped_rather_than_failing_the_whole_trap() {
        let mut ev = event(EventKind::Trap);
        ev.varbinds.push(("garbage-oid".to_owned(), "x".to_owned()));
        let back = parse_trap(&render_trap_v2c(&ev, DEFAULT_TRAP_COMMUNITY).unwrap()).unwrap();
        assert!(back.varbinds.iter().all(|(oid, _)| oid != "garbage-oid"));
        assert!(back
            .varbinds
            .iter()
            .any(|(oid, _)| oid == "1.3.6.1.2.1.2.2.1.2.4"));
    }

    #[test]
    fn trap_with_no_uptime_varbind_renders_zero_instead_of_dropping() {
        let mut ev = event(EventKind::Trap);
        ev.varbinds.retain(|(oid, _)| oid != SYS_UPTIME_0);
        let back = parse_trap(&render_trap_v2c(&ev, DEFAULT_TRAP_COMMUNITY).unwrap()).unwrap();
        assert_eq!(back.uptime_ticks, Some(0));
    }
}
