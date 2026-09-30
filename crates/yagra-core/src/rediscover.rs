// SPDX-License-Identifier: AGPL-3.0-only
//! What Nodes ▸ Rediscover shows and what its Apply may write (ADR-186).
//!
//! A rediscovery re-reads one **monitored** node's address with its own credential — as a
//! one-address discovery sweep, because that sweep says when it has an answer and a poll does not
//! (ADR-186 decision 2) — and puts what the node holds beside what the device now says it is.
//!
//! **Nothing here writes.** A person reads the comparison and presses Apply (ADR-140 decision 8), and
//! [`check_apply`] re-judges the request at that moment against the same scan, because the dialog
//! may be minutes old and the node may have been edited since.
//!
//! Pure over its arguments, like [`crate::reclassify`]: the classifier is built without a database
//! by [`Classifier::from_rules`], and a scan's status is a plain value.

use serde::Serialize;
use uuid::Uuid;

use crate::classification::{identity_of, Classifier};
use crate::discovery::{Candidate, DiscoveryScanState, ScanStatus};
use crate::reclassify::suggestion_for;

/// What the node holds now — the "current" column of the dialog.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Current {
    pub profile_id: Option<Uuid>,
    pub profile_locked: bool,
    pub vendor: Option<String>,
    pub model: Option<String>,
}

/// Where a rediscovery is.
///
/// 🚨 **Only `answered` carries a comparison.** The two waiting states say nothing about
/// the device, and neither may be read as "nothing changed" — that is the mistake this whole
/// feature was built around (a poll's result cannot tell "unchanged" from "not arrived yet").
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum RediscoverState {
    /// Published, and no poller has said anything about it yet.
    Waiting,
    /// A poller has it and is asking the device.
    Reading,
    /// The device answered SNMP; the comparison is ready.
    Answered,
    /// The device answered ping but not SNMP with the node's credential.
    NoSnmpAnswer,
    /// Neither ping nor SNMP answered.
    NoAnswer,
    /// Someone stopped the sweep before it finished.
    Stopped,
}

/// How one row of the comparison reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum RediscoverVerdict {
    /// The device says what the node already holds.
    Same,
    /// The device says something else; Apply may write it.
    Differs,
    /// Nothing could be derived from what the device said. Apply never blanks the node's value.
    Undetermined,
    /// The profile differs, and a person locked it (ADR-140). Apply leaves it.
    Locked,
}

/// One row: what the node holds, what the device now says, and how the two compare.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row<T> {
    pub current: Option<T>,
    pub found: Option<T>,
    pub verdict: RediscoverVerdict,
}

/// What the device said about itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found {
    pub sys_object_id: Option<String>,
    pub sys_descr: Option<String>,
    pub sys_name: Option<String>,
}

/// The three rows, when the device answered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Comparison {
    pub found: Found,
    pub profile: Row<Uuid>,
    /// The rule that chose `profile.found`; `None` ⇒ the device fell through to "Generic SNMP", or
    /// no profile was chosen at all.
    pub rule_id: Option<Uuid>,
    pub vendor: Row<String>,
    pub model: Row<String>,
}

/// Everything the dialog shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Judgement {
    pub state: RediscoverState,
    /// `Some` exactly when `state` is [`RediscoverState::Answered`].
    pub comparison: Option<Comparison>,
}

fn row<T: PartialEq>(current: Option<T>, found: Option<T>, locked: bool) -> Row<T> {
    let verdict = match &found {
        None => RediscoverVerdict::Undetermined,
        Some(f) if current.as_ref() == Some(f) => RediscoverVerdict::Same,
        Some(_) if locked => RediscoverVerdict::Locked,
        Some(_) => RediscoverVerdict::Differs,
    };
    Row {
        current,
        found,
        verdict,
    }
}

/// Where the scan is, and the one candidate it found if it has finished.
fn outcome(status: &ScanStatus) -> (RediscoverState, Option<&Candidate>) {
    match status.state {
        DiscoveryScanState::Queued => (RediscoverState::Waiting, None),
        DiscoveryScanState::Running | DiscoveryScanState::Cancelling => {
            (RediscoverState::Reading, None)
        }
        DiscoveryScanState::Cancelled => (RediscoverState::Stopped, None),
        DiscoveryScanState::Done => match status.candidates.first() {
            // One target, so at most one candidate. None means nothing answered at all.
            None => (RediscoverState::NoAnswer, None),
            Some(c) if c.sysobjectid.is_some() || c.sysdescr.is_some() => {
                (RediscoverState::Answered, Some(c))
            }
            Some(c) if c.reachable => (RediscoverState::NoSnmpAnswer, None),
            Some(_) => (RediscoverState::NoAnswer, None),
        },
    }
}

