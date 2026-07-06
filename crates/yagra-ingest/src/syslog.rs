//! Syslog datagram parsing: RFC 5424, then RFC 3164, then a raw fallback.
//!
//! Parsing is **infallible** — real-world syslog is wildly non-conformant (Cisco's
//! `%FACILITY-SEV-MNEMONIC` lines, missing timestamps, bare text), so anything that
//! doesn't match a stricter format degrades to `SyslogFormat::Raw` with the PRI (if any)
//! still decoded. The `message` field keeps as much matchable text as possible: for
//! RFC 3164 it includes the `TAG:` prefix (operators match on mnemonics like
//! `%LINK-3-UPDOWN`), for RFC 5424 it is the MSG part.

use crate::{clip_chars, MAX_EVENT_TEXT};

/// Which format the datagram parsed as.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyslogFormat {
    /// RFC 5424 (`<PRI>1 TIMESTAMP HOSTNAME APP-NAME PROCID MSGID SD [MSG]`).
    Rfc5424,
    /// Classic BSD syslog (`<PRI>Mmm dd hh:mm:ss HOSTNAME TAG: CONTENT`).
    Rfc3164,
    /// Anything else — PRI (if present) decoded, the rest kept verbatim.
    Raw,
}

/// A parsed syslog datagram, normalized for rule matching.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyslogEvent {
    /// Facility decoded from the PRI field (0–23), if present.
    pub facility: Option<u8>,
    /// Severity decoded from the PRI field (0–7), if present.
    pub severity: Option<u8>,
    /// HOSTNAME field, if the format carries one (NILVALUE `-` → `None`).
    pub hostname: Option<String>,
    /// APP-NAME (5424) or TAG (3164, `[pid]` suffix stripped), if present.
    pub app_name: Option<String>,
    /// The text rules match against (≤ [`MAX_EVENT_TEXT`] chars, lossy UTF-8).
    pub message: String,
    /// Which format matched.
    pub format: SyslogFormat,
    /// Whether `message` was clipped to the size cap.
    pub truncated: bool,
}

const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

/// Parse a syslog datagram. Never fails: unparseable input comes back as
/// [`SyslogFormat::Raw`] with the whole (PRI-stripped) text as the message.
#[must_use]
pub fn parse_syslog(bytes: &[u8]) -> SyslogEvent {
    let text = String::from_utf8_lossy(bytes);
    let text = text.trim_end_matches(['\r', '\n', '\0']);

    let (pri, rest) = split_pri(text);
    let (facility, severity) = match pri {
        Some(p) => (Some(p / 8), Some(p % 8)),
        None => (None, None),
    };

    let parsed = parse_rfc5424(rest).or_else(|| parse_rfc3164(rest));
    let (hostname, app_name, message, format) = match parsed {
        Some(p) => (p.hostname, p.app_name, p.message, p.format),
        None => (None, None, rest.to_owned(), SyslogFormat::Raw),
    };

    let (message, truncated) = clip_chars(&message, MAX_EVENT_TEXT);
    SyslogEvent {
        facility,
        severity,
        hostname,
        app_name,
        message,
        format,
        truncated,
    }
}

/// Intermediate result of a structured (5424/3164) parse attempt.
struct Structured {
    hostname: Option<String>,
    app_name: Option<String>,
    message: String,
    format: SyslogFormat,
}

/// Split a leading `<PRI>` off `text`. PRI is 1–3 digits, value ≤ 191 (RFC 5424 §6.2.1).
fn split_pri(text: &str) -> (Option<u8>, &str) {
    let Some(inner) = text.strip_prefix('<') else {
        return (None, text);
    };
    let Some(close) = inner.find('>') else {
        return (None, text);
    };
    let digits = &inner[..close];
    if digits.is_empty() || digits.len() > 3 || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return (None, text);
    }
    match digits.parse::<u16>() {
        Ok(pri) if pri <= 191 => (Some(pri as u8), &inner[close + 1..]),
        _ => (None, text),
    }
}

/// A field that may be the NILVALUE `-`.
fn nil_field(field: &str) -> Option<String> {
    if field == "-" || field.is_empty() {
        None
    } else {
        Some(field.to_owned())
    }
}

/// RFC 5424: `1 TIMESTAMP HOSTNAME APP-NAME PROCID MSGID SD [MSG]` (after the PRI).
fn parse_rfc5424(rest: &str) -> Option<Structured> {
    let rest = rest.strip_prefix("1 ")?;
    let mut parts = rest.splitn(6, ' ');
    let _timestamp = parts.next()?;
    let hostname = parts.next()?;
    let app_name = parts.next()?;
    let _procid = parts.next()?;
    let _msgid = parts.next()?;
    let sd_and_msg = parts.next()?;

    let msg = skip_structured_data(sd_and_msg)?;
    // RFC 5424 §6.4: the MSG part may begin with a UTF-8 BOM.
    let msg = msg.strip_prefix('\u{FEFF}').unwrap_or(msg);

    Some(Structured {
        hostname: nil_field(hostname),
        app_name: nil_field(app_name),
        message: msg.to_owned(),
        format: SyslogFormat::Rfc5424,
    })
}

