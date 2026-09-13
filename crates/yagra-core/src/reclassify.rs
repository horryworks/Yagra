// SPDX-License-Identifier: AGPL-3.0-only
//! What Nodes ▸ Reclassify offers (ADR-140): the device nodes whose profile differs from the one the
//! current classification rules choose for what the device says it is.
//!
//! **Computed on every read and never stored**, for the reason ADR-139 decision 2 gives a scan's
//! "already in the tree": rules are edited and nodes are re-profiled by hand after the fact, and a
//! stored proposal would be wrong the moment either happened.
//!
//! **Nothing here applies anything.** A profile carries a node's metric set, its profile-scoped
//! thresholds and its maintenance windows all at once, so a proposal is applied by a person from the
//! screen (decision 8) — and [`check_request`] re-judges it at that moment, because the screen may be
//! hours old.
//!
//! Pure over its arguments. The one input a test cannot fake by hand is the rule set, and
//! [`Classifier::from_rules`] builds that without a database.

use uuid::Uuid;

use crate::classification::{ClassificationMatch, Classifier};
use crate::repo::ReclassifyInput;

/// The most proposals one read lists, and the most nodes one write may name. A fleet whose rules
/// just changed can differ on thousands of nodes; the screen is a table a person reads, and the
/// read's `total` still says how many there are.
pub const PROPOSAL_LIMIT: usize = 5_000;

/// One node the rules would move.
#[derive(Debug)]
pub struct Proposal<'a> {
    pub node: &'a ReclassifyInput,
    /// What the rules chose. Its `profile_id` is never the node's own.
    pub suggestion: ClassificationMatch,
}

/// Everything one read of the screen shows.
#[derive(Debug, Default)]
pub struct ProposalSet<'a> {
    /// In the order the nodes came (the repository sorts by name), at most the limit.
    pub proposals: Vec<Proposal<'a>>,
    /// How many unlocked nodes differ — more than `proposals.len()` when the list was cut.
    pub total: usize,
    /// Locked nodes the rules would move. Counted, never listed: a person said to leave them.
    pub locked: usize,
    /// Nodes the rules cannot be run for yet — no stored `sysObjectID`.
    pub unidentified: usize,
}

/// What the rules choose for one node, or `None` when it cannot be judged.
///
/// 🚨 **A node with a `sysDescr` but no `sysObjectID` is not judged.** Every prefix rule outranks
/// every `sysDescr`-only rule, so running the rules on the description alone would send every Cisco
/// to the Catalyst catch-all and every Linux-based appliance to "Linux server" — a confident, wrong
/// proposal for exactly the nodes an N-1 poller left half-identified (it sends `sys_descr` and not
/// `sys_object_id`).
///
/// Also `None` when the rules match nothing and no "Generic SNMP" profile exists to fall back to.
#[must_use]
pub fn suggestion(node: &ReclassifyInput, classifier: &Classifier) -> Option<ClassificationMatch> {
    let oid = node.sys_object_id.as_deref()?;
    classifier.classify(Some(oid), node.sys_descr.as_deref())
}

/// The proposals for `nodes` under `classifier`, listing at most `limit`.
#[must_use]
pub fn propose<'a>(
    nodes: &'a [ReclassifyInput],
    classifier: &Classifier,
    limit: usize,
) -> ProposalSet<'a> {
    let mut set = ProposalSet::default();
    for node in nodes {
        let Some(suggestion) = suggestion(node, classifier) else {
            set.unidentified += 1;
            continue;
        };
        if node.profile_id == Some(suggestion.profile_id) {
            continue;
        }
        if node.profile_locked {
            set.locked += 1;
            continue;
        }
        set.total += 1;
        if set.proposals.len() < limit {
            set.proposals.push(Proposal { node, suggestion });
        }
    }
    set
}

/// Why a requested change was not made.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Skip {
    /// The caller may not see the node, or it is not a device node, or it does not exist. One
    /// answer for all three, so a scoped caller cannot learn which.
    Hidden,
    /// Someone locked it.
    Locked,
    /// It is no longer on the profile the caller saw, or the rules no longer choose the profile
    /// the caller was shown.
    Changed,
}

