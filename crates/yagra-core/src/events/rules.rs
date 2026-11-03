// SPDX-License-Identifier: AGPL-3.0-only
//! **A stored rule becomes a matcher** (ADR-095): pattern compilation, scoping, and the compiled
//! snapshot [`super::engine`] evaluates against.
//!
//! Compilation is fallible and **failure narrows rather than widens** — a rule with a bad regex, a
//! disabled rule, and a rule naming a source kind this build does not know all compile to nothing,
//! so an unreadable rule matches no events instead of every event. That direction is the whole
//! reason this is a separate step from evaluation.

use uuid::Uuid;
use yagra_bus::EventKind;
use yagra_common::Severity;

// The vocabulary lives in the parent, which a child can see without any widening — see
// `super`'s doc for why that is what decides where a thing goes here.
use super::*;

// ─── Rule matching (pure) ───────────────────────────────────────────────────────────

/// A compiled match expression. Substring is checked before regex site-wide (cheap first);
/// matching is case-sensitive (use `(?i)` in a regex for case-insensitive).
#[derive(Debug, Clone)]
pub enum Matcher {
    Substring(String),
    Regex(regex::Regex),
}

impl Matcher {
    /// Whether `text` matches (also used by the API's rule-test endpoint).
    #[must_use]
    pub fn matches(&self, text: &str) -> bool {
        match self {
            Self::Substring(s) => text.contains(s.as_str()),
            Self::Regex(re) => re.is_match(text),
        }
    }
}

/// Compile a matcher, enforcing the same bounds as the DB CHECKs. Shared by the engine
/// snapshot, the API-edge validation, and the rule-test endpoint.
pub fn compile_matcher(match_kind: &str, pattern: &str) -> Result<Matcher, String> {
    if pattern.is_empty() || pattern.len() > 512 {
        return Err("pattern must be 1..=512 characters".to_owned());
    }
    match match_kind {
        "substring" => Ok(Matcher::Substring(pattern.to_owned())),
        "regex" => regex::RegexBuilder::new(pattern)
            .size_limit(1 << 20)
            .build()
            .map(Matcher::Regex)
            .map_err(|e| e.to_string()),
        other => Err(format!("unknown match kind {other:?}")),
    }
}

/// Compile a pattern that is a regex **by construction** — the caller holds a boolean, not a stored
/// match-kind string.
///
/// Separate from [`compile_matcher`] because that function's first parameter is the matcher *kind*,
/// and passing anything else is not a compile error: it is an `unknown match kind` at runtime, on
/// the branch a rejection test cannot distinguish from a correct rejection. That is exactly what
/// happened — the event filter's per-column condition passed the *column name* there, so
/// `msg_regex=true` rejected every pattern it was ever given while the test asserting that a broken
/// pattern is refused went on passing. A caller with a boolean should never have to name a kind.
pub fn compile_regex(pattern: &str) -> Result<Matcher, String> {
    compile_matcher("regex", pattern)
}

/// Whether a rule's target covers `node` (ADR-232): no target is every node, a node target is that
/// node, a folder target is every node whose folder chain (`chain`, nearest first) holds it.
///
/// `chain` is `None` for a node the alert config has not seen yet, and a folder rule then does not
/// apply: an unknown position narrows, never widens. Shared by the matcher and by the
/// "which rules reach this node" list, so the two cannot disagree.
#[must_use]
pub fn targets_node(
    rule_node: Option<Uuid>,
    rule_group: Option<Uuid>,
    node: Uuid,
    chain: Option<&[Uuid]>,
) -> bool {
    match (rule_node, rule_group) {
        (Some(n), _) => n == node,
        (None, Some(g)) => chain.is_some_and(|c| c.contains(&g)),
        (None, None) => true,
    }
}

/// The stored rules that reach `node` — every-node rules, rules on one of its folders, and rules on
/// the node itself (`GET /api/v1/event-rules?node_id=`, and MCP's `get_config`).
#[must_use]
pub fn rules_for_node(
    rules: Vec<StoredEventRule>,
    node: Uuid,
    chain: Option<&[Uuid]>,
) -> Vec<StoredEventRule> {
    rules
        .into_iter()
        .filter(|r| targets_node(r.node_id, r.group_id, node, chain))
        .collect()
}

