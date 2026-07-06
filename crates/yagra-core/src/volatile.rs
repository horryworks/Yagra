//! Volatile poller state mirror: Redis (ADR-004/009).
//!
//! Liveness (which pollers are alive) and assignment (which poller owns which node) are
//! **ephemeral** — the in-memory coordinator registry is the source of truth (ADR-009). This
//! module mirrors that state into Redis purely for **observability** and cross-process visibility:
//! `yagra:poller:{id}` (a short-TTL liveness/telemetry doc) and `yagra:assign:{pool}` (the current
//! node→poller map). Redis is rebuildable, so ADR-017 makes its loss non-fatal — **every method
//! here is best-effort**: it logs (once per outage, resetting on recovery) and never returns an
//! error to the caller. A degraded store (no URL, bad URL, or Redis down) is a silent no-op.
//!
//! The module is deliberately thin: it caches one auto-recovering multiplexed connection and
//! re-acquires it after an error. Keys are minted by small helpers so the exact wire format is
//! unit-tested in one place.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;
use uuid::Uuid;

/// Redis key for a poller's volatile liveness/telemetry doc.
fn poller_key(id: &str) -> String {
    format!("yagra:poller:{id}")
}

/// Redis key for a pool's current node→poller assignment hash.
fn assign_key(pool: &str) -> String {
    format!("yagra:assign:{pool}")
}

/// The JSON document mirrored to `yagra:poller:{id}` (TTL-bounded). A snapshot of the poller's
/// last heartbeat plus when core last saw it, so another process (or a human via redis-cli) can
/// read current liveness without core's in-memory registry. Optional telemetry fields default so
/// the doc stays forward-compatible (ADR-017).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PollerLiveDoc {
    /// Pool the poller serves.
    pub pool: String,
    /// Per-process incarnation (fresh UUID each start).
    pub incarnation: Uuid,
    /// Poller build version.
    #[serde(default)]
    pub version: String,
    /// Highest sync sequence the poller has applied.
    #[serde(default)]
    pub last_seq: u64,
    /// Nodes in the poller's working set.
    #[serde(default)]
    pub working_set_nodes: u32,
    /// Specs in the poller's working set.
    #[serde(default)]
    pub working_set_specs: u32,
    /// Polls currently in flight.
    #[serde(default)]
    pub inflight: u32,
    /// Total results the poller has produced since start.
    #[serde(default)]
    pub results_total: u64,
    /// When core last saw this poller (Unix ms, UTC).
    pub seen_unix_ms: i64,
}

/// Best-effort mirror of volatile poller state into Redis. `client == None` ⇒ degraded (every
/// method is a no-op); otherwise a lazily-acquired, auto-recovering multiplexed connection is
/// cached and re-acquired after an error.
pub struct VolatileStore {
    /// `None` in degraded mode (no/blank/bad URL) — all methods short-circuit to no-ops.
    client: Option<redis::Client>,
    /// Cached multiplexed connection; re-created lazily and dropped on error so it self-heals.
    conn: Mutex<Option<redis::aio::MultiplexedConnection>>,
    /// Whether the current outage has already been logged — flips back on recovery so we log once
    /// per outage rather than on every failed call (ADR-017: Redis loss is expected and noisy).
    outage_logged: AtomicBool,
}

impl VolatileStore {
    /// Build the store from `YAGRA_REDIS_URL`. Unset, blank, or an unparseable URL ⇒ degraded
    /// (a one-time warning, then silent no-ops). Never fails.
    // `run_live` builds the store from `Config::redis_url` via `from_optional_url`; this env
    // convenience constructor is kept for callers that resolve the URL themselves.
    #[allow(dead_code)]
    #[must_use]
    pub fn from_env() -> Self {
        let url = std::env::var("YAGRA_REDIS_URL")
            .ok()
            .map(|s| s.trim().to_owned())
            .filter(|s| !s.is_empty());
        Self::from_optional_url(url.as_deref())
    }

