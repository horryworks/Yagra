// SPDX-License-Identifier: AGPL-3.0-only
//! Core HA leader election (ADR-016).
//!
//! A PostgreSQL **session-level advisory lock** elects a single active core. Only the lock holder
//! runs the coordinator + ingest + alert/notify singletons (see `run_live`); standbys serve
//! `/healthz` (and `503 /readyz`) and wait. The lock is held on a **dedicated connection** — opened
//! here, outside the shared pool — for the process lifetime; when that connection drops (process
//! exit, crash, or a lost link to PostgreSQL) the server auto-releases the lock so a standby can
//! take over. Opt-in via `YAGRA_ENABLE_HA`; when off, `run_live` never enters this module.
//!
//! Why advisory locks (ADR-016): no extra dependency, one clear owner, semantics ("held until the
//! session ends") that match "leader until this process dies". The dedicated connection is +1
//! outside `YAGRA_PG_MAX_CONNECTIONS`, so size PostgreSQL `max_connections` accordingly.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use futures::future::BoxFuture;
use sqlx::{Connection, PgConnection};
use yagra_telemetry::CancellationToken;

/// Advisory-lock key every core contends for against the same database. A fixed constant so all
/// instances race for the *same* lock; the value is arbitrary but stable (ASCII "YAGRLEAD", which
/// stays within the positive `i64`/`bigint` range PostgreSQL expects).
const LEADER_LOCK_KEY: i64 = 0x5941_4752_4c45_4144;

// A positive key avoids sign-confusion across drivers when passed to `pg_try_advisory_lock(bigint)`.
const _: () = assert!(LEADER_LOCK_KEY > 0);

/// How often a standby retries `pg_try_advisory_lock` while waiting to become leader. Also the rough
/// upper bound on failover latency (retry cadence + the leader's connection-loss detection).
const ELECTION_POLL: Duration = Duration::from_secs(5);

/// How often the leader pings its held lock connection to notice a lost link to PostgreSQL.
const LIVENESS_PING: Duration = Duration::from_secs(5);

/// One advisory-lock attempt, abstracted so the election loop is unit-testable without PostgreSQL.
/// `Ok(true)` = lock won, `Ok(false)` = another core holds it, `Err` = connection problem (retry).
trait Probe: Send {
    fn try_acquire(&mut self) -> BoxFuture<'_, Result<bool, sqlx::Error>>;
}

/// Acquire the leader advisory lock, polling until won or `shutdown` fires. Opens and holds a
/// **dedicated** connection (not from the shared pool) — dropping the returned connection releases
/// the lock. Returns `Some(conn)` when this core becomes the leader (keep `conn` alive to hold the
/// lock), or `None` when shutdown was requested before leadership was won (standby exiting).
pub async fn acquire(database_url: &str, shutdown: &CancellationToken) -> Option<PgConnection> {
    let mut probe = PgProbe {
        database_url,
        conn: None,
    };
    if run_election(&mut probe, shutdown, ELECTION_POLL).await {
        // Won: the winning connection is retained inside the probe.
        probe.conn
    } else {
        None
    }
}

/// Hold leadership: ping the lock connection until it dies (lost link ⇒ the lock is released and a
/// standby may take over) or `shutdown` fires. Returns on either; the caller decides whether that
/// means "shut down for restart" (connection lost) or "clean exit" (shutdown).
pub async fn hold_until_lost(mut conn: PgConnection, shutdown: &CancellationToken) {
    loop {
        tokio::select! {
            _ = shutdown.cancelled() => return,
            _ = tokio::time::sleep(LIVENESS_PING) => {
                if let Err(e) = conn.ping().await {
                    tracing::warn!(error = %e, "leader lock connection lost");
                    return;
                }
            }
        }
    }
}

/// Poll `probe` until it wins the lock (`true`) or `shutdown` fires. Extracted from [`acquire`] so
/// the wait/win/shutdown control flow is testable with a fake probe. Returns `true` if leadership
/// was won, `false` if shutdown interrupted the wait.
async fn run_election(probe: &mut dyn Probe, shutdown: &CancellationToken, poll: Duration) -> bool {
    loop {
        if shutdown.is_cancelled() {
            return false;
        }
        match probe.try_acquire().await {
            Ok(true) => return true,
            Ok(false) => tracing::debug!("standby — another core holds leadership; waiting"),
            Err(e) => tracing::warn!(error = %e, "advisory-lock probe failed; will retry"),
        }
        tokio::select! {
            _ = shutdown.cancelled() => return false,
            _ = tokio::time::sleep(poll) => {}
        }
    }
}

/// The real probe: lazily opens a dedicated connection and runs `pg_try_advisory_lock`. A lost
/// connection is dropped so the next attempt reconnects (a standby must survive a PostgreSQL blip).
struct PgProbe<'a> {
    database_url: &'a str,
    conn: Option<PgConnection>,
}

impl Probe for PgProbe<'_> {
    fn try_acquire(&mut self) -> BoxFuture<'_, Result<bool, sqlx::Error>> {
        Box::pin(async move {
            if self.conn.is_none() {
                self.conn = Some(PgConnection::connect(self.database_url).await?);
            }
            let conn = self.conn.as_mut().expect("connection set above");
            match sqlx::query_scalar::<_, bool>("SELECT pg_try_advisory_lock($1)")
                .bind(LEADER_LOCK_KEY)
                .fetch_one(&mut *conn)
                .await
            {
                Ok(got) => Ok(got),
                Err(e) => {
                    // Drop the dead connection so the next round reconnects.
                    self.conn = None;
                    Err(e)
                }
            }
        })
    }
}

