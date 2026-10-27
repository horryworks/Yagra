// SPDX-License-Identifier: AGPL-3.0-only
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
//! The module is deliberately thin: it caches one connection, drops it after an error and opens a
//! new one on the next call, but not before [`RECONNECT_BACKOFF`] has passed. Keys are minted by
//! small helpers so the exact wire format is unit-tested in one place. The protocol is
//! `fleet/resp.rs` since ADR-213 decision 3 (it was the `redis` crate); every exchange now has an
//! upper bound ([`EXCHANGE_TIMEOUT`]), where the crate had none.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;
use uuid::Uuid;

use crate::fleet::resp::{Connection, RedisTarget, RespError};

/// How long opening a connection (TCP + `AUTH` + `SELECT`) may take.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
/// How long one exchange may take. `write_assignments` sends one `HSET` for a whole pool, which at
/// 50,000 nodes is a few megabytes on a LAN — well inside this.
const EXCHANGE_TIMEOUT: Duration = Duration::from_secs(10);
/// After a failure, calls inside this window return at once instead of reconnecting. Every poller
/// heartbeat awaits `record_poller`, so a Redis that black-holes connections would otherwise cost
/// each heartbeat a full [`CONNECT_TIMEOUT`].
const RECONNECT_BACKOFF: Duration = Duration::from_secs(5);

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

/// The cached connection, and when a reconnect may next be tried.
#[derive(Default)]
struct ConnState {
    conn: Option<Connection>,
    retry_at: Option<Instant>,
}

/// Best-effort mirror of volatile poller state into Redis. `target == None` ⇒ degraded (every
/// method is a no-op); otherwise a lazily-opened connection is cached and reopened after an error.
pub struct VolatileStore {
    /// `None` in degraded mode (no/blank/bad URL) — all methods short-circuit to no-ops.
    target: Option<RedisTarget>,
    /// Cached connection; opened lazily and dropped on error so it self-heals. Held for the length
    /// of one exchange, which is what keeps two callers' replies apart on one socket.
    conn: Mutex<ConnState>,
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
        let url = yagra_common::env::nonempty("YAGRA_REDIS_URL");
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
        match RedisTarget::parse(url) {
            Ok(target) => Self {
                target: Some(target),
                conn: Mutex::new(ConnState::default()),
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
            target: None,
            conn: Mutex::new(ConnState::default()),
            outage_logged: AtomicBool::new(false),
        }
    }

    /// Mirror one poller's liveness/telemetry doc: `SET yagra:poller:{id} <json> EX <ttl>`.
    pub async fn record_poller(&self, id: &str, doc: &PollerLiveDoc, ttl: Duration) {
        if self.target.is_none() {
            return;
        }
        let json = match serde_json::to_string(doc) {
            Ok(j) => j,
            Err(e) => {
                tracing::warn!(error = %e, poller = id, "serializing poller live doc failed");
                return;
            }
        };
        let _ = self.exchange(vec![set_with_ttl(id, json, ttl)]).await;
    }

    /// Replace a pool's assignment map atomically-ish: `DEL yagra:assign:{pool}` then rebuild it
    /// with `HSET` (node uuid → poller id) inside a MULTI/EXEC pipeline. An empty map just clears
    /// the key (a pool with no live pollers has no assignments).
    pub async fn write_assignments(&self, pool: &str, map: &HashMap<Uuid, String>) {
        if self.target.is_none() {
            return;
        }
        let _ = self.exchange(assignment_block(pool, map)).await;
    }

    /// Liveness probe: `PING`. `false` in degraded mode or on any error.
    // Consumed by the Pollers/health API (Step 6).
    #[allow(dead_code)]
    pub async fn healthy(&self) -> bool {
        if self.target.is_none() {
            return false;
        }
        self.exchange(vec![vec![b"PING".to_vec()]]).await.is_some()
    }

    /// Run `commands` on the cached connection, opening one if there is none, and fold the result
    /// into the outage flag: reset (and log recovery) on success; log once, drop the connection and
    /// start the reconnect backoff on failure. `None` on any failure — never surfaces the error.
    async fn exchange(
        &self,
        commands: Vec<Vec<Vec<u8>>>,
    ) -> Option<Vec<crate::fleet::resp::Reply>> {
        let target = self.target.as_ref()?;
        let mut state = self.conn.lock().await;
        // Taken out for the exchange and put back only when it completes: a caller dropped between
        // the write and the last read would otherwise leave unread replies on the cached
        // connection, and the next exchange would read them as its own.
        let mut conn = match state.conn.take() {
            Some(c) => c,
            None => {
                if state.retry_at.is_some_and(|at| Instant::now() < at) {
                    return None;
                }
                match Connection::open(target, CONNECT_TIMEOUT).await {
                    Ok(c) => c,
                    Err(e) => {
                        self.note_outage(&mut state, &e);
                        return None;
                    }
                }
            }
        };
        match conn.exchange(&commands, EXCHANGE_TIMEOUT).await {
            Ok(replies) => {
                state.conn = Some(conn);
                state.retry_at = None;
                if self.outage_logged.swap(false, Ordering::Relaxed) {
                    tracing::info!("Redis recovered — poller state mirroring resumed");
                }
                Some(replies)
            }
            Err(e) => {
                self.note_outage(&mut state, &e);
                None
            }
        }
    }

