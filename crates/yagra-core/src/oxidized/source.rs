// SPDX-License-Identifier: AGPL-3.0-only
//! What each configured node is handed to oxidized as, and why a node is not handed over
//! (ADR-210 stage 2 decisions 12-14).
//!
//! The judgement is [`readiness`], which is pure: the screen asks it to say why a node is not
//! fetched, and the source asks it to decide what to send. The two cannot disagree because they
//! are the same function.

use serde::Serialize;
use uuid::Uuid;
use yagra_base::config_backup::{BackupInput, BackupTarget, NodeConfigBackup};
use yagra_base::secrets::{CliLoginSecret, CredentialStore, KIND_CLI_LOGIN};

/// Whether a node is fetched, and if not, the one thing to fix.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Readiness {
    /// Handed to oxidized.
    Ready,
    /// Nobody switched it on.
    Off,
    /// Its profile maps to no oxidized model and nobody picked one.
    NoModel,
    /// No login is chosen, or the chosen one was deleted.
    NoCredential,
    /// The chosen credential is not a CLI login.
    NotCliLogin,
    /// The stored input is a token a newer core wrote.
    UnknownInput,
    /// The node's address changed since the configuration was saved, so its login is withheld
    /// until someone allowed to choose a login saves it again.
    AddressChanged,
}

/// The model a node is fetched with: the person's pick, else its profile's row.
#[must_use]
pub(crate) fn model_of(model_override: Option<&str>, profile_name: Option<&str>) -> Option<String> {
    match model_override {
        Some(m) => Some(m.to_owned()),
        None => profile_name
            .and_then(yagra_common::oxidized_model::model_for_profile)
            .map(str::to_owned),
    }
}

/// Whether `cfg` is fetched, given the node's profile and the kind of its chosen credential
/// (`None` when it has none, or the credential no longer exists).
#[must_use]
pub(crate) fn readiness(
    cfg: Option<&NodeConfigBackup>,
    profile_name: Option<&str>,
    credential_kind: Option<&str>,
    address_changed: bool,
) -> Readiness {
    let Some(cfg) = cfg.filter(|c| c.enabled) else {
        return Readiness::Off;
    };
    if address_changed {
        return Readiness::AddressChanged;
    }
    if cfg.input == BackupInput::Unknown {
        return Readiness::UnknownInput;
    }
    if model_of(cfg.model_override.as_deref(), profile_name).is_none() {
        return Readiness::NoModel;
    }
    match credential_kind {
        None => Readiness::NoCredential,
        Some(KIND_CLI_LOGIN) => Readiness::Ready,
        Some(_) => Readiness::NotCliLogin,
    }
}

/// One node as oxidized's HTTP source reads it. The field names are the keys the generated
/// configuration's `map` names (`config::render`).
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub(crate) struct SourceNode {
    /// The node id: oxidized's name for it, and the file it is stored under.
    pub name: String,
    pub ip: String,
    pub model: String,
    pub input: &'static str,
    pub group: &'static str,
    pub username: String,
    pub password: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enable: Option<String>,
}

/// Every configured node that is ready, as oxidized's source. A node that is not ready is left
/// out; one whose credential fails to open is left out and logged by id, never by content.
pub(crate) async fn source_nodes(
    targets: Vec<BackupTarget>,
    creds: &CredentialStore,
) -> Vec<SourceNode> {
    let mut out = Vec::with_capacity(targets.len());
    for t in targets {
        let Some(cred) = t.credential_id else {
            continue;
        };
        let opened = match creds.open(cred).await {
            Ok(Some(o)) => o,
            Ok(None) => continue,
            Err(e) => {
                tracing::warn!(node = %t.node_id, error = %e, "config backup: credential decrypt failed");
                continue;
            }
        };
        let cfg = NodeConfigBackup {
            enabled: true,
            credential_id: Some(cred),
            input: t.input,
            model_override: t.model_override.clone(),
        };
        let ready = readiness(
            Some(&cfg),
            t.profile_name.as_deref(),
            Some(&opened.0),
            t.address_changed,
        );
        if ready != Readiness::Ready {
            continue;
        }
        let Ok(login) = CliLoginSecret::parse(&opened.1) else {
            tracing::warn!(node = %t.node_id, "config backup: the CLI login does not parse");
            continue;
        };
        let (Some(model), Some(input)) = (
            model_of(t.model_override.as_deref(), t.profile_name.as_deref()),
            t.input.oxidized(),
        ) else {
            continue;
        };
        out.push(SourceNode {
            name: t.node_id.to_string(),
            ip: t.address.to_string(),
            model,
            input,
            group: super::GROUP,
            username: login.username,
            password: login.password,
            enable: login.enable,
        });
    }
    out
}

