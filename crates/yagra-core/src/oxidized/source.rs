// SPDX-License-Identifier: AGPL-3.0-only
//! What each configured node is handed to oxidized as, and why a node is not handed over
//! (ADR-210 stage 2 decisions 12-14, stage 3 decision 22).
//!
//! The judgement is [`readiness`], which is pure: the screen asks it to say why a node is not
//! fetched, and the source asks it to decide what to send. The two cannot disagree because they
//! are the same function.

use chrono::{DateTime, Utc};
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
    /// There is no CLI login to try: none exists, or the node's own order names none that does.
    NoCredential,
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

/// Whether `cfg` is fetched, given the node's profile and whether it has any CLI login to try
/// (`rotation::candidates`).
#[must_use]
pub(crate) fn readiness(
    cfg: Option<&NodeConfigBackup>,
    profile_name: Option<&str>,
    has_candidates: bool,
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
    if has_candidates {
        Readiness::Ready
    } else {
        Readiness::NoCredential
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

/// The address a parked node is handed to oxidized with: its own loopback, where nothing listens,
/// so a fetch oxidized starts on its own schedule is refused at once and no login leaves it.
pub(crate) const PARKED_IP: &str = "127.0.0.1";
/// The login a parked node carries. Never sent anywhere: the connection to [`PARKED_IP`] is refused
/// before a login is asked for.
const PARKED_LOGIN: &str = "parked";

/// A configured node as oxidized must not fetch it: in the list, so its stored versions can be read
/// back (oxidized answers a node's history only for a node it holds), but at [`PARKED_IP`] with no
/// real login (stage 3 decision 26).
fn parked(name: String, model: String, input: &'static str) -> SourceNode {
    SourceNode {
        name,
        ip: PARKED_IP.to_owned(),
        model,
        input,
        group: super::GROUP,
        username: PARKED_LOGIN.to_owned(),
        password: PARKED_LOGIN.to_owned(),
        enable: None,
    }
}

/// Whether a node's login may be handed to oxidized now: it is known to work, or a fetch Yagra
/// asked for is out — a login nobody has confirmed is tried only when the limits allowed it, never
/// by oxidized's own daily schedule — and the node is still at the address it was saved with.
#[must_use]
pub(crate) fn hands_login(t: &BackupTarget, now: DateTime<Utc>) -> bool {
    !t.address_changed
        && t.rotation.credential_id.is_some()
        && (t.rotation.login_confirmed || super::rotation::in_flight(t.rotation.requested_at, now))
}

/// Every configured node with a model and a connection, as oxidized's source.
///
/// A node whose login may be handed over ([`hands_login`]) gets it and its address. Every other
/// one is [`parked`]: in the list, so its versions stay readable while it looks for a login, and
/// unable to reach the device. A login that fails to open, or is no longer a CLI login, parks the
/// node too, logged by id, never by content.
pub(crate) async fn source_nodes(
    targets: Vec<BackupTarget>,
    creds: &CredentialStore,
    now: DateTime<Utc>,
) -> Vec<SourceNode> {
    let mut out = Vec::with_capacity(targets.len());
    for t in targets {
        let (Some(model), Some(input)) = (
            model_of(
                t.config.model_override.as_deref(),
                t.profile_name.as_deref(),
            ),
            t.config.input.oxidized(),
        ) else {
            continue;
        };
        let name = t.node_id.to_string();
        let login = match t.rotation.credential_id.filter(|_| hands_login(&t, now)) {
            Some(cred) => match creds.open(cred).await {
                Ok(Some((kind, secret))) if kind == KIND_CLI_LOGIN => {
                    let parsed = CliLoginSecret::parse(&secret).ok();
                    if parsed.is_none() {
                        tracing::warn!(node = %t.node_id, "config backup: the CLI login does not parse");
                    }
                    parsed
                }
                Ok(_) => None,
                Err(e) => {
                    tracing::warn!(node = %t.node_id, error = %e, "config backup: credential decrypt failed");
                    None
                }
            },
            None => None,
        };
        out.push(match login {
            Some(login) => SourceNode {
                name,
                ip: t.address.to_string(),
                model,
                input,
                group: super::GROUP,
                username: login.username,
                password: login.password,
                enable: login.enable,
            },
            None => parked(name, model, input),
        });
    }
    out
}

/// The group the placeholder is filed under. Not [`super::GROUP`], so nothing Yagra reads back from
/// oxidized ever names it.
pub(crate) const PLACEHOLDER_GROUP: &str = "yagra-placeholder";

/// What the source answers when no node is switched on.
///
/// 🚨 oxidized refuses to start on an empty node list (`core.rb`: "source returns no usable nodes")
/// and restarts forever; an empty list on a later reload is fine. So the source is never empty: with
/// nothing to fetch it carries one entry no one will see — the container's own loopback, where
/// nothing listens on port 22, so its daily fetch fails at once and costs nothing. Measured on a lab
/// deployment with no node switched on.
#[must_use]
pub(crate) fn placeholder() -> SourceNode {
    SourceNode {
        name: "no-node-switched-on".to_owned(),
        ip: "127.0.0.1".to_owned(),
        model: "linuxgeneric".to_owned(),
        input: "ssh",
        group: PLACEHOLDER_GROUP,
        username: "none".to_owned(),
        password: "none".to_owned(),
        enable: None,
    }
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
        has_candidates: bool,
    ) -> Readiness {
        readiness(cfg, profile, has_candidates, false)
    }

    fn cfg(enabled: bool, input: BackupInput, model: Option<&str>) -> NodeConfigBackup {
        NodeConfigBackup {
            enabled,
            login_order: None,
            input,
            model_override: model.map(str::to_owned),
        }
    }

    #[test]
    fn readiness_names_the_first_thing_to_fix() {
        let ios = Some("Cisco Catalyst switch (IOS/IOS-XE)");
        let on = cfg(true, BackupInput::Ssh, None);
        assert_eq!(ready_now(None, ios, true), Readiness::Off);
        assert_eq!(
            ready_now(Some(&cfg(false, BackupInput::Ssh, None)), ios, true),
            Readiness::Off
        );
        assert_eq!(
            ready_now(Some(&cfg(true, BackupInput::Unknown, None)), ios, true),
            Readiness::UnknownInput
        );
        assert_eq!(
            ready_now(Some(&on), Some("Cisco wireless controller"), true),
            Readiness::NoModel
        );
        assert_eq!(ready_now(Some(&on), ios, false), Readiness::NoCredential);
        assert_eq!(ready_now(Some(&on), ios, true), Readiness::Ready);
        // A person's pick stands in for a profile with no row.
        let picked = cfg(true, BackupInput::Telnet, Some("aireos"));
        assert_eq!(
            ready_now(Some(&picked), Some("Cisco wireless controller"), true),
            Readiness::Ready
        );
    }

    #[test]
    fn a_moved_node_has_its_login_withheld() {
        let on = cfg(true, BackupInput::Ssh, Some("ios"));
        assert_eq!(
            readiness(Some(&on), None, true, true),
            Readiness::AddressChanged
        );
        // Switched off, it is simply off.
        let off = cfg(false, BackupInput::Ssh, Some("ios"));
        assert_eq!(readiness(Some(&off), None, true, true), Readiness::Off);
    }

    fn target(confirmed: bool, requested: Option<DateTime<Utc>>, moved: bool) -> BackupTarget {
        BackupTarget {
            node_id: Uuid::from_u128(1),
            address: "192.0.2.10".parse().expect("address"),
            profile_name: None,
            config: cfg(true, BackupInput::Ssh, Some("ios")),
            rotation: yagra_base::config_backup::Rotation {
                credential_id: Some(Uuid::from_u128(2)),
                login_confirmed: confirmed,
                requested_at: requested,
                ..Default::default()
            },
            address_changed: moved,
        }
    }

    /// A login reaches oxidized only while it works or a fetch for it is out, and never for a node
    /// that moved; everything else is parked.
    #[test]
    fn only_a_working_or_requested_login_is_handed_over() {
        let now = Utc::now();
        assert!(hands_login(&target(true, None, false), now));
        assert!(hands_login(&target(false, Some(now), false), now));
        assert!(!hands_login(&target(false, None, false), now));
        assert!(!hands_login(
            &target(false, Some(now - chrono::TimeDelta::hours(1)), false),
            now
        ));
        assert!(!hands_login(&target(true, None, true), now));
        let p = parked("n".to_owned(), "ios".to_owned(), "ssh");
        assert_eq!(p.ip, PARKED_IP);
        assert_eq!(p.group, super::super::GROUP);
        assert_eq!(p.enable, None);
    }

    #[test]
    fn the_placeholder_is_never_read_back_as_a_node() {
        let p = placeholder();
        assert_eq!(node_of(&p.name, Some(p.group)), None);
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