    /// Log an outage at most once (until the next success), invalidate the cached connection so a
    /// later call reconnects, and hold reconnects off for [`RECONNECT_BACKOFF`].
    fn note_outage(&self, state: &mut ConnState, e: &RespError) {
        if !self.outage_logged.swap(true, Ordering::Relaxed) {
            tracing::warn!(error = %e, "Redis unavailable — poller state mirroring degraded to no-op (best-effort, ADR-017)");
        }
        state.conn = None;
        state.retry_at = Some(Instant::now() + RECONNECT_BACKOFF);
    }
}

/// `SET yagra:poller:{id} <json> EX <ttl>` — the TTL is at least one second, as Redis refuses 0.
fn set_with_ttl(id: &str, json: String, ttl: Duration) -> Vec<Vec<u8>> {
    vec![
        b"SET".to_vec(),
        poller_key(id).into_bytes(),
        json.into_bytes(),
        b"EX".to_vec(),
        ttl.as_secs().max(1).to_string().into_bytes(),
    ]
}

/// `MULTI`, `DEL yagra:assign:{pool}`, one `HSET` of every node → poller pair (none for an empty
/// map), `EXEC` — the same block the `redis` crate's atomic pipeline sent.
fn assignment_block(pool: &str, map: &HashMap<Uuid, String>) -> Vec<Vec<Vec<u8>>> {
    let key = assign_key(pool).into_bytes();
    let mut block = vec![vec![b"MULTI".to_vec()], vec![b"DEL".to_vec(), key.clone()]];
    if !map.is_empty() {
        let mut hset = Vec::with_capacity(2 + map.len() * 2);
        hset.push(b"HSET".to_vec());
        hset.push(key);
        for (node, poller) in map {
            hset.push(node.to_string().into_bytes());
            hset.push(poller.clone().into_bytes());
        }
        block.push(hset);
    }
    block.push(vec![b"EXEC".to_vec()]);
    block
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

    #[test]
    fn the_set_carries_the_doc_and_a_ttl_of_at_least_a_second() {
        let cmd = set_with_ttl("edge-1", "{}".into(), Duration::from_millis(200));
        let words: Vec<String> = cmd
            .iter()
            .map(|w| String::from_utf8(w.clone()).unwrap())
            .collect();
        assert_eq!(words, ["SET", "yagra:poller:edge-1", "{}", "EX", "1"]);
    }

    #[test]
    fn the_assignment_block_is_one_transaction_and_an_empty_map_only_clears() {
        let text = |block: Vec<Vec<Vec<u8>>>| -> Vec<String> {
            block
                .into_iter()
                .map(|c| String::from_utf8(c[0].clone()).unwrap())
                .collect()
        };
        assert_eq!(
            text(assignment_block("lab", &HashMap::new())),
            ["MULTI", "DEL", "EXEC"]
        );
        let mut map = HashMap::new();
        map.insert(Uuid::nil(), "edge-1".to_string());
        let block = assignment_block("lab", &map);
        assert_eq!(block[2].len(), 4, "HSET key field value");
        assert_eq!(block[2][1], b"yagra:assign:lab");
        assert_eq!(block[2][3], b"edge-1");
        assert_eq!(text(block), ["MULTI", "DEL", "HSET", "EXEC"]);
    }

    /// A caller dropped mid-exchange must not leave the connection cached with its replies unread —
    /// the next exchange would read them as its own.
    #[tokio::test]
    async fn an_exchange_abandoned_mid_reply_does_not_keep_the_connection() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        // Accepts, reads the request, and never answers.
        let server = tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut buf = [0u8; 64];
            let _ = tokio::io::AsyncReadExt::read(&mut sock, &mut buf).await;
            std::future::pending::<()>().await;
        });
        let store = VolatileStore::from_url(&format!("redis://127.0.0.1:{port}"));
        let abandoned = tokio::time::timeout(Duration::from_millis(200), store.healthy()).await;
        assert!(abandoned.is_err(), "the caller gave up before any reply");
        assert!(
            store.conn.lock().await.conn.is_none(),
            "a connection with a reply still owed was put back"
        );
        server.abort();
    }

    /// A Redis that is down costs one connection attempt per backoff window, not one per call.
    #[tokio::test]
    async fn a_failed_connect_holds_further_attempts_off() {
        // Bind and drop: nothing listens on this port afterwards, so the connect is refused.
        let port = {
            let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            l.local_addr().unwrap().port()
        };
        let store = VolatileStore::from_url(&format!("redis://127.0.0.1:{port}"));
        assert!(!store.healthy().await);
        let retry_at = store.conn.lock().await.retry_at;
        assert!(retry_at.is_some(), "the refusal started the backoff");
        assert!(!store.healthy().await);
        assert_eq!(
            store.conn.lock().await.retry_at,
            retry_at,
            "the second call inside the window did not try again"
        );
    }

    #[tokio::test]
    async fn bad_url_degrades_to_disabled() {
        // An unparseable URL must not panic — it degrades to a no-op store.
        let store = VolatileStore::from_url("not a redis url");
        assert!(!store.healthy().await);
    }
}
