// SPDX-License-Identifier: AGPL-3.0-only
//! Vendor event-code extraction from a syslog message body.
//!
//! Passive-event analytics cluster unmatched events by a *signature* — a stable key naming the
//! event class, so "5,000 of these arrived and no rule matched" is a sentence one can write. Traps
//! have one (the trap OID) and well-formed RFC 5424 senders have one (APP-NAME), but a large share
//! of real network gear has neither: its timestamp format falls outside both RFC 3164 and RFC 5424,
//! so [`crate::parse_syslog`] degrades to [`crate::SyslogFormat::Raw`] and extracts no fields at
//! all. Those devices still name the event class — inside the message text, in their own encoding:
//!
//! ```text
//! Aug  7 2026 15:43:42 fw01 %%01URL/4/FILTER(l):CID=0x814f0420;The URL filtering policy ...
//! 106: Jan  1 00:00:00: %LINEPROTO-5-UPDOWN: Line protocol on Interface Gi0/1, changed ...
//! ```
//!
//! # The invariant
//!
//! **A signature is a verbatim slice of the message it came from** — no case folding, no
//! synthesis, no reordering. The borrow in [`Signature`] makes that structural, and
//! `every_signature_is_a_verbatim_slice_of_its_message` keeps it so. Two things depend on it:
//! a signature named in a rule-gap finding can be pasted into the event search and will find the
//! events, and it can be pasted into a `substring` event rule and will match them. It also bounds
//! what a future pattern is allowed to invent.
//!
//! # Why the bounds are not decoration
//!
//! Input here is an attacker-controlled datagram, and the output becomes a stored column *and* a
//! VictoriaLogs field — i.e. a cardinality surface. A pattern that accidentally captures a session
//! id or a port number turns one event class into millions. Hence the scan window, the length cap,
//! and [`SignaturePattern::UpperSnakeTag`]'s digit-run rejection.

/// Longest signature accepted. Real vendor codes run well under this; anything longer is a sign the
/// pattern captured something variable, which is a cardinality bug rather than a long name.
pub const SIGNATURE_MAX_CHARS: usize = 64;

/// How far into the message to look. Every supported encoding puts the code in the header, so
/// scanning further only buys the chance of matching something in a free-text body.
pub const SIGNATURE_SCAN_CHARS: usize = 256;

/// Which encoding a signature was recognised as. Also the bounded label set for the
/// `yagra_event_signature_total` counter — keep it small.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignaturePattern {
    /// Huawei/H3C VRP: `%%<version><MODULE>/<severity>/<BRIEF>(<flag>):`.
    HuaweiBrief,
    /// Cisco IOS/NX-OS/ASA: `%<FACILITY>-<severity>-<MNEMONIC>:`.
    CiscoMnemonic,
    /// A leading `ALL_CAPS_TOKEN:` (Juniper `SNMP_TRAP_LINK_DOWN:` and many appliances).
    UpperSnakeTag,
}

impl SignaturePattern {
    /// Stable token for metrics and tests.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::HuaweiBrief => "huawei_brief",
            Self::CiscoMnemonic => "cisco_mnemonic",
            Self::UpperSnakeTag => "upper_snake",
        }
    }
}

/// A vendor event code found in a message, borrowed from it (see the module's invariant).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Signature<'a> {
    /// The code, exactly as it appears in the message.
    pub text: &'a str,
    /// Which pattern recognised it.
    pub pattern: SignaturePattern,
}

/// Lift a vendor event code out of a syslog message body, if one is recognisable.
///
/// Patterns are tried most-specific first; `HuaweiBrief` precedes `CiscoMnemonic` so a `%%` prefix
/// is never read as Cisco's single `%`.
#[must_use]
pub fn extract_signature(message: &str) -> Option<Signature<'_>> {
    let head = head(message);
    let sig = huawei_brief(head)
        .or_else(|| cisco_mnemonic(head))
        .or_else(|| upper_snake_tag(head))?;
    // The cap is applied once, here, so no pattern can forget it. `text` is ASCII by construction
    // (code bytes plus ASCII separators), so byte length is character length.
    (sig.text.len() <= SIGNATURE_MAX_CHARS).then_some(sig)
}

/// The leading [`SIGNATURE_SCAN_CHARS`] characters, on a character boundary.
fn head(message: &str) -> &str {
    match message.char_indices().nth(SIGNATURE_SCAN_CHARS) {
        Some((byte_idx, _)) => &message[..byte_idx],
        None => message,
    }
}

/// A byte that may appear inside a code word. Uppercase-only is the discriminator that keeps these
/// patterns off prose — lowercase would make `%is-4-not:` a signature.
fn is_code_byte(b: u8) -> bool {
    b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_'
}

/// End index of the `[A-Z0-9_]+` run starting at `from`, or `None` if it is empty.
fn run(b: &[u8], from: usize) -> Option<usize> {
    let mut i = from;
    while i < b.len() && is_code_byte(b[i]) {
        i += 1;
    }
    (i > from).then_some(i)
}