    /// Build the store from an already-resolved optional URL (e.g. `Config::redis_url`). `None`
    /// (unset/blank) ⇒ degraded (a one-time info log, then silent no-ops); a bad URL degrades too.
    /// This is the seam `run_live` uses so the live/skeleton config decides the Redis mirror, not a
    /// second env read.
    #[must_use]
    pub fn from_optional_url(url: Option<&str>) -> Self {
        match url {
            Some(url) => Self::from_url(url),
            None => {
                tracing::info!(
                    "Redis URL not set — poller liveness/assignment mirroring disabled \
                     (degraded, best-effort; ADR-017)"
                );
                Self::disabled()
            }
        }
    }

    /// Build from an explicit URL. A bad URL degrades (warn once) rather than failing.
    fn from_url(url: &str) -> Self {
        match redis::Client::open(url) {
            Ok(client) => Self {
                client: Some(client),
                conn: Mutex::new(None),
                outage_logged: AtomicBool::new(false),
            },
            Err(e) => {
                tracing::warn!(error = %e, "invalid YAGRA_REDIS_URL — poller state mirroring disabled (degraded)");
                Self::disabled()
            }
        }
    }

    /// A degraded (no-Redis) store: every method is a no-op. For skeleton mode and tests.
    #[must_use]
    pub fn disabled() -> Self {
        Self {
            client: None,
            conn: Mutex::new(None),
            outage_logged: AtomicBool::new(false),
        }
    }

    /// Mirror one poller's liveness/telemetry doc: `SET yagra:poller:{id} <json> EX <ttl>`.
    pub async fn record_poller(&self, id: &str, doc: &PollerLiveDoc, ttl: Duration) {
        let Some(mut conn) = self.connection().await else {
            return;
        };
        let json = match serde_json::to_string(doc) {
            Ok(j) => j,
            Err(e) => {
                tracing::warn!(error = %e, poller = id, "serializing poller live doc failed");
                return;
            }
        };
        let res: redis::RedisResult<()> = redis::cmd("SET")
            .arg(poller_key(id))
            .arg(json)
            .arg("EX")
            .arg(ttl.as_secs().max(1))
            .query_async(&mut conn)
            .await;
        self.settle(res).await;
    }

    /// Replace a pool's assignment map atomically-ish: `DEL yagra:assign:{pool}` then rebuild it
    /// with `HSET` (node uuid → poller id) inside a MULTI/EXEC pipeline. An empty map just clears
    /// the key (a pool with no live pollers has no assignments).
    pub async fn write_assignments(&self, pool: &str, map: &HashMap<Uuid, String>) {
        let Some(mut conn) = self.connection().await else {
            return;
        };
        let key = assign_key(pool);
        let items: Vec<(String, String)> = map
            .iter()
            .map(|(node, poller)| (node.to_string(), poller.clone()))
            .collect();
        let mut pipe = redis::pipe();
        pipe.atomic();
        pipe.del(&key).ignore();
        if !items.is_empty() {
            pipe.hset_multiple(&key, &items).ignore();
        }
        let res: redis::RedisResult<()> = pipe.query_async(&mut conn).await;
        self.settle(res).await;
    }

    /// Liveness probe: `PING`. `false` in degraded mode or on any error.
    // Consumed by the Pollers/health API (Step 6).
    #[allow(dead_code)]
    pub async fn healthy(&self) -> bool {
        let Some(mut conn) = self.connection().await else {
            return false;
        };
        let res: redis::RedisResult<String> = redis::cmd("PING").query_async(&mut conn).await;
        let ok = res.is_ok();
        self.settle(res).await;
        ok
    }

    /// A cloned multiplexed connection, lazily acquiring (and caching) one. `None` in degraded mode
    /// or when the connection can't be established (logged once per outage).
    async fn connection(&self) -> Option<redis::aio::MultiplexedConnection> {
        let client = self.client.as_ref()?;
        let mut guard = self.conn.lock().await;
        if guard.is_none() {
            match client.get_multiplexed_tokio_connection().await {
                Ok(c) => *guard = Some(c),
                Err(e) => {
                    drop(guard);
                    self.note_outage(&e).await;
                    return None;
                }
            }
        }
        guard.clone()
    }

