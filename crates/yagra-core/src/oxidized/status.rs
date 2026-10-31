// SPDX-License-Identifier: AGPL-3.0-only
//! The leader loop between Yagra and the bundled oxidized (ADR-210 stage 2 decision 12).
//!
//! Every tick it asks oxidized to read the node list again — oxidized reads its source only at
//! start and on `/reload` — unless a fetch is out, when the reload waits for up to two ticks
//! ([`MAX_SKIPPED_RELOADS`], and `rotation`'s module doc for why it waits at all). So an address, a
//! login or a new node reaches oxidized within three ticks. It also copies what oxidized last
//! reported for each node into `config_backup_status`.
//!
//! Leader-only because two cores would write the same rows and each send oxidized a reload — and a
//! reload while a fetch is out can start that fetch a second time. An oxidized that does not answer leaves the last
//! recorded status in place: a screen then shows the last known result with its time, never a
//! fresh "failed" invented from silence.

use std::sync::Arc;
use std::time::Duration;

use yagra_base::config_backup::ConfigBackupRepo;

use super::client::{parse_time, OxNode, OxidizedClient};
use super::source::node_of;

/// How often the node list is reloaded and the status copied.
const TICK: Duration = Duration::from_secs(300);

/// The rows to record from one `/nodes.json` answer: Yagra's own nodes only.
#[must_use]
pub(crate) fn status_rows(
    nodes: &[OxNode],
) -> Vec<(uuid::Uuid, String, Option<chrono::DateTime<chrono::Utc>>)> {
    nodes
        .iter()
        .filter_map(|n| {
            let id = node_of(&n.name, n.group.as_deref())?;
            let status = n.status.clone().unwrap_or_else(|| "never".to_owned());
            let end = n
                .time
                .as_ref()
                .and_then(serde_json::Value::as_str)
                .and_then(parse_time);
            Some((id, status, end))
        })
        .collect()
}

/// How many ticks in a row may skip their reload because a fetch is out. A node switched off or a
/// login taken out reaches oxidized only on a reload, so a run of fetches (someone pressing Fetch
/// now again and again) must not hold it back for ever: after this many skipped ticks the next one
/// reloads anyway, accepting that a fetch out at that moment may run a second time.
const MAX_SKIPPED_RELOADS: u32 = 2;

/// Whether this tick reloads: when no fetch is out, or when too many ticks in a row have not.
#[must_use]
pub(crate) const fn reload_now(may_reload: bool, skipped: u32) -> bool {
    may_reload || skipped >= MAX_SKIPPED_RELOADS
}

/// One tick: reload — unless a fetch is out, which a reload would start a second time
/// (`rotation::may_reload`), and not for longer than [`MAX_SKIPPED_RELOADS`] ticks — then copy the
/// statuses. Returns how many ticks in a row have now skipped their reload.
async fn tick(client: &OxidizedClient, repo: &ConfigBackupRepo, skipped: u32) -> u32 {
    let may = match super::rotation::may_reload(repo, chrono::Utc::now()).await {
        Ok(may) => may,
        Err(e) => {
            tracing::warn!(error = %e, "config backup: could not tell whether a fetch is out");
            false
        }
    };
    let skipped = if reload_now(may, skipped) {
        if let Err(e) = client.reload().await {
            tracing::warn!(error = %e, "config backup: oxidized did not reload its node list");
            metrics::counter!("yagra_oxidized_errors_total", "op" => "reload").increment(1);
            return skipped;
        }
        0
    } else {
        skipped + 1
    };
    match client.nodes().await {
        Ok(nodes) => {
            if let Err(e) = repo.record_statuses(&status_rows(&nodes)).await {
                tracing::warn!(error = %e, "config backup: failed to record oxidized's statuses");
            }
        }
        Err(e) => {
            tracing::warn!(error = %e, "config backup: oxidized's node list could not be read");
            metrics::counter!("yagra_oxidized_errors_total", "op" => "nodes").increment(1);
        }
    }
    skipped
}

/// Runs until cancelled.
///
/// Each tick also gives up saves that waited or ran past their deadline (ADR-222), so a save
/// whose fetch never succeeded reads as failed rather than pending for ever.
pub(crate) async fn run_status_loop(
    client: Arc<OxidizedClient>,
    repo: Arc<ConfigBackupRepo>,
    saves: Arc<yagra_base::config_save::ConfigSaveRepo>,
) {
    let mut every = tokio::time::interval(TICK);
    every.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut skipped = 0;
    loop {
        every.tick().await;
        skipped = tick(&client, &repo, skipped).await;
        if let Err(e) = saves.expire().await {
            tracing::warn!(error = %e, "config save: overdue saves could not be given up");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A reload waits for fetches, but not for ever: a change that takes a node or a login away
    /// reaches oxidized within three ticks however busy it is.
    #[test]
    fn a_reload_held_back_by_fetches_happens_by_the_third_tick() {
        assert!(reload_now(true, 0));
        assert!(!reload_now(false, 0));
        assert!(!reload_now(false, 1));
        assert!(reload_now(false, MAX_SKIPPED_RELOADS));
    }

    #[test]
    fn only_yagras_nodes_are_recorded_and_never_has_no_time() {
        let id = uuid::Uuid::new_v4();
        let nodes = vec![
            OxNode {
                name: id.to_string(),
                group: Some("yagra".to_owned()),
                status: Some("success".to_owned()),
                time: Some(serde_json::json!("2026-10-22 10:00:00 UTC")),
            },
            OxNode {
                name: "hand-added".to_owned(),
                group: Some("default".to_owned()),
                status: Some("fail".to_owned()),
                time: None,
            },
        ];
        let rows = status_rows(&nodes);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].0, id);
        assert!(rows[0].2.is_some());

        let never = vec![OxNode {
            name: id.to_string(),
            group: Some("yagra".to_owned()),
            status: Some("never".to_owned()),
            time: Some(serde_json::json!("never")),
        }];
        assert_eq!(status_rows(&never)[0].2, None);
    }
}