/// A rule compiled for the hot path.
#[derive(Debug, Clone)]
pub struct CompiledRule {
    pub id: Uuid,
    pub name: String,
    source_kind: Option<EventKind>,
    source_id: Option<Uuid>,
    node_id: Option<Uuid>,
    group_id: Option<Uuid>,
    pub(super) matcher: Matcher,
    pub(super) clear_matcher: Option<Matcher>,
    pub severity: Severity,
    pub(super) ttl_secs: u32,
    pub(super) min_count: u32,
    pub(super) window_secs: u32,
}

impl CompiledRule {
    /// Whether this rule applies to an event of `kind` from `source_id` on `node`, whose folder
    /// chain is `chain` (see [`targets_node`]).
    pub(super) fn applies(
        &self,
        kind: EventKind,
        source: Option<Uuid>,
        node: Uuid,
        chain: Option<&[Uuid]>,
    ) -> bool {
        self.source_kind.is_none_or(|k| k == kind)
            && self.source_id.is_none_or(|s| Some(s) == source)
            && targets_node(self.node_id, self.group_id, node, chain)
    }
}

/// Compile a stored rule; `None` for disabled rules or a pattern that no longer compiles
/// (rejected at the API edge, so this only catches drift — logged by the caller).
pub(super) fn compile_rule(stored: &StoredEventRule) -> Option<CompiledRule> {
    if !stored.enabled {
        return None;
    }
    let matcher = compile_matcher(stored.match_kind.as_str(), &stored.pattern).ok()?;
    let clear_matcher = match stored.clear_pattern.as_deref() {
        Some(p) => Some(compile_matcher(stored.match_kind.as_str(), p).ok()?),
        None => None,
    };
    // A null `source_kind` means "any kind" — see `applies`. So an unparseable *non-null* one must
    // drop the rule, not fall through to `None`: that would silently widen a rule scoped to one
    // stream into one that matches every event, which is the opposite of what the operator asked
    // for and would fire on traffic they never intended it to see.
    let source_kind = match stored.source_kind.as_deref() {
        None => None,
        Some(raw) => match EventKind::from_token(raw) {
            Some(k) => Some(k),
            None => {
                tracing::warn!(
                    rule = %stored.id,
                    source_kind = %raw,
                    "event rule names an unknown source kind; dropping it rather than widening it to every stream"
                );
                return None;
            }
        },
    };
    Some(CompiledRule {
        id: stored.id,
        name: stored.name.clone(),
        source_kind,
        source_id: stored.source_id,
        node_id: stored.node_id,
        group_id: stored.group_id,
        matcher,
        clear_matcher,
        severity: stored.severity,
        ttl_secs: u32::try_from(stored.ttl_secs).unwrap_or(1800),
        min_count: u32::try_from(stored.min_count).unwrap_or(1).max(1),
        window_secs: u32::try_from(stored.window_secs).unwrap_or(60).max(1),
    })
}

#[cfg(test)]
mod tests {
    use super::super::testkit::stored_rule;
    use super::*;

    #[test]
    fn substring_and_regex_matchers() {
        let sub = compile_matcher("substring", "link down").unwrap();
        assert!(sub.matches("chassisd: link down on ge-0/0/1"));
        assert!(!sub.matches("link up"));

        let re = compile_matcher("regex", r"(?i)%LINEPROTO-\d-UPDOWN").unwrap();
        assert!(re.matches("%lineproto-5-updown: state change"));
        assert!(!re.matches("%SYS-5-CONFIG_I"));
    }

    #[test]
    fn invalid_patterns_are_rejected() {
        assert!(compile_matcher("regex", "(unclosed").is_err());
        assert!(compile_matcher("substring", "").is_err());
        assert!(compile_matcher("substring", &"a".repeat(513)).is_err());
        assert!(compile_matcher("glob", "x*").is_err());
        // Pathological expansion (100^4 states) is capped by the 1 MiB size limit.
        assert!(compile_matcher("regex", "((((a{100}){100}){100}){100})").is_err());
    }