/// Compare what the node holds with what the scan found.
///
/// The profile is judged by [`suggestion_for`] — the Reclassify rule, so a device with no
/// `sysObjectID` gets no profile proposal (ADR-140 decision 7) — and **not** by the candidate's
/// `suggested_profile_id`, which the sweep chose from `sysDescr` alone when the OID was missing.
/// Vendor and model come from [`identity_of`], the answer the poll path fills.
#[must_use]
pub fn judge(current: &Current, status: &ScanStatus, classifier: &Classifier) -> Judgement {
    let (state, candidate) = outcome(status);
    let comparison = candidate.map(|c| {
        let oid = c.sysobjectid.as_deref();
        let descr = c.sysdescr.as_deref();
        let matched = suggestion_for(oid, descr, classifier);
        let (vendor, model) = identity_of(classifier, oid, descr);
        Comparison {
            found: Found {
                sys_object_id: c.sysobjectid.clone(),
                sys_descr: c.sysdescr.clone(),
                sys_name: c.sysname.clone(),
            },
            profile: row(
                current.profile_id,
                matched.as_ref().map(|m| m.profile_id),
                current.profile_locked,
            ),
            rule_id: matched.and_then(|m| m.rule_id),
            vendor: row(current.vendor.clone(), vendor, false),
            model: row(current.model.clone(), model, false),
        }
    });
    Judgement { state, comparison }
}

/// One field the caller asked to change, echoing what the dialog showed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Change<T> {
    pub from: Option<T>,
    pub to: T,
}

/// What Apply was asked to do.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ApplyRequest {
    pub profile: Option<Change<Uuid>>,
    pub vendor: Option<Change<String>>,
    pub model: Option<Change<String>>,
}

/// What Apply may write: the accepted changes, plus the identity the device reported, so Nodes ▸
/// Reclassify judges from the same answer rather than proposing to undo it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApplyPlan {
    pub profile: Option<Change<Uuid>>,
    pub vendor: Option<Change<String>>,
    pub model: Option<Change<String>>,
    pub sys_object_id: Option<String>,
    pub sys_descr: Option<String>,
}

/// Why an Apply was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    /// The request named no field.
    NothingToApply,
    /// The scan has no comparison — still waiting, or the device did not answer SNMP.
    NotAnswered,
    /// A profile change was asked for on a locked node.
    Locked,
    /// The node no longer holds what the dialog showed as current.
    NodeChanged,
    /// The rules no longer derive what the dialog showed as new, or that field was not a change.
    JudgementChanged,
}

fn accept<T: PartialEq + Clone>(
    asked: Option<&Change<T>>,
    row: &Row<T>,
) -> Result<Option<Change<T>>, Refusal> {
    let Some(asked) = asked else {
        return Ok(None);
    };
    if asked.from != row.current {
        return Err(Refusal::NodeChanged);
    }
    match row.verdict {
        RediscoverVerdict::Differs if row.found.as_ref() == Some(&asked.to) => {
            Ok(Some(asked.clone()))
        }
        RediscoverVerdict::Locked => Err(Refusal::Locked),
        RediscoverVerdict::Differs | RediscoverVerdict::Same | RediscoverVerdict::Undetermined => {
            Err(Refusal::JudgementChanged)
        }
    }
}

