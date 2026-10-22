// SPDX-License-Identifier: AGPL-3.0-only
//! The leader loop between Yagra and the bundled oxidized (ADR-210 stage 2 decision 12).
//!
//! Every tick it asks oxidized to read the node list again — oxidized reads its source only at
//! start and on `/reload`, so an address, a login or a new node reaches it no later than one tick —
//! and copies what oxidized last reported for each node into `config_backup_status`.
//!
//! Leader-only because two cores would write the same rows and send oxidized two reloads; both are
//! harmless, but there is no reason to have them. An oxidized that does not answer leaves the last
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

/// One tick: reload, then copy the statuses.
async fn tick(client: &OxidizedClient, repo: &ConfigBackupRepo) {
    if let Err(e) = client.reload().await {
        tracing::warn!(error = %e, "config backup: oxidized did not reload its node list");
        metrics::counter!("yagra_oxidized_errors_total", "op" => "reload").increment(1);
        return;
    }
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
}

/// Runs until cancelled.
pub(crate) async fn run_status_loop(client: Arc<OxidizedClient>, repo: Arc<ConfigBackupRepo>) {
    let mut every = tokio::time::interval(TICK);
    every.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        every.tick().await;
        tick(&client, &repo).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