    /// Fold a command result into the outage flag: reset (and log recovery) on success; log once
    /// and drop the cached connection on failure. Never surfaces the error.
    async fn settle<T>(&self, res: redis::RedisResult<T>) {
        match res {
            Ok(_) => {
                if self.outage_logged.swap(false, Ordering::Relaxed) {
                    tracing::info!("Redis recovered — poller state mirroring resumed");
                }
            }
            Err(e) => self.note_outage(&e).await,
        }
    }

    /// Log an outage at most once (until the next success) and invalidate the cached connection so
    /// the next call reconnects.
    async fn note_outage(&self, e: &redis::RedisError) {
        if !self.outage_logged.swap(true, Ordering::Relaxed) {
            tracing::warn!(error = %e, "Redis unavailable — poller state mirroring degraded to no-op (best-effort, ADR-017)");
        }
        *self.conn.lock().await = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_doc() -> PollerLiveDoc {
        PollerLiveDoc {
            pool: "default".into(),
            incarnation: Uuid::nil(),
            version: "0.1.2".into(),
            last_seq: 7,
            working_set_nodes: 3,
            working_set_specs: 5,
            inflight: 1,
            results_total: 42,
            seen_unix_ms: 1_700_000_000_000,
        }
    }

    #[test]
    fn poller_key_format_is_stable() {
        assert_eq!(poller_key("edge-1"), "yagra:poller:edge-1");
        // FQDN dots survive verbatim (id sanitization is the caller's job, not the key's).
        assert_eq!(
            poller_key("poller.lab.example"),
            "yagra:poller:poller.lab.example"
        );
    }

    #[test]
    fn assign_key_format_is_stable() {
        assert_eq!(assign_key("default"), "yagra:assign:default");
        assert_eq!(assign_key("lab"), "yagra:assign:lab");
    }

    #[test]
    fn live_doc_round_trips_through_json() {
        let doc = sample_doc();
        let json = serde_json::to_string(&doc).unwrap();
        let back: PollerLiveDoc = serde_json::from_str(&json).unwrap();
        assert_eq!(doc, back);
    }

    #[test]
    fn live_doc_defaults_optional_telemetry_fields() {
        // A minimal producer (only the two required fields) still deserializes; telemetry
        // defaults to zero/empty (ADR-017 forward-compat).
        let json = r#"{
            "pool": "default",
            "incarnation": "00000000-0000-0000-0000-000000000000",
            "seen_unix_ms": 123
        }"#;
        let doc: PollerLiveDoc = serde_json::from_str(json).unwrap();
        assert_eq!(doc.pool, "default");
        assert_eq!(doc.seen_unix_ms, 123);
        assert!(doc.version.is_empty());
        assert_eq!(doc.last_seq, 0);
        assert_eq!(doc.working_set_nodes, 0);
        assert_eq!(doc.results_total, 0);
    }

    #[tokio::test]
    async fn disabled_store_methods_are_infallible_noops() {
        let store = VolatileStore::disabled();
        // None of these panic or block; writes are silent no-ops and the probe reports unhealthy.
        store
            .record_poller("edge-1", &sample_doc(), Duration::from_secs(30))
            .await;
        let mut map = HashMap::new();
        map.insert(Uuid::nil(), "edge-1".to_string());
        store.write_assignments("default", &map).await;
        store.write_assignments("empty", &HashMap::new()).await;
        assert!(!store.healthy().await);
    }

    #[tokio::test]
    async fn bad_url_degrades_to_disabled() {
        // An unparseable URL must not panic — it degrades to a no-op store.
        let store = VolatileStore::from_url("not a redis url");
        assert!(!store.healthy().await);
    }
}