/// `<CODE><sep><one digit><sep><CODE>` starting at `from`. Returns the slice and the index just
/// past it, so the caller can check its own trailer.
fn triple(head: &str, from: usize, sep: u8) -> Option<(&str, usize)> {
    let b = head.as_bytes();
    let first_end = run(b, from)?;
    if b.get(first_end) != Some(&sep) {
        return None;
    }
    // Both supported vendors write exactly one severity digit; accepting more would let a counter
    // or an id slip into the key.
    let sev = first_end + 1;
    if !b.get(sev)?.is_ascii_digit() {
        return None;
    }
    if b.get(sev + 1) != Some(&sep) {
        return None;
    }
    let end = run(b, sev + 2)?;
    Some((head.get(from..end)?, end))
}

/// `%%<version digits><MODULE>/<severity>/<BRIEF>` followed by `(` or `:`.
fn huawei_brief(head: &str) -> Option<Signature<'_>> {
    let b = head.as_bytes();
    let mut from = 0usize;
    while let Some(rel) = head.get(from..)?.find("%%") {
        // Past the `%%`; both bytes are ASCII, so this stays a character boundary.
        from += rel + 2;
        let mut at = from;
        while at < b.len() && b[at].is_ascii_digit() {
            at += 1;
        }
        if let Some((text, end)) = triple(head, at, b'/') {
            if matches!(b.get(end), Some(b'(' | b':')) {
                return Some(Signature {
                    text,
                    pattern: SignaturePattern::HuaweiBrief,
                });
            }
        }
    }
    None
}

/// `%<FACILITY>-<severity>-<MNEMONIC>:`.
fn cisco_mnemonic(head: &str) -> Option<Signature<'_>> {
    let b = head.as_bytes();
    let mut from = 0usize;
    while let Some(rel) = head.get(from..)?.find('%') {
        from += rel + 1;
        // A doubled `%` is Huawei's marker, which was already tried and declined.
        if b.get(from) == Some(&b'%') {
            continue;
        }
        if let Some((text, end)) = triple(head, from, b'-') {
            if b.get(end) == Some(&b':') {
                return Some(Signature {
                    text,
                    pattern: SignaturePattern::CiscoMnemonic,
                });
            }
        }
    }
    None
}

/// A leading `ALL_CAPS_TOKEN:`.
///
/// Anchored at offset 0 on purpose: an all-caps word *inside* a message is prose or a value, not an
/// event code. The digit-run rejection is what stops `SESSION_12345:` from making every session its
/// own signature.
fn upper_snake_tag(head: &str) -> Option<Signature<'_>> {
    let b = head.as_bytes();
    if !b.first()?.is_ascii_uppercase() {
        return None;
    }
    let end = run(b, 0)?;
    if b.get(end) != Some(&b':') {
        return None;
    }
    let text = head.get(..end)?;
    (text.len() >= 3 && !has_long_digit_run(text)).then_some(Signature {
        text,
        pattern: SignaturePattern::UpperSnakeTag,
    })
}

