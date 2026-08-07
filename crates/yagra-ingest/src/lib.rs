// SPDX-License-Identifier: AGPL-3.0-only
//! Yagra-ingest — passive event parsing for the poller's edge listeners (Phase 2).
//!
//! Pure logic only: syslog parsing (RFC 5424 → RFC 3164 → raw fallback), vendor event-code
//! extraction for the messages that fit neither RFC (`signature`), SNMP trap PDU
//! normalization (over the vendored `snmp2` decode types), **traffic-flow decoding (NetFlow
//! v5/v9, IPFIX and sFlow v5, with the template cache and per-exporter aggregator — ADR-031,
//! and by volume the largest thing in this crate)**, and a per-source token-bucket rate limiter.
//! The UDP socket loops live in `yagra-poller` (`listeners.rs`); keeping the parsers here lets
//! them unit-test against byte fixtures — including hostile ones — without any I/O, and isolates
//! the raw SNMP library dependency the way `yagra-transport` does for the polling path.
//!
//! **Robustness contract:** every input here is an attacker-controlled datagram. Parsers
//! never panic — syslog parsing is infallible (worst case: raw fallback), trap parsing
//! returns `Err` on anything malformed, and both clip output to fixed size caps.

pub mod flow;
pub mod ratelimit;
pub mod signature;
pub mod syslog;
pub mod trap;

pub use flow::{
    carries_template_set, parse_flow_export, parse_sflow, AggregatedFlow, ExporterBatch,
    ExporterBuckets, FlowAggregator, FlowError, FlowTemplates, RawFlow, DEFAULT_FLOW_TOP_N,
};
pub use ratelimit::SourceLimiter;
pub use signature::{
    extract_signature, Signature, SignaturePattern, SIGNATURE_MAX_CHARS, SIGNATURE_SCAN_CHARS,
};
pub use syslog::{parse_syslog, SyslogEvent, SyslogFormat};
pub use trap::{build_inform_response, parse_trap, TrapError, TrapEvent};

/// Maximum characters kept in a normalized event message; anything longer is clipped
/// (and flagged `truncated`). Matches the `events.message` DB CHECK in core.
pub const MAX_EVENT_TEXT: usize = 4096;

/// Clip `text` to at most `max` characters (not bytes), returning whether it was clipped.
#[must_use]
pub(crate) fn clip_chars(text: &str, max: usize) -> (String, bool) {
    match text.char_indices().nth(max) {
        Some((byte_idx, _)) => (text[..byte_idx].to_owned(), true),
        None => (text.to_owned(), false),
    }
}

/// Clip a normalized event message to [`MAX_EVENT_TEXT`] characters, returning whether it
/// was truncated. Used by the trap path (the syslog parser already clips internally) so
/// every event message satisfies the `events.message` DB CHECK — a rendered trap with
/// many/large varbinds can otherwise exceed the cap and be rejected on insert.
#[must_use]
pub fn clip_event_text(text: &str) -> (String, bool) {
    clip_chars(text, MAX_EVENT_TEXT)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clip_respects_char_boundaries() {
        let (s, truncated) = clip_chars("あいうえお", 3);
        assert_eq!(s, "あいう");
        assert!(truncated);

        let (s, truncated) = clip_chars("short", 10);
        assert_eq!(s, "short");
        assert!(!truncated);
    }

    #[test]
    fn clip_event_text_bounds_to_max() {
        let long = "a".repeat(MAX_EVENT_TEXT + 500);
        let (s, truncated) = clip_event_text(&long);
        assert_eq!(s.chars().count(), MAX_EVENT_TEXT);
        assert!(truncated);

        let (s, truncated) = clip_event_text("fits");
        assert_eq!(s, "fits");
        assert!(!truncated);
    }
}
