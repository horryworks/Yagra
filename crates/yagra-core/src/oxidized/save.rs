// SPDX-License-Identifier: AGPL-3.0-only
//! What the save script does after a fetch (ADR-222).
//!
//! **The one exception to "nothing in `oxidized/` speaks to a device".** The commands it may send
//! are `yagra_common::config_save::SAVE_COMMANDS` and nothing else (decision 2). The script
//! (`yagra-save.rb`) runs on oxidized's `node_success` hook, right after a fetch, with the login that
//! fetch used, and asks core what to do; core answers from [`decide`]. A save is therefore always
//! preceded by a backup of what it saves (decision 7).

use uuid::Uuid;
use yagra_common::config_save::commands_for;

/// What the script does after a fetch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Action {
    /// Save, recording the result against this save.
    Save(Uuid),
    /// Only compare running with startup and report it (decision 4).
    Check,
}

/// What to do for a node whose fetch just succeeded, given the save waiting for it, if any.
///
/// `None` is "nothing": no save asked for, and either no table entry or a model whose unsaved
/// state Yagra reads over SNMP instead.
#[must_use]
pub(crate) fn decide(model: &str, pending: Option<Uuid>) -> Option<Action> {
    let commands = commands_for(model)?;
    match pending {
        Some(id) => Some(Action::Save(id)),
        None => commands.compare.map(|_| Action::Check),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pending_save_is_run_and_otherwise_only_a_comparable_model_is_checked() {
        let id = Uuid::new_v4();
        assert_eq!(decide("vrp", Some(id)), Some(Action::Save(id)));
        assert_eq!(decide("vrp", None), Some(Action::Check));
        assert_eq!(decide("ios", Some(id)), Some(Action::Save(id)));
        assert_eq!(decide("ios", None), None);
        assert_eq!(decide("junos", Some(id)), None);
    }
}