/// Run the leader-only work — now, or the moment this core wins the election.
///
/// Moved out of `run_live` by ADR-090. Choosing *when* the singletons start is this module's
/// subject, and it was the last background task the wiring spawned for itself.
///
/// With HA off (the default) `work` is awaited inline before the caller serves — byte-identical to
/// pre-HA. With HA on the caller serves immediately and this stands by: the API (including
/// `/healthz`) is already up, so the container stays healthy while waiting, and promotion is live
/// (no restart). A lost lock connection = graceful shutdown for orchestrator restart (spawn-once,
/// ADR-016 model B).
pub(crate) async fn run_leader_work<F>(
    enable_ha: bool,
    database_url: &str,
    core_id: Option<&str>,
    is_leader: std::sync::Arc<AtomicBool>,
    shutdown: &CancellationToken,
    work: F,
) -> anyhow::Result<()>
where
    F: std::future::Future<Output = anyhow::Result<()>> + Send + 'static,
{
    if !enable_ha {
        // Single active core: run all leader work inline before serving — byte-identical to pre-HA.
        return work.await;
    }
    let shutdown_lease = shutdown.clone();
    let db_url = database_url.to_owned();
    let core_label = core_id.map_or_else(|| "core".to_owned(), str::to_owned);
    tokio::spawn(async move {
        tracing::info!(core = %core_label, "HA enabled — standby, waiting for leadership");
        let Some(conn) = acquire(&db_url, &shutdown_lease).await else {
            return; // shutdown fired before leadership was won
        };
        is_leader.store(true, Ordering::Release);
        metrics::gauge!("yagra_core_is_leader").set(1.0);
        tracing::info!(core = %core_label, "acquired leadership — starting coordinator and background workers");
        if let Err(e) = work.await {
            tracing::error!(error = %e, "leader startup failed — shutting down for restart");
            shutdown_lease.cancel();
            return;
        }
        hold_until_lost(conn, &shutdown_lease).await;
        if !shutdown_lease.is_cancelled() {
            is_leader.store(false, Ordering::Release);
            metrics::gauge!("yagra_core_is_leader").set(0.0);
            tracing::error!(
                "lost leadership (PostgreSQL connection lost) — shutting down for restart"
            );
            shutdown_lease.cancel();
        }
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Scripted probe: yields queued outcomes (`Some(bool)` = result, `None` = simulate a
    /// connection error), counting calls. Runs out ⇒ `Ok(false)` (keep waiting).
    struct FakeProbe {
        results: Vec<Option<bool>>,
        calls: usize,
        // Optional side effect: cancel this token on the Nth call, to exercise the wait-cancel path.
        cancel_after: Option<(usize, CancellationToken)>,
    }

    impl Probe for FakeProbe {
        fn try_acquire(&mut self) -> BoxFuture<'_, Result<bool, sqlx::Error>> {
            let idx = self.calls;
            self.calls += 1;
            if let Some((n, token)) = &self.cancel_after {
                if self.calls == *n {
                    token.cancel();
                }
            }
            let outcome = self.results.get(idx).copied().unwrap_or(Some(false));
            Box::pin(async move {
                match outcome {
                    Some(v) => Ok(v),
                    None => Err(sqlx::Error::PoolClosed),
                }
            })
        }
    }

    const FAST: Duration = Duration::from_millis(1);

    #[tokio::test]
    async fn wins_immediately_when_lock_is_free() {
        let mut probe = FakeProbe {
            results: vec![Some(true)],
            calls: 0,
            cancel_after: None,
        };
        let shutdown = CancellationToken::new();
        assert!(run_election(&mut probe, &shutdown, FAST).await);
        assert_eq!(probe.calls, 1);
    }

    #[tokio::test]
    async fn waits_then_wins_after_contention() {
        let mut probe = FakeProbe {
            results: vec![Some(false), Some(false), Some(true)],
            calls: 0,
            cancel_after: None,
        };
        let shutdown = CancellationToken::new();
        assert!(run_election(&mut probe, &shutdown, FAST).await);
        assert_eq!(probe.calls, 3);
    }

    #[tokio::test]
    async fn retries_through_a_connection_error() {
        // A probe error (lost connection) must not abort the wait — it retries and eventually wins.
        let mut probe = FakeProbe {
            results: vec![None, Some(true)],
            calls: 0,
            cancel_after: None,
        };
        let shutdown = CancellationToken::new();
        assert!(run_election(&mut probe, &shutdown, FAST).await);
        assert_eq!(probe.calls, 2);
    }

    #[tokio::test]
    async fn returns_false_when_already_shut_down() {
        let mut probe = FakeProbe {
            results: vec![Some(true)],
            calls: 0,
            cancel_after: None,
        };
        let shutdown = CancellationToken::new();
        shutdown.cancel();
        // Pre-cancelled: never probes, reports "not leader" so the standby can exit.
        assert!(!run_election(&mut probe, &shutdown, FAST).await);
        assert_eq!(probe.calls, 0);
    }

    #[tokio::test]
    async fn returns_false_when_shutdown_arrives_during_the_wait() {
        let shutdown = CancellationToken::new();
        // Always "held elsewhere"; cancel the token on the first attempt so the post-probe
        // `select!` takes the cancellation arm rather than looping forever.
        let mut probe = FakeProbe {
            results: vec![Some(false)],
            calls: 0,
            cancel_after: Some((1, shutdown.clone())),
        };
        assert!(!run_election(&mut probe, &shutdown, Duration::from_secs(3600)).await);
        assert_eq!(probe.calls, 1);
    }
}
