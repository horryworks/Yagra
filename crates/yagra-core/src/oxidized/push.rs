// SPDX-License-Identifier: AGPL-3.0-only
//! The config-backup destination's alert (ADR-210 stage 4 decision 33): the leader reads how the
//! last pushes went, once a minute, and opens or closes the one alert that says config backups are
//! not leaving the box.
//!
//! The pushes themselves are the push script's, inside oxidized (`config::PUSH_SCRIPT_FILE`), and
//! what each one reported is in `config_backup_remote` — written by whichever core oxidized reached.
//! Deciding from the row rather than from the report keeps the decision on the leader, where every
//! other alert source raises, and lets a restart or a failover pick up an open alert from the same
//! row instead of from a report that was already answered.
//!
//! Leader-only: two cores raising the same alert would both page.

use std::sync::Arc;
use std::time::Duration;

use yagra_base::config_remote::{alert_due, RemoteRepo};

use crate::alerts::AlertManager;
use yagra_alert::action::AlertSink;

/// How often the leader reads the destination's row.
pub(crate) const WATCH_TICK: Duration = Duration::from_secs(60);

/// What one tick does: open the alert while [`alert_due`] says so, close it otherwise. A read that
/// fails changes nothing — an absent answer is not evidence that pushes recovered.
pub(crate) async fn tick(remote: &RemoteRepo, alerts: &AlertManager, sink: &dyn AlertSink) {
    let row = match remote.get().await {
        Ok(row) => row,
        Err(e) => {
            tracing::warn!(error = %e, "config backup: could not read the Git destination");
            return;
        }
    };
    let action = match alert_due(row.as_ref()) {
        Some(failures) => {
            alerts.raise_config_push_alert(failures, chrono::Utc::now().timestamp_millis())
        }
        None => alerts.resolve_config_push_alert(),
    };
    if let Some(action) = action {
        sink.dispatch(action).await;
    }
}

/// The leader loop.
pub(crate) async fn run_push_alert_watch(
    remote: Arc<RemoteRepo>,
    alerts: Arc<AlertManager>,
    sink: Arc<dyn AlertSink>,
) {
    loop {
        tokio::time::sleep(WATCH_TICK).await;
        tick(&remote, &alerts, sink.as_ref()).await;
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;
    use yagra_alert::action::NotifyAction;
    use yagra_base::config_remote::{GitAuth, PushFailure, Pushed, RemoteSettings};

    const PUSHED: Pushed<'static> = Pushed {
        head: "a1",
        layout: "L",
        commit: "c1",
    };

    #[derive(Default)]
    struct Captured(Mutex<Vec<NotifyAction>>);

    #[async_trait::async_trait]
    impl AlertSink for Captured {
        async fn dispatch(&self, action: NotifyAction) {
            self.0
                .lock()
                .expect("no panic holds this lock")
                .push(action);
        }
    }

    impl Captured {
        fn take(&self) -> Vec<&'static str> {
            self.0
                .lock()
                .expect("no panic holds this lock")
                .drain(..)
                .map(|a| match a {
                    NotifyAction::Fire(_) => "fire",
                    NotifyAction::Resolve(_) => "resolve",
                    NotifyAction::Suppress(_) => "suppress",
                })
                .collect()
        }
    }

    /// A refused login opens the alert on its first failure; a destination that did not answer
    /// opens it on the second; a push that goes through closes it, and so does removing the
    /// destination.
    #[sqlx::test(migrator = "yagra_base::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn the_alert_follows_the_pushes(pool: sqlx::PgPool) {
        let remote = RemoteRepo::new(pool.clone());
        let alerts =
            crate::alerts::manager_with(yagra_common::poll_interval::PollIntervals::default());
        let sink = Captured::default();
        let login = yagra_base::pgtest::credential(&pool, "git", "git_remote").await;
        remote
            .put(&RemoteSettings {
                url: "https://example.com/acme/configs.git".to_owned(),
                branch: "main".to_owned(),
                auth: GitAuth::HttpsToken,
                credential_id: login,
                ssh_public_key: None,
            })
            .await
            .expect("put");
        tick(&remote, &alerts, &sink).await;
        assert!(sink.take().is_empty(), "nothing has failed");

        remote
            .record_push(1, None, Some(PushFailure::Unreachable), None)
            .await
            .unwrap();
        tick(&remote, &alerts, &sink).await;
        assert!(sink.take().is_empty(), "one dropped push is not an alert");
        remote
            .record_push(1, None, Some(PushFailure::Unreachable), None)
            .await
            .unwrap();
        tick(&remote, &alerts, &sink).await;
        assert_eq!(sink.take(), ["fire"]);
        tick(&remote, &alerts, &sink).await;
        assert!(sink.take().is_empty(), "open once");

        remote
            .record_push(1, Some(&PUSHED), None, None)
            .await
            .unwrap();
        tick(&remote, &alerts, &sink).await;
        assert_eq!(sink.take(), ["resolve"]);

        remote
            .record_push(1, None, Some(PushFailure::Auth), None)
            .await
            .unwrap();
        tick(&remote, &alerts, &sink).await;
        assert_eq!(
            sink.take(),
            ["fire"],
            "a refused login is worth an alert at once"
        );
        remote.delete().await.unwrap();
        tick(&remote, &alerts, &sink).await;
        assert_eq!(sink.take(), ["resolve"]);
    }
}