    #[test]
    fn rule_scoping_applies_kind_source_and_node() {
        let node = Uuid::new_v4();
        let other_node = Uuid::new_v4();
        let source = Uuid::new_v4();
        let mut stored = stored_rule("x", "warning");
        stored.source_kind = Some("syslog".into());
        stored.node_id = Some(node);
        let rule = compile_rule(&stored).unwrap();

        assert!(rule.applies(EventKind::Syslog, None, node, None));
        assert!(!rule.applies(EventKind::Trap, None, node, None));
        assert!(!rule.applies(EventKind::Syslog, None, other_node, None));

        let mut stored = stored_rule("x", "warning");
        stored.source_id = Some(source);
        let rule = compile_rule(&stored).unwrap();
        assert!(rule.applies(EventKind::Webhook, Some(source), node, None));
        assert!(!rule.applies(EventKind::Webhook, Some(Uuid::new_v4()), node, None));
        assert!(!rule.applies(EventKind::Webhook, None, node, None));
    }

    #[test]
    fn a_folder_rule_reaches_the_nodes_under_that_folder_and_no_others() {
        let (node, outer, inner, elsewhere) = (
            Uuid::new_v4(),
            Uuid::new_v4(),
            Uuid::new_v4(),
            Uuid::new_v4(),
        );
        let chain = [inner, outer];
        let mut stored = stored_rule("x", "warning");
        stored.group_id = Some(outer);
        let rule = compile_rule(&stored).unwrap();

        assert!(rule.applies(EventKind::Syslog, None, node, Some(&chain)));
        assert!(!rule.applies(EventKind::Syslog, None, node, Some(&[elsewhere])));
        assert!(!rule.applies(EventKind::Syslog, None, node, Some(&[])));
        // A node the alert config has not seen yet: its folder is unknown, so a folder rule must
        // not reach it — narrowing, never widening.
        assert!(!rule.applies(EventKind::Syslog, None, node, None));
        // A rule with no target still reaches that node.
        let every = compile_rule(&stored_rule("x", "warning")).unwrap();
        assert!(every.applies(EventKind::Syslog, None, node, None));
    }

    #[test]
    fn the_rules_for_a_node_are_every_node_its_folders_and_itself() {
        let (node, other, folder, other_folder) = (
            Uuid::new_v4(),
            Uuid::new_v4(),
            Uuid::new_v4(),
            Uuid::new_v4(),
        );
        let rule = |name: &str, node_id: Option<Uuid>, group_id: Option<Uuid>| {
            let mut r = stored_rule("x", "warning");
            r.name = name.into();
            r.node_id = node_id;
            r.group_id = group_id;
            r
        };
        let all = vec![
            rule("every", None, None),
            rule("folder", None, Some(folder)),
            rule("node", Some(node), None),
            rule("other folder", None, Some(other_folder)),
            rule("other node", Some(other), None),
        ];
        let names = |rules: Vec<StoredEventRule>| -> Vec<String> {
            rules.into_iter().map(|r| r.name).collect()
        };
        assert_eq!(
            names(rules_for_node(all.clone(), node, Some(&[folder]))),
            ["every", "folder", "node"]
        );
        assert_eq!(
            names(rules_for_node(all, node, None)),
            ["every", "node"],
            "an unknown folder position keeps the folder rules out"
        );
    }

    #[test]
    fn disabled_and_broken_rules_do_not_compile() {
        let mut stored = stored_rule("x", "warning");
        stored.enabled = false;
        assert!(compile_rule(&stored).is_none());

        let mut stored = stored_rule("(bad", "warning");
        stored.match_kind = EventMatchKind::Regex;
        assert!(compile_rule(&stored).is_none());
    }

    #[test]
    fn an_unknown_source_kind_drops_the_rule_rather_than_widening_it() {
        // `source_kind: None` means "any kind" (see `applies`), so parsing an unrecognised token
        // *to* None would turn a rule the operator scoped to one stream into one that matches every
        // event — a rule firing on traffic nobody pointed it at. A newer core writing a fourth kind
        // is the case that produces this, so the rule sits out until this core is upgraded too.
        let mut stored = stored_rule("x", "warning");
        stored.source_kind = Some("kafka".into());
        assert!(compile_rule(&stored).is_none());

        // A genuinely absent kind still means "any", which is the operator's own choice.
        let mut stored = stored_rule("x", "warning");
        stored.source_kind = None;
        let rule = compile_rule(&stored).expect("a kind-less rule still compiles");
        let node = Uuid::new_v4();
        for kind in EventKind::ALL {
            assert!(rule.applies(kind, None, node, None));
        }
    }
}