/// Skip the STRUCTURED-DATA part (`-` or one or more `[..]` elements, with `\]` escapes
/// inside quoted param values) and return the MSG that follows (empty if none).
fn skip_structured_data(sd_and_msg: &str) -> Option<&str> {
    if sd_and_msg == "-" {
        return Some("");
    }
    if let Some(msg) = sd_and_msg.strip_prefix("- ") {
        return Some(msg);
    }
    if !sd_and_msg.starts_with('[') {
        return None;
    }

    let mut in_quotes = false;
    let mut escaped = false;
    let mut depth = 0usize;
    let mut iter = sd_and_msg.char_indices().peekable();
    while let Some((i, c)) = iter.next() {
        if escaped {
            escaped = false;
            continue;
        }
        match c {
            '\\' if in_quotes => escaped = true,
            '"' => in_quotes = !in_quotes,
            '[' if !in_quotes => depth += 1,
            ']' if !in_quotes => {
                depth = depth.checked_sub(1)?;
                if depth == 0 {
                    match iter.peek() {
                        // Another SD-ELEMENT follows back-to-back.
                        Some((_, '[')) => {}
                        // ` MSG` follows.
                        Some((j, ' ')) => return Some(&sd_and_msg[j + 1..]),
                        // SD was the last thing in the datagram.
                        None => return Some(""),
                        Some(_) => return None,
                    }
                }
            }
            _ => {}
        }
        let _ = i;
    }
    None
}

/// RFC 3164: `Mmm dd hh:mm:ss HOSTNAME TAG: CONTENT` (after the PRI). The returned
/// message keeps the `TAG:` prefix so rules can match on it.
fn parse_rfc3164(rest: &str) -> Option<Structured> {
    let bytes = rest.as_bytes();
    // Timestamp is exactly 15 chars: "Mmm dd hh:mm:ss" (day space-padded), then a space.
    if bytes.len() < 17 || !MONTHS.contains(&rest.get(0..3)?) {
        return None;
    }
    let ts_ok = bytes[3] == b' '
        && (bytes[4] == b' ' || bytes[4].is_ascii_digit())
        && bytes[5].is_ascii_digit()
        && bytes[6] == b' '
        && bytes[7].is_ascii_digit()
        && bytes[8].is_ascii_digit()
        && bytes[9] == b':'
        && bytes[10].is_ascii_digit()
        && bytes[11].is_ascii_digit()
        && bytes[12] == b':'
        && bytes[13].is_ascii_digit()
        && bytes[14].is_ascii_digit()
        && bytes[15] == b' ';
    if !ts_ok {
        return None;
    }

    let after_ts = &rest[16..];
    let (hostname, content) = after_ts.split_once(' ')?;
    if hostname.is_empty() || content.is_empty() {
        return None;
    }

    Some(Structured {
        hostname: Some(hostname.to_owned()),
        app_name: extract_tag(content),
        message: content.to_owned(),
        format: SyslogFormat::Rfc3164,
    })
}

