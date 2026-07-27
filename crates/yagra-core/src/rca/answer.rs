// SPDX-License-Identifier: AGPL-3.0-only
//! Reading what the model said (ADR-029).
//!
//! The prompt asks for a JSON object. Models mostly comply, and then occasionally wrap it in a
//! ```` ```json ```` fence, prefix it with "Here is the analysis:", or answer in prose because the
//! incident confused them. **None of those is an error worth failing an operator's request over** —
//! the text is still the explanation they asked for. So parsing degrades: strict JSON first, then a
//! braces-only rescue, then the raw text presented as-is with [`RcaAnswer::raw`] set so the UI can
//! render it plainly instead of pretending it has structured sections.
//!
//! Everything is bounded before it is stored. Model output is not attacker-controlled in the usual
//! sense, but it is *unbounded* by nature and it lands in PostgreSQL and then in a browser; a model
//! that loops for eight thousand tokens should cost a truncated report, not a wide row.
//!
//! Pure — no clock, no network, no store — so every one of these shapes is a unit test.

use serde::{Deserialize, Serialize};

/// Longest stored summary. It is one sentence by contract; the cap is for when it is not.
const MAX_SUMMARY_CHARS: usize = 500;
/// Longest stored value for the prose sections and each next step.
const MAX_SECTION_CHARS: usize = 4_000;
/// Most next steps kept. A model listing thirty things to check has not prioritized anything.
const MAX_NEXT_STEPS: usize = 10;
/// Longest raw fallback text kept.
const MAX_RAW_CHARS: usize = 16_000;

/// The parsed explanation.
///
/// Every field is plain text and stays plain text: it is rendered as text by the WebUI, never as
/// HTML or markdown. Model output is untrusted for the same reason device output is (security.md),
/// and here it is doubly so — the model was itself reading device output.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct RcaAnswer {
    /// One sentence an on-call engineer can act on.
    pub summary: String,
    /// What most likely failed, and why the evidence says so.
    #[serde(default)]
    pub root_cause: String,
    /// Whether the other affected nodes are consequences of it.
    #[serde(default)]
    pub dependents: String,
    /// Concrete things to check next.
    #[serde(default)]
    pub next_steps: Vec<String>,
    /// `high` | `medium` | `low`, or `unknown` when the model said something else.
    #[serde(default)]
    pub confidence: String,
    /// Set when the reply was not parseable JSON: the model's text, verbatim and bounded. Its
    /// presence is what tells the UI to render one prose block rather than empty sections.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw: Option<String>,
}

/// What the model was asked to return. Separate from [`RcaAnswer`] so the wire contract can be
/// forgiving (missing fields default) without that leniency leaking into the stored shape.
#[derive(Debug, Deserialize)]
struct Wire {
    #[serde(default)]
    summary: String,
    #[serde(default)]
    root_cause: String,
    #[serde(default)]
    dependents: String,
    #[serde(default)]
    next_steps: Vec<String>,
    #[serde(default)]
    confidence: String,
}

/// Parse a completion into an answer. Never fails: an unreadable reply becomes a raw one.
#[must_use]
pub fn parse(text: &str) -> RcaAnswer {
    match extract_json(text).and_then(|json| serde_json::from_str::<Wire>(&json).ok()) {
        // A JSON object with no summary is not usable structure — treat it as prose rather than
        // showing the operator a report whose headline is blank.
        Some(w) if !w.summary.trim().is_empty() => RcaAnswer {
            summary: clamp(&w.summary, MAX_SUMMARY_CHARS),
            root_cause: clamp(&w.root_cause, MAX_SECTION_CHARS),
            dependents: clamp(&w.dependents, MAX_SECTION_CHARS),
            next_steps: w
                .next_steps
                .iter()
                .filter(|s| !s.trim().is_empty())
                .take(MAX_NEXT_STEPS)
                .map(|s| clamp(s, MAX_SECTION_CHARS))
                .collect(),
            confidence: normalize_confidence(&w.confidence),
            raw: None,
        },
        _ => raw_answer(text),
    }
}

/// The fallback: keep the text, and derive a headline from its first real line so list views and
/// the modal header still have something to show.
fn raw_answer(text: &str) -> RcaAnswer {
    let trimmed = text.trim();
    let first_line = trimmed
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("The model returned no text.");
    RcaAnswer {
        summary: clamp(first_line, MAX_SUMMARY_CHARS),
        confidence: "unknown".to_owned(),
        raw: Some(clamp(trimmed, MAX_RAW_CHARS)),
        ..RcaAnswer::default()
    }
}

/// Pull the JSON object out of a reply.
///
/// Handles the two shapes models actually produce beyond bare JSON: a fenced code block, and JSON
/// with commentary around it. The brace scan is outermost-first (`find` to `rfind`) because the
/// object we want encloses any nested ones.
fn extract_json(text: &str) -> Option<String> {
    let t = strip_fence(text.trim());
    let start = t.find('{')?;
    let end = t.rfind('}')?;
    if end <= start {
        return None;
    }
    Some(t[start..=end].to_owned())
}

/// Remove a surrounding ```` ``` ```` fence, with or without a language tag.
fn strip_fence(text: &str) -> &str {
    let Some(rest) = text.strip_prefix("```") else {
        return text;
    };
    // Drop the language tag line (```json), if any.
    let body = rest.split_once('\n').map_or(rest, |(_, after)| after);
    body.trim_end()
        .strip_suffix("```")
        .unwrap_or(body)
        .trim_matches('\n')
}