/// Re-judge an Apply against the node **now** and the scan it names.
///
/// # Errors
/// The [`Refusal`] saying why nothing may be written.
pub fn check_apply(
    current: &Current,
    status: &ScanStatus,
    classifier: &Classifier,
    request: &ApplyRequest,
) -> Result<ApplyPlan, Refusal> {
    if request.profile.is_none() && request.vendor.is_none() && request.model.is_none() {
        return Err(Refusal::NothingToApply);
    }
    let Some(cmp) = judge(current, status, classifier).comparison else {
        return Err(Refusal::NotAnswered);
    };
    Ok(ApplyPlan {
        profile: accept(request.profile.as_ref(), &cmp.profile)?,
        vendor: accept(request.vendor.as_ref(), &cmp.vendor)?,
        model: accept(request.model.as_ref(), &cmp.model)?,
        sys_object_id: cmp.found.sys_object_id,
        sys_descr: cmp.found.sys_descr,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use yagra_common::ClassificationRule;

    const HUAWEI: u128 = 0x4A1;
    const GENERIC: u128 = 0x9E2E61C;
    const LINUX: u128 = 0x117;

    fn classifier() -> Classifier {
        let rule = |priority: i32,
                    prefix: Option<&str>,
                    regex: Option<&str>,
                    profile: u128,
                    vendor: Option<&str>| ClassificationRule {
            id: Uuid::from_u128(u128::from(priority.unsigned_abs())),
            priority,
            sysobjectid_prefix: prefix.map(str::to_owned),
            sysdescr_regex: regex.map(str::to_owned),
            profile_id: Uuid::from_u128(profile).into(),
            vendor: vendor.map(str::to_owned),
            model: None,
            enabled: true,
        };
        Classifier::from_rules(
            vec![
                rule(10, Some("1.3.6.1.4.1.2011."), None, HUAWEI, Some("Huawei")),
                rule(20, None, Some("(?i)linux"), LINUX, None),
            ],
            Some(Uuid::from_u128(GENERIC)),
        )
    }

    fn current() -> Current {
        Current {
            profile_id: Some(Uuid::from_u128(GENERIC)),
            profile_locked: false,
            vendor: None,
            model: None,
        }
    }

    fn status(state: DiscoveryScanState, candidates: Vec<Candidate>) -> ScanStatus {
        ScanStatus {
            scan_id: Uuid::from_u128(1),
            done: state.is_terminal(),
            state,
            probed: 1,
            total: 1,
            scanning: None,
            started_at: String::new(),
            updated_at: String::new(),
            pool: Some("default".into()),
            candidates,
        }
    }

    fn candidate(oid: Option<&str>, descr: Option<&str>, reachable: bool) -> Candidate {
        Candidate {
            address: "192.0.2.10".into(),
            reachable,
            sysdescr: descr.map(str::to_owned),
            sysname: Some("sw-01".into()),
            sysobjectid: oid.map(str::to_owned),
            // Deliberately a profile the rules would not choose: the judgement must not read it.
            suggested_profile_id: Some(Uuid::from_u128(LINUX)),
            vendor: None,
            model: None,
            matched_credential_id: None,
        }
    }

    fn huawei() -> Candidate {
        candidate(
            Some("1.3.6.1.4.1.2011.2.23.1"),
            Some("Huawei Versatile Routing Platform Software"),
            true,
        )
    }

    fn done(c: Candidate) -> ScanStatus {
        status(DiscoveryScanState::Done, vec![c])
    }

    #[test]
    fn a_scan_still_waiting_or_reading_says_nothing_about_the_device() {
        for (state, want) in [
            (DiscoveryScanState::Queued, RediscoverState::Waiting),
            (DiscoveryScanState::Running, RediscoverState::Reading),
            (DiscoveryScanState::Cancelling, RediscoverState::Reading),
        ] {
            // Even with a candidate already folded in: a partial result is not an answer.
            let j = judge(&current(), &status(state, vec![huawei()]), &classifier());
            assert_eq!(j.state, want);
            assert!(
                j.comparison.is_none(),
                "{state:?} must never produce a comparison, least of all one reading Same"
            );
        }
    }

    #[test]
    fn a_device_that_answered_is_compared_row_by_row() {
        let j = judge(&current(), &done(huawei()), &classifier());
        assert_eq!(j.state, RediscoverState::Answered);
        let c = j.comparison.expect("answered");
        assert_eq!(c.profile.found, Some(Uuid::from_u128(HUAWEI)));
        assert_eq!(c.profile.verdict, RediscoverVerdict::Differs);
        assert_eq!(c.vendor.found.as_deref(), Some("Huawei"));
        assert_eq!(c.vendor.verdict, RediscoverVerdict::Differs);
        assert_eq!(c.found.sys_name.as_deref(), Some("sw-01"));
    }

    #[test]
    fn what_the_node_already_holds_reads_same() {
        let now = Current {
            profile_id: Some(Uuid::from_u128(HUAWEI)),
            profile_locked: false,
            vendor: Some("Huawei".into()),
            model: None,
        };
        let c = judge(&now, &done(huawei()), &classifier())
            .comparison
            .expect("answered");
        assert_eq!(c.profile.verdict, RediscoverVerdict::Same);
        assert_eq!(c.vendor.verdict, RediscoverVerdict::Same);
    }

    #[test]
    fn no_sysobjectid_means_no_profile_proposal_whatever_the_sweep_suggested() {
        let c = judge(
            &current(),
            &done(candidate(None, Some("Linux host 5.15"), true)),
            &classifier(),
        )
        .comparison
        .expect("sysDescr alone is still an SNMP answer");
        assert_eq!(c.profile.found, None);
        assert_eq!(c.profile.verdict, RediscoverVerdict::Undetermined);
    }

    #[test]
    fn a_value_that_could_not_be_derived_is_undetermined_and_never_blanks_the_node() {
        let now = Current {
            model: Some("CE6881".into()),
            ..current()
        };
        let c = judge(&now, &done(huawei()), &classifier())
            .comparison
            .expect("answered");
        assert_eq!(c.model.current.as_deref(), Some("CE6881"));
        assert_eq!(c.model.found, None);
        assert_eq!(c.model.verdict, RediscoverVerdict::Undetermined);
        let refused = check_apply(
            &now,
            &done(huawei()),
            &classifier(),
            &ApplyRequest {
                model: Some(Change {
                    from: Some("CE6881".into()),
                    to: String::new(),
                }),
                ..ApplyRequest::default()
            },
        );
        assert_eq!(refused, Err(Refusal::JudgementChanged));
    }

    #[test]
    fn a_locked_profile_is_shown_as_locked_and_apply_leaves_it() {
        let now = Current {
            profile_locked: true,
            ..current()
        };
        let c = judge(&now, &done(huawei()), &classifier())
            .comparison
            .expect("answered");
        assert_eq!(c.profile.verdict, RediscoverVerdict::Locked);
        let refused = check_apply(
            &now,
            &done(huawei()),
            &classifier(),
            &ApplyRequest {
                profile: Some(Change {
                    from: Some(Uuid::from_u128(GENERIC)),
                    to: Uuid::from_u128(HUAWEI),
                }),
                ..ApplyRequest::default()
            },
        );
        assert_eq!(refused, Err(Refusal::Locked));
    }

    #[test]
    fn silence_is_told_apart_from_ping_without_snmp() {
        assert_eq!(
            judge(
                &current(),
                &done(candidate(None, None, true)),
                &classifier()
            )
            .state,
            RediscoverState::NoSnmpAnswer
        );
        assert_eq!(
            judge(
                &current(),
                &status(DiscoveryScanState::Done, Vec::new()),
                &classifier()
            )
            .state,
            RediscoverState::NoAnswer
        );
        assert_eq!(
            judge(
                &current(),
                &status(DiscoveryScanState::Cancelled, Vec::new()),
                &classifier()
            )
            .state,
            RediscoverState::Stopped
        );
    }

    #[test]
    fn apply_takes_what_was_shown_and_carries_the_identity_with_it() {
        let plan = check_apply(
            &current(),
            &done(huawei()),
            &classifier(),
            &ApplyRequest {
                profile: Some(Change {
                    from: Some(Uuid::from_u128(GENERIC)),
                    to: Uuid::from_u128(HUAWEI),
                }),
                vendor: Some(Change {
                    from: None,
                    to: "Huawei".into(),
                }),
                model: None,
            },
        )
        .expect("accepted");
        assert_eq!(plan.profile.map(|c| c.to), Some(Uuid::from_u128(HUAWEI)));
        assert_eq!(plan.vendor.map(|c| c.to).as_deref(), Some("Huawei"));
        assert_eq!(plan.model, None);
        assert_eq!(
            plan.sys_object_id.as_deref(),
            Some("1.3.6.1.4.1.2011.2.23.1")
        );
    }

    #[test]
    fn apply_refuses_what_the_node_or_the_rules_have_moved_away_from() {
        let asked = ApplyRequest {
            profile: Some(Change {
                from: Some(Uuid::from_u128(GENERIC)),
                to: Uuid::from_u128(HUAWEI),
            }),
            ..ApplyRequest::default()
        };
        let edited = Current {
            profile_id: Some(Uuid::from_u128(LINUX)),
            ..current()
        };
        assert_eq!(
            check_apply(&edited, &done(huawei()), &classifier(), &asked),
            Err(Refusal::NodeChanged)
        );
        let other = ApplyRequest {
            profile: Some(Change {
                from: Some(Uuid::from_u128(GENERIC)),
                to: Uuid::from_u128(LINUX),
            }),
            ..ApplyRequest::default()
        };
        assert_eq!(
            check_apply(&current(), &done(huawei()), &classifier(), &other),
            Err(Refusal::JudgementChanged)
        );
        assert_eq!(
            check_apply(
                &current(),
                &status(DiscoveryScanState::Running, Vec::new()),
                &classifier(),
                &asked
            ),
            Err(Refusal::NotAnswered)
        );
        assert_eq!(
            check_apply(
                &current(),
                &done(huawei()),
                &classifier(),
                &ApplyRequest::default()
            ),
            Err(Refusal::NothingToApply)
        );
    }
}