/// Pull the RFC 3164 TAG (`app[pid]:` / `app:`) off the front of the content, if the
/// first `:` comes within the conventional 32-char tag budget.
fn extract_tag(content: &str) -> Option<String> {
    let colon = content.find(':')?;
    if colon == 0 || colon > 32 {
        return None;
    }
    let tag = &content[..colon];
    // Strip a trailing `[pid]`.
    let tag = match tag.split_once('[') {
        Some((name, _)) => name,
        None => tag,
    };
    let ok = !tag.is_empty()
        && tag
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-' || b == b'.' || b == b'%');
    ok.then(|| tag.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfc5424_with_structured_data_and_msg() {
        let e = parse_syslog(
            b"<165>1 2026-07-06T22:14:15.003Z edge-sw1 chassisd 1234 ID47 [exampleSDID@32473 iut=\"3\" eventSource=\"App\"] link down on ge-0/0/1",
        );
        assert_eq!(e.format, SyslogFormat::Rfc5424);
        assert_eq!(e.facility, Some(20));
        assert_eq!(e.severity, Some(5));
        assert_eq!(e.hostname.as_deref(), Some("edge-sw1"));
        assert_eq!(e.app_name.as_deref(), Some("chassisd"));
        assert_eq!(e.message, "link down on ge-0/0/1");
        assert!(!e.truncated);
    }

    #[test]
    fn rfc5424_nilvalue_fields_and_no_sd() {
        let e = parse_syslog(b"<34>1 2026-07-06T22:14:15Z - - - - - disk full");
        assert_eq!(e.format, SyslogFormat::Rfc5424);
        assert_eq!(e.hostname, None);
        assert_eq!(e.app_name, None);
        assert_eq!(e.message, "disk full");
    }

    #[test]
    fn rfc5424_sd_only_no_msg() {
        let e = parse_syslog(b"<34>1 2026-07-06T22:14:15Z host app - - [x@1 k=\"v\"]");
        assert_eq!(e.format, SyslogFormat::Rfc5424);
        assert_eq!(e.message, "");
    }

    #[test]
    fn rfc5424_sd_with_escaped_bracket_in_value() {
        let e = parse_syslog(b"<34>1 2026-07-06T22:14:15Z host app - - [x@1 k=\"a\\]b\"] real msg");
        assert_eq!(e.format, SyslogFormat::Rfc5424);
        assert_eq!(e.message, "real msg");
    }

    #[test]
    fn rfc5424_msg_bom_is_stripped() {
        let mut datagram = b"<34>1 2026-07-06T22:14:15Z host app - - - ".to_vec();
        datagram.extend_from_slice(&[0xEF, 0xBB, 0xBF]);
        datagram.extend_from_slice(b"bom msg");
        let e = parse_syslog(&datagram);
        assert_eq!(e.message, "bom msg");
    }

    #[test]
    fn rfc3164_with_tag_and_pid() {
        let e = parse_syslog(b"<13>Jul  6 22:14:15 edge-sw1 sshd[4242]: Failed password for admin");
        assert_eq!(e.format, SyslogFormat::Rfc3164);
        assert_eq!(e.facility, Some(1));
        assert_eq!(e.severity, Some(5));
        assert_eq!(e.hostname.as_deref(), Some("edge-sw1"));
        assert_eq!(e.app_name.as_deref(), Some("sshd"));
        // TAG prefix is kept in the matchable message.
        assert_eq!(e.message, "sshd[4242]: Failed password for admin");
    }

    #[test]
    fn rfc3164_two_digit_day() {
        let e = parse_syslog(b"<13>Jul 16 02:04:05 host kernel: oops");
        assert_eq!(e.format, SyslogFormat::Rfc3164);
        assert_eq!(e.app_name.as_deref(), Some("kernel"));
    }

    #[test]
    fn cisco_style_falls_back_to_raw_with_pri() {
        // Cisco's sequence-number prefix doesn't match 3164's timestamp — the fallback
        // must still decode PRI and keep the whole line matchable.
        let e = parse_syslog(b"<189>106: Jan  1 00:00:00: %LINEPROTO-5-UPDOWN: Line protocol on Interface Gi0/1, changed state to down");
        assert_eq!(e.format, SyslogFormat::Raw);
        assert_eq!(e.facility, Some(23));
        assert_eq!(e.severity, Some(5));
        assert!(e.message.contains("%LINEPROTO-5-UPDOWN"));
        assert_eq!(e.hostname, None);
    }

    #[test]
    fn bare_text_is_raw_without_pri() {
        let e = parse_syslog(b"hello world");
        assert_eq!(e.format, SyslogFormat::Raw);
        assert_eq!(e.facility, None);
        assert_eq!(e.severity, None);
        assert_eq!(e.message, "hello world");
    }

    #[test]
    fn invalid_pri_values_are_not_decoded() {
        // >191, empty, non-numeric, too many digits.
        for input in [
            "<192>oops".as_bytes(),
            "<>oops".as_bytes(),
            "<abc>oops".as_bytes(),
            "<1234>oops".as_bytes(),
        ] {
            let e = parse_syslog(input);
            assert_eq!(
                e.facility,
                None,
                "input {:?}",
                String::from_utf8_lossy(input)
            );
            // The unparsed PRI stays part of the raw message.
            assert!(e.message.starts_with('<'));
        }
    }

    #[test]
    fn non_utf8_bytes_are_lossy_decoded_not_fatal() {
        let e = parse_syslog(&[0x3C, 0x31, 0x33, 0x3E, 0xFF, 0xFE, 0x80, b'x']); // "<13>" + garbage
        assert_eq!(e.facility, Some(1));
        assert_eq!(e.format, SyslogFormat::Raw);
        assert!(e.message.contains('\u{FFFD}'));
    }

    #[test]
    fn oversized_message_is_clipped_and_flagged() {
        let mut datagram = b"<13>".to_vec();
        datagram.extend(std::iter::repeat_n(b'a', MAX_EVENT_TEXT + 100));
        let e = parse_syslog(&datagram);
        assert_eq!(e.message.chars().count(), MAX_EVENT_TEXT);
        assert!(e.truncated);
    }

    #[test]
    fn trailing_newlines_are_trimmed() {
        let e = parse_syslog(b"<13>Jul  6 22:14:15 h app: msg\r\n");
        assert_eq!(e.message, "app: msg");
    }

    #[test]
    fn empty_datagram_is_raw_empty() {
        let e = parse_syslog(b"");
        assert_eq!(e.format, SyslogFormat::Raw);
        assert_eq!(e.message, "");
        assert!(!e.truncated);
    }

    #[test]
    fn rfc5424_truncated_header_falls_back_to_raw() {
        // Version tag but not enough header fields — must not panic, must not misparse.
        let e = parse_syslog(b"<34>1 2026-07-06T22:14:15Z host");
        assert_eq!(e.format, SyslogFormat::Raw);
        assert_eq!(e.message, "1 2026-07-06T22:14:15Z host");
    }

    #[test]
    fn rfc5424_unterminated_sd_falls_back_to_raw() {
        let e = parse_syslog(b"<34>1 2026-07-06T22:14:15Z host app - - [x@1 k=\"v\" msg");
        assert_eq!(e.format, SyslogFormat::Raw);
    }
}