/// Constrain confidence to the closed set the UI styles. Anything else becomes `unknown` rather
/// than being passed through: the value drives a badge, and an unbounded string there would be both
/// an unstyled badge and (were it ever counted) an unbounded metric label.
fn normalize_confidence(raw: &str) -> String {
    match raw.trim().to_ascii_lowercase().as_str() {
        "high" => "high",
        "medium" | "moderate" => "medium",
        "low" => "low",
        _ => "unknown",
    }
    .to_owned()
}

/// Trim and cap on a char boundary, marking the cut so a truncated section does not read as a
/// complete one that simply ends abruptly.
fn clamp(s: &str, max: usize) -> String {
    let s = s.trim();
    if s.chars().count() <= max {
        return s.to_owned();
    }
    let cut = s
        .char_indices()
        .nth(max)
        .map_or(s.len(), |(byte_idx, _)| byte_idx);
    format!("{}…", &s[..cut])
}

#[cfg(test)]
mod tests {
    use super::*;

    const GOOD: &str = r#"{
      "summary": "core-sw-01 is unreachable and took 12 access switches with it.",
      "root_cause": "The uplink flapped, then stopped responding to ICMP entirely.",
      "dependents": "All 12 are single-homed behind it, so they are consequences.",
      "next_steps": ["Check the uplink optic", "Look at the upstream router's logs"],
      "confidence": "high"
    }"#;

    #[test]
    fn a_well_formed_reply_parses_into_sections() {
        let a = parse(GOOD);
        assert!(a.summary.starts_with("core-sw-01 is unreachable"));
        assert_eq!(a.next_steps.len(), 2);
        assert_eq!(a.confidence, "high");
        assert!(a.raw.is_none(), "structured replies carry no raw block");
    }

    #[test]
    fn a_fenced_reply_parses_the_same_way() {
        // The single most common deviation from the contract.
        let fenced = format!("```json\n{GOOD}\n```");
        assert_eq!(parse(&fenced), parse(GOOD));
    }

    #[test]
    fn commentary_around_the_object_is_ignored() {
        let chatty = format!("Here is my analysis:\n\n{GOOD}\n\nHope that helps!");
        assert_eq!(parse(&chatty).confidence, "high");
    }

    #[test]
    fn prose_falls_back_to_raw_with_a_headline() {
        let a = parse("The switch is down.\nIts children followed it.");
        assert_eq!(a.summary, "The switch is down.");
        assert_eq!(
            a.raw.as_deref(),
            Some("The switch is down.\nIts children followed it.")
        );
        assert_eq!(a.confidence, "unknown");
        assert!(a.root_cause.is_empty(), "no structure was invented");
    }

    #[test]
    fn json_without_a_summary_is_treated_as_prose() {
        // Structure with a blank headline is worse than no structure: the modal would open on an
        // empty line. Better to show what the model actually wrote.
        let a = parse(r#"{"root_cause": "something", "confidence": "high"}"#);
        assert!(a.raw.is_some());
    }

    #[test]
    fn an_empty_reply_still_produces_a_readable_answer() {
        let a = parse("   \n  ");
        assert_eq!(a.summary, "The model returned no text.");
        assert_eq!(a.confidence, "unknown");
    }

    #[test]
    fn confidence_is_constrained_to_the_styled_set() {
        assert_eq!(normalize_confidence("High"), "high");
        assert_eq!(normalize_confidence(" MODERATE "), "medium");
        assert_eq!(normalize_confidence("very high indeed"), "unknown");
        assert_eq!(normalize_confidence(""), "unknown");
    }

    #[test]
    fn everything_stored_is_bounded() {
        let long = "x".repeat(50_000);
        let reply = serde_json::json!({
            "summary": long,
            "root_cause": long,
            "dependents": long,
            "next_steps": (0..40).map(|_| long.clone()).collect::<Vec<_>>(),
            "confidence": "low",
        })
        .to_string();
        let a = parse(&reply);
        assert_eq!(
            a.summary.chars().count(),
            MAX_SUMMARY_CHARS + 1,
            "+1 = the ellipsis"
        );
        assert_eq!(a.root_cause.chars().count(), MAX_SECTION_CHARS + 1);
        assert_eq!(a.next_steps.len(), MAX_NEXT_STEPS);
        assert!(a
            .next_steps
            .iter()
            .all(|s| s.chars().count() == MAX_SECTION_CHARS + 1));
    }

    #[test]
    fn raw_fallback_is_bounded_too() {
        let a = parse(&"あ".repeat(30_000));
        // Multi-byte on purpose: a byte-indexed truncation here would panic.
        assert_eq!(a.raw.as_ref().unwrap().chars().count(), MAX_RAW_CHARS + 1);
    }

    #[test]
    fn blank_next_steps_are_dropped_rather_than_shown_as_empty_bullets() {
        let a = parse(r#"{"summary":"s","next_steps":["real","","   "]}"#);
        assert_eq!(a.next_steps, vec!["real".to_owned()]);
    }

    #[test]
    fn a_fence_without_a_language_tag_still_unwraps() {
        let a = parse(&format!("```\n{GOOD}\n```"));
        assert_eq!(a.confidence, "high");
    }
}