/// Re-judge one requested change against what the node is **now** — the screen may be hours old.
/// `Ok` carries the suggestion, whose vendor and model the write takes.
///
/// # Errors
/// The [`Skip`] saying why the change no longer stands.
pub fn check_request(
    node: Option<&ReclassifyInput>,
    from: Option<Uuid>,
    to: Uuid,
    classifier: &Classifier,
) -> Result<ClassificationMatch, Skip> {
    let node = node.ok_or(Skip::Hidden)?;
    if node.profile_locked {
        return Err(Skip::Locked);
    }
    if node.profile_id != from {
        return Err(Skip::Changed);
    }
    match suggestion(node, classifier) {
        Some(s) if s.profile_id == to && node.profile_id != Some(to) => Ok(s),
        _ => Err(Skip::Changed),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use yagra_common::ClassificationRule;

    const CISCO_CATALYST: u128 = 0xCA7;
    const CISCO_WLC: u128 = 0x31C;
    const GENERIC: u128 = 0x9E2E61C;

    fn classifier() -> Classifier {
        let rule =
            |priority: i32, prefix: &str, regex: Option<&str>, profile: u128| ClassificationRule {
                id: Uuid::from_u128(u128::from(priority.unsigned_abs())),
                priority,
                sysobjectid_prefix: Some(prefix.to_owned()),
                sysdescr_regex: regex.map(str::to_owned),
                profile_id: Uuid::from_u128(profile).into(),
                vendor: Some("Cisco".to_owned()),
                model: None,
                enabled: true,
            };
        Classifier::from_rules(
            vec![
                rule(
                    60,
                    "1.3.6.1.4.1.9.",
                    Some("(?i)cisco controller"),
                    CISCO_WLC,
                ),
                rule(80, "1.3.6.1.4.1.9.", None, CISCO_CATALYST),
            ],
            Some(Uuid::from_u128(GENERIC)),
        )
    }

    fn node(name: &str, profile: Option<u128>, oid: Option<&str>, descr: &str) -> ReclassifyInput {
        ReclassifyInput {
            id: Uuid::new_v4(),
            name: name.to_owned(),
            profile_id: profile.map(Uuid::from_u128),
            sys_object_id: oid.map(str::to_owned),
            sys_descr: Some(descr.to_owned()),
            profile_locked: false,
        }
    }

    const WLC_OID: &str = "1.3.6.1.4.1.9.1.1069";

    #[test]
    fn only_a_node_whose_profile_differs_is_proposed() {
        let nodes = vec![
            node(
                "wlc-wrong",
                Some(CISCO_CATALYST),
                Some(WLC_OID),
                "Cisco Controller",
            ),
            node(
                "wlc-right",
                Some(CISCO_WLC),
                Some(WLC_OID),
                "Cisco Controller",
            ),
            node("no-profile", None, Some("1.3.6.1.4.1.9.1.516"), "Cisco IOS"),
        ];
        let set = propose(&nodes, &classifier(), PROPOSAL_LIMIT);
        let names: Vec<&str> = set.proposals.iter().map(|p| p.node.name.as_str()).collect();
        assert_eq!(names, ["wlc-wrong", "no-profile"]);
        assert_eq!(set.total, 2);
        assert_eq!(
            set.proposals[0].suggestion.profile_id,
            Uuid::from_u128(CISCO_WLC)
        );
        assert_eq!((set.locked, set.unidentified), (0, 0));
    }

    #[test]
    fn a_locked_node_is_counted_and_not_listed() {
        let mut locked = node(
            "wlc",
            Some(CISCO_CATALYST),
            Some(WLC_OID),
            "Cisco Controller",
        );
        locked.profile_locked = true;
        // A locked node the rules agree with is not counted: there is nothing it is being kept from.
        let mut agreeing = node(
            "sw",
            Some(CISCO_CATALYST),
            Some("1.3.6.1.4.1.9.1.516"),
            "IOS",
        );
        agreeing.profile_locked = true;
        let nodes = vec![locked, agreeing];
        let set = propose(&nodes, &classifier(), PROPOSAL_LIMIT);
        assert!(set.proposals.is_empty());
        assert_eq!((set.total, set.locked), (0, 1));
    }

    /// 🚨 The description alone would say "Catalyst" here, and be wrong — see [`suggestion`].
    #[test]
    fn a_node_without_a_sys_object_id_is_unidentified_not_guessed() {
        let nodes = vec![node("half", Some(CISCO_WLC), None, "Cisco IOS Software")];
        let set = propose(&nodes, &classifier(), PROPOSAL_LIMIT);
        assert!(set.proposals.is_empty());
        assert_eq!((set.total, set.unidentified), (0, 1));
    }

    #[test]
    fn the_list_is_cut_at_the_limit_and_the_total_is_not() {
        let nodes: Vec<_> = (0..5)
            .map(|i| node(&format!("wlc-{i}"), None, Some(WLC_OID), "Cisco Controller"))
            .collect();
        let set = propose(&nodes, &classifier(), 2);
        assert_eq!(set.proposals.len(), 2);
        assert_eq!(set.total, 5);
    }

    #[test]
    fn a_request_is_rejudged_against_the_node_as_it_is_now() {
        let c = classifier();
        let (catalyst, wlc) = (Uuid::from_u128(CISCO_CATALYST), Uuid::from_u128(CISCO_WLC));
        let n = node(
            "wlc",
            Some(CISCO_CATALYST),
            Some(WLC_OID),
            "Cisco Controller",
        );

        let ok = check_request(Some(&n), Some(catalyst), wlc, &c).expect("still stands");
        assert_eq!(ok.vendor.as_deref(), Some("Cisco"));

        assert_eq!(
            check_request(None, Some(catalyst), wlc, &c),
            Err(Skip::Hidden)
        );
        // Someone moved it since the screen was read.
        assert_eq!(
            check_request(Some(&n), Some(wlc), wlc, &c),
            Err(Skip::Changed)
        );
        // The rules no longer choose what the screen proposed.
        assert_eq!(
            check_request(Some(&n), Some(catalyst), Uuid::from_u128(GENERIC), &c),
            Err(Skip::Changed)
        );
        let mut locked = n.clone();
        locked.profile_locked = true;
        assert_eq!(
            check_request(Some(&locked), Some(catalyst), wlc, &c),
            Err(Skip::Locked)
        );
    }
}
