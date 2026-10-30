// SPDX-License-Identifier: AGPL-3.0-only
//! The bundled oxidized (ADR-210 stage 2): Yagra decides which devices are fetched and with which
//! login; oxidized logs in over SSH or telnet, runs the model's commands and keeps every version in
//! Git. Nothing here speaks to a device.
//!
//! - [`config`] writes oxidized's configuration file and the source token, from the
//!   `oxidized-config` one-shot container, before oxidized starts.
//! - [`source`] decides what each configured node is handed to oxidized as — model, input, login —
//!   and why one is not handed over. `GET /internal/oxidized/nodes` serves it.
//! - [`client`] reads oxidized's REST API: the node list, the versions, a version's text.
//! - [`status`] is the leader loop that asks oxidized to re-read the node list and copies what it
//!   last reported into `config_backup_status`.
//! - [`rotation`] tries each node's CLI logins in turn and remembers the one that works, within
//!   the lockout limits (stage 3): the pure choice, and the leader loop that acts on it.
//! - [`schedule`] decides when the nodes whose login works are fetched: every day from one UTC
//!   time, so many at a time (stage 6). oxidized fetches nothing on its own.
//!
//! - [`relay`] carries oxidized's login sessions to the poller assigned to each device (stage 5):
//!   oxidized connects to core, never to a device.
//!
//! - [`push`] is the leader loop that opens and closes the alert saying config backups are not
//!   reaching their Git destination (stage 4). The pushes are the push script's, inside oxidized.
//!
//! ⚠️ oxidized's REST API has no authentication. It is reachable only on the compose network,
//! which holds nothing but this deployment's own containers; that is the whole protection.

pub(crate) mod client;
pub(crate) mod config;
pub(crate) mod push;
pub(crate) mod relay;
pub(crate) mod rotation;
pub(crate) mod schedule;
pub(crate) mod source;
pub(crate) mod status;

use std::path::PathBuf;

/// Where core reaches oxidized's REST API. Unset or empty turns config backup off.
pub(crate) const URL_ENV: &str = "YAGRA_OXIDIZED_URL";
/// The directory oxidized's configuration and the source token live in, shared with the
/// oxidized container.
pub(crate) const HOME_ENV: &str = "YAGRA_OXIDIZED_HOME";

/// The oxidized group every Yagra node is filed under. A node's oxidized name is its id.
pub(crate) const GROUP: &str = "yagra";

/// What this core needs to reach the bundled oxidized.
#[derive(Debug, Clone)]
pub(crate) struct Settings {
    pub url: String,
    pub home: PathBuf,
}

impl Settings {
    /// Read from the environment; `None` when either is unset, which leaves config backup off.
    pub(crate) fn from_env() -> Option<Self> {
        let url = std::env::var(URL_ENV)
            .ok()
            .filter(|v| !v.trim().is_empty())?;
        let home = std::env::var(HOME_ENV)
            .ok()
            .filter(|v| !v.trim().is_empty())?;
        Some(Self {
            url: url.trim().trim_end_matches('/').to_owned(),
            home: PathBuf::from(home),
        })
    }
}