/// Whether `text` contains four or more consecutive digits — the tell of an embedded id.
fn has_long_digit_run(text: &str) -> bool {
    let mut streak = 0usize;
    for b in text.bytes() {
        streak = if b.is_ascii_digit() { streak + 1 } else { 0 };
        if streak >= 4 {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One table drives every case below, so a new pattern is a new row rather than a new test.
    ///
    /// The two Huawei lines are **captured from the live test-server device** (`jpmyj01fw01`, a
    /// Huawei USG, 2026-08-07) rather than written from the format documentation — they are the
    /// reason this module exists. The rest are spec/vendor-documentation shapes; the negatives are
    /// the inputs a signature must *not* be invented for.
    const FIXTURES: &[(&str, Option<(&str, SignaturePattern)>)] = &[
        // ── Huawei USG, real capture ──
        (
            "Aug  7 2026 15:43:42 jpmyj01fw01 %%01URL/4/FILTER(l):CID=0x814f0420;The URL filtering \
             policy was matched. (SyslogId=1598559, VSys=\"public\", SrcIp=192.168.1.142)",
            Some(("URL/4/FILTER", SignaturePattern::HuaweiBrief)),
        ),
        (
            "Aug  7 2026 15:43:41 jpmyj01fw01 %%01POLICY/6/POLICYPERMIT(l):CID=0x814f041e;\
             vsys=public, protocol=6, source-ip=192.168.1.142, source-port=52433",
            Some(("POLICY/6/POLICYPERMIT", SignaturePattern::HuaweiBrief)),
        ),
        // ── Cisco IOS (the shape `syslog.rs::cisco_style_falls_back_to_raw_with_pri` already uses) ──
        (
            "106: Jan  1 00:00:00: %LINEPROTO-5-UPDOWN: Line protocol on Interface Gi0/1, changed \
             state to down",
            Some(("LINEPROTO-5-UPDOWN", SignaturePattern::CiscoMnemonic)),
        ),
        // ── Leading all-caps tag (Juniper-style) ──
        (
            "SNMP_TRAP_LINK_DOWN: ifIndex 528, ifAdminStatus up(1), ifOperStatus down(2)",
            Some((
                "SNMP_TRAP_LINK_DOWN",
                SignaturePattern::UpperSnakeTag,
            )),
        ),
        // ── Negatives ──
        ("hello world", None),
        // A webhook body is JSON, and JSON is not an event code.
        ("{\"alert\":\"disk full\",\"host\":\"web1\"}", None),
        // A rendered trap message leads with the dotted OID; traps carry `trap_oid` and must not
        // also mint a signature out of their text.
        ("1.3.6.1.6.3.1.1.5.3 1.3.6.1.2.1.2.2.1.1.4=4;", None),
        // RFC 5424 parsed cleanly, so `app_name` already exists and the body is prose.
        ("link down on ge-0/0/1", None),
        // An embedded id must never become a signature — this is the cardinality guard.
        ("SESSION_12345: user admin logged in", None),
        // Lowercase is prose, whatever punctuation surrounds it.
        ("interface-4-down: something happened", None),
    ];

    #[test]
    fn every_fixture_extracts_what_the_table_says() {
        for (message, expected) in FIXTURES {
            let got = extract_signature(message).map(|s| (s.text, s.pattern));
            assert_eq!(got, *expected, "message: {message}");
        }
    }

    /// The load-bearing invariant: a signature is a slice of its message, never a rewrite of it.
    /// Everything downstream — pasting it into the event search, pasting it into a `substring`
    /// event rule — rests on this.
    #[test]
    fn every_signature_is_a_verbatim_slice_of_its_message() {
        for (message, expected) in FIXTURES {
            let Some(sig) = extract_signature(message) else {
                continue;
            };
            assert!(
                message.contains(sig.text),
                "{:?} is not a substring of {message:?}",
                sig.text
            );
            assert!(expected.is_some(), "unexpected signature in {message:?}");
        }
    }

    #[test]
    fn an_over_long_candidate_is_rejected() {
        let long = "A".repeat(SIGNATURE_MAX_CHARS + 6);
        let msg = format!("host %%01{long}/4/B(l):body");
        assert_eq!(extract_signature(&msg), None);
        // One character shorter than the cap still passes, so the boundary is the cap and not an
        // accident of the fixture.
        let fits = "A".repeat(SIGNATURE_MAX_CHARS - 4);
        let msg = format!("host %%01{fits}/4/B(l):body");
        assert_eq!(
            extract_signature(&msg).map(|s| s.text.len()),
            Some(SIGNATURE_MAX_CHARS)
        );
    }

    #[test]
    fn a_code_beyond_the_scan_window_is_not_found() {
        let msg = format!("{}%%01URL/4/FILTER(l):x", " ".repeat(SIGNATURE_SCAN_CHARS));
        assert_eq!(extract_signature(&msg), None);
    }

    #[test]
    fn a_code_without_its_trailer_is_not_a_signature() {
        // Truncation mid-code is likelier than a code with no punctuation after it; refusing here
        // costs a rare miss and buys immunity from matching `RATE/4/SEC` inside a units string.
        assert_eq!(extract_signature("%%01URL/4/FILTER"), None);
        assert_eq!(extract_signature("%LINEPROTO-5-UPDOWN "), None);
    }

    #[test]
    fn a_two_digit_severity_is_not_accepted() {
        // Guards the shape: a multi-digit middle field is a counter, not a severity.
        assert_eq!(extract_signature("%%01URL/44/FILTER(l):x"), None);
    }

    #[test]
    fn a_later_code_is_found_when_an_earlier_marker_does_not_parse() {
        // The scan must not stop at the first `%%`/`%` it sees.
        assert_eq!(
            extract_signature("rate 95%% then %%01URL/4/FILTER(l):x").map(|s| s.text),
            Some("URL/4/FILTER")
        );
        assert_eq!(
            extract_signature("100% used, %SYS-5-CONFIG_I: configured").map(|s| s.text),
            Some("SYS-5-CONFIG_I")
        );
    }

    #[test]
    fn hostile_input_never_panics() {
        // The same shapes `syslog.rs` guards against, plus multibyte text straddling the window.
        for input in [
            "",
            "%",
            "%%",
            "%%01",
            "%%01/",
            "%-",
            ":",
            "\u{FFFD}%%01A/1/B(l):",
            &"あ".repeat(SIGNATURE_SCAN_CHARS + 10),
            &"%".repeat(1000),
        ] {
            let _ = extract_signature(input);
        }
    }

    #[test]
    fn pattern_tokens_are_distinct() {
        // They are metric label values; a collision would silently merge two populations.
        let tokens = [
            SignaturePattern::HuaweiBrief.as_str(),
            SignaturePattern::CiscoMnemonic.as_str(),
            SignaturePattern::UpperSnakeTag.as_str(),
        ];
        let mut sorted = tokens.to_vec();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), tokens.len());
    }
}