/// The node an oxidized name refers to, when it is one of Yagra's.
#[must_use]
pub(crate) fn node_of(name: &str, group: Option<&str>) -> Option<Uuid> {
    if group.is_some_and(|g| g != super::GROUP) {
        return None;
    }
    name.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// [`readiness`] for a node whose address has not moved since it was saved.
    fn ready_now(
        cfg: Option<&NodeConfigBackup>,
        profile: Option<&str>,
        kind: Option<&str>,
    ) -> Readiness {
        readiness(cfg, profile, kind, false)
    }

    fn cfg(enabled: bool, input: BackupInput, model: Option<&str>) -> NodeConfigBackup {
        NodeConfigBackup {
            enabled,
            credential_id: None,
            input,
            model_override: model.map(str::to_owned),
        }
    }

    #[test]
    fn readiness_names_the_first_thing_to_fix() {
        let ios = Some("Cisco Catalyst switch (IOS/IOS-XE)");
        let on = cfg(true, BackupInput::Ssh, None);
        assert_eq!(ready_now(None, ios, Some(KIND_CLI_LOGIN)), Readiness::Off);
        assert_eq!(
            ready_now(
                Some(&cfg(false, BackupInput::Ssh, None)),
                ios,
                Some(KIND_CLI_LOGIN)
            ),
            Readiness::Off
        );
        assert_eq!(
            ready_now(
                Some(&cfg(true, BackupInput::Unknown, None)),
                ios,
                Some(KIND_CLI_LOGIN)
            ),
            Readiness::UnknownInput
        );
        assert_eq!(
            ready_now(
                Some(&on),
                Some("Cisco wireless controller"),
                Some(KIND_CLI_LOGIN)
            ),
            Readiness::NoModel
        );
        assert_eq!(ready_now(Some(&on), ios, None), Readiness::NoCredential);
        assert_eq!(
            ready_now(Some(&on), ios, Some("snmp_v2c")),
            Readiness::NotCliLogin
        );
        assert_eq!(
            ready_now(Some(&on), ios, Some(KIND_CLI_LOGIN)),
            Readiness::Ready
        );
        // A person's pick stands in for a profile with no row.
        let picked = cfg(true, BackupInput::Telnet, Some("aireos"));
        assert_eq!(
            ready_now(
                Some(&picked),
                Some("Cisco wireless controller"),
                Some(KIND_CLI_LOGIN)
            ),
            Readiness::Ready
        );
    }

    #[test]
    fn a_moved_node_has_its_login_withheld() {
        let on = cfg(true, BackupInput::Ssh, Some("ios"));
        assert_eq!(
            readiness(Some(&on), None, Some(KIND_CLI_LOGIN), true),
            Readiness::AddressChanged
        );
        // Switched off, it is simply off.
        let off = cfg(false, BackupInput::Ssh, Some("ios"));
        assert_eq!(
            readiness(Some(&off), None, Some(KIND_CLI_LOGIN), true),
            Readiness::Off
        );
    }

    #[test]
    fn only_a_uuid_in_yagras_group_is_a_node() {
        let id = Uuid::new_v4();
        assert_eq!(node_of(&id.to_string(), Some("yagra")), Some(id));
        assert_eq!(node_of(&id.to_string(), None), Some(id));
        assert_eq!(node_of(&id.to_string(), Some("default")), None);
        assert_eq!(node_of("sw-01", Some("yagra")), None);
    }
}
