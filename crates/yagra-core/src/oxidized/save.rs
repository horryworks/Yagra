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
    /// Compare running with startup, and save in the same login when they differ (decision 13).
    /// Nothing is recorded in advance: the report says whether a save was tried.
    SaveIfUnsaved,
    /// Only compare running with startup and report it (decision 4).
    Check,
}

/// What to do for a node whose fetch just succeeded, given the save waiting for it, if any, and
/// whether Yagra may save it on its own now.
///
/// `None` is "nothing": no save asked for, and either no table entry or a model whose unsaved
/// state Yagra reads over SNMP instead - an automatic save of one of those is asked for in advance,
/// and so arrives here as `pending`.
#[must_use]
pub(crate) fn decide(model: &str, pending: Option<Uuid>, auto: bool) -> Option<Action> {
    let commands = commands_for(model)?;
    match pending {
        Some(id) => Some(Action::Save(id)),
        None => commands.compare.map(|_| {
            if auto {
                Action::SaveIfUnsaved
            } else {
                Action::Check
            }
        }),
    }
}

/// Whether Yagra has a save command for the model a backed-up node is fetched with: the picked
/// model, else its profile's (ADR-222 Inc.11 decision 16). The one answer the switch and the
/// clean-up below share.
#[must_use]
pub(crate) fn can_save(t: &yagra_base::config_backup::BackupTarget) -> bool {
    super::source::model_of(
        t.config.model_override.as_deref(),
        t.profile_name.as_deref(),
    )
    .is_some_and(|m| commands_for(&m).is_some())
}

/// The nodes switched on for automatic save whose model has no save command: carried over from
/// the deployment-wide switch by migration 0179, or moved since to a model Yagra cannot save. They
/// are switched off, so a later change of model back to one Yagra can save does not start saving
/// a device nobody switched on for it.
#[must_use]
pub(crate) fn unsaveable_auto_saves(
    targets: &[yagra_base::config_backup::BackupTarget],
) -> Vec<Uuid> {
    targets
        .iter()
        .filter(|t| t.auto_save && !can_save(t))
        .map(|t| t.node_id)
        .collect()
}

/// Switch off automatic save on every node [`unsaveable_auto_saves`] names. Run by the leader's
/// status loop, so it is settled within one tick of start-up and of any change of model.
pub(crate) async fn switch_off_unsaveable(repo: &yagra_base::config_backup::ConfigBackupRepo) {
    let targets = match repo.targets().await {
        Ok(t) => t,
        Err(e) => {
            tracing::warn!(error = %e, "config save: could not read the backed-up nodes");
            return;
        }
    };
    let ids = unsaveable_auto_saves(&targets);
    if ids.is_empty() {
        return;
    }
    match repo.set_auto_save(&ids, false).await {
        Ok(off) => {
            for node in off {
                tracing::info!(%node, "config save: automatic save switched off, the model has no save command");
            }
        }
        Err(e) => tracing::warn!(error = %e, "config save: could not switch off automatic save"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target(
        model: Option<&str>,
        profile: Option<&str>,
        auto_save: bool,
    ) -> yagra_base::config_backup::BackupTarget {
        yagra_base::config_backup::BackupTarget {
            node_id: Uuid::new_v4(),
            address: "192.0.2.10".parse().expect("address"),
            profile_name: profile.map(str::to_owned),
            config: yagra_base::config_backup::NodeConfigBackup {
                enabled: true,
                login_order: None,
                input: yagra_base::config_backup::BackupInput::Ssh,
                model_override: model.map(str::to_owned),
                scan_mode: yagra_base::config_backup::ScanMode::Normal,
            },
            rotation: yagra_base::config_backup::Rotation::default(),
            address_changed: false,
            auto_save,
        }
    }

    /// Only a node switched on whose model has no save command is switched off; one that can be
    /// saved, and one already off, are left alone.
    #[test]
    fn automatic_save_is_switched_off_only_where_the_model_cannot_be_saved() {
        let vrp = target(Some("vrp"), None, true);
        let asa = target(Some("asa"), None, true);
        let no_model = target(None, None, true);
        let asa_off = target(Some("asa"), None, false);
        let got = unsaveable_auto_saves(&[vrp, asa.clone(), no_model.clone(), asa_off]);
        assert_eq!(got, vec![asa.node_id, no_model.node_id]);
    }

    #[test]
    fn a_pending_save_is_run_and_otherwise_only_a_comparable_model_is_checked() {
        let id = Uuid::new_v4();
        for auto in [false, true] {
            assert_eq!(decide("vrp", Some(id), auto), Some(Action::Save(id)));
            assert_eq!(decide("ios", Some(id), auto), Some(Action::Save(id)));
            assert_eq!(decide("ios", None, auto), None);
            assert_eq!(decide("junos", Some(id), auto), None);
        }
        assert_eq!(decide("vrp", None, false), Some(Action::Check));
        assert_eq!(decide("vrp", None, true), Some(Action::SaveIfUnsaved));
    }
}
