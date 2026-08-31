// SPDX-License-Identifier: AGPL-3.0-only
//! Durable poller inventory persistence (ADR-009).
//!
//! The `pollers` table is the *durable* record of which pollers have ever registered — first/last
//! seen, and the pool/version/incarnation of their most recent heartbeat. It is what lets the
//! Pollers view show a poller that is currently **offline** (its live liveness/assignment live in
//! Redis, ADR-004, and vanish on TTL expiry). This is the I/O adapter only; the coordinator
//! decides *when* to upsert (throttled, so a 10s heartbeat doesn't write every beat) and how the
//! rows are merged with the live view for the API — both in a later step.
//!
//! Runtime `sqlx::query` (not the compile-time macro) so the build needs no live database, and all
//! inputs are bound parameters (security.md). Mirrors the [`crate::audit`] repo's shape.

use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::{PgPool, Row};
use uuid::Uuid;

/// One `pollers` row (API shape). Timestamps are RFC 3339 text at the edge, matching how the
/// audit repo exposes its `at` column.
#[derive(Debug, Clone, Serialize)]
pub struct PollerRow {
    /// Sanitized poller id (the NATS-subject-safe identifier, stable across restarts).
    pub id: String,
    /// Pool the poller last reported serving.
    pub pool: String,
    /// Where an operator asked this poller to be, when that is not where it says it is (ADR-107).
    ///
    /// `None` for every poller under normal operation. Set, it means the destination has been
    /// recorded and the site has not been restarted yet — the poller is still serving [`Self::pool`]
    /// and will keep doing so until its own `.env` changes. Cleared by the first heartbeat that
    /// reports having arrived.
    /// When this poller was first seen (RFC 3339).
    pub first_seen: String,
    /// When this poller was last seen (RFC 3339).
    pub last_seen: String,
    /// Build version from its most recent heartbeat, if reported.
    pub last_version: Option<String>,
    /// Per-process incarnation from its most recent heartbeat, if reported.
    pub last_incarnation: Option<Uuid>,
    /// Interface addresses the poller reported for itself (ADR-043). Empty from an N-1 poller, and
    /// empty for a containerized poller whose only address is a bridge address — the two cases are
    /// answered the same way, by naming `anchor_node_id`.
    pub mgmt_addrs: Vec<String>,
    /// The node an operator named as this poller's attachment point, rooting the derived dependency
    /// graph. `None` means core places the poller from `mgmt_addrs` instead — or, if that matches
    /// nothing, that the poller is unplaced and derived suppression stays blocked.
    pub anchor_node_id: Option<Uuid>,
    /// Whether this poller has a bus token of its own (ADR-065). `false` means it is admitted by
    /// the deployment-wide bootstrap secret instead — which every poller was before tokens existed,
    /// and which a co-located poller on an unencrypted internal bus still is.
    // The token itself is never returned. This is the one fact the UI needs: it is what tells an
    // operator which sites still share one credential.
    pub has_token: bool,
    /// When its token was issued, RFC 3339, or empty if it has none.
    pub token_issued_at: Option<String>,
}

/// One `monitoring_gaps` row (API shape). A gap is one core↔poller **visibility outage**: core
/// stopped hearing from the poller (partition or the poller went down) and later saw it again. If the
/// poller was alive but partitioned, its store-and-forward buffer backfills the metrics for the
/// window on reconnect (Phase 3); alerts are *not* backfilled (they resume from "now").
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct MonitoringGapRow {
    /// Row id.
    pub id: Uuid,
    /// The poller whose visibility lapsed.
    pub poller_id: String,
    /// Pool it serves.
    pub pool: String,
    /// Start of the gap window (RFC 3339 — core's last contact before the outage).
    pub started_at: String,
    /// End of the gap window (RFC 3339 — core heard from it again).
    pub ended_at: String,
    /// Gap length in seconds (UI convenience).
    pub duration_secs: i64,
    /// When core recorded the gap (RFC 3339).
    pub recorded_at: String,
    /// Passive listeners the poller had bound when the gap began (e.g. `syslog:514`, `trap:162`).
    ///
    /// Empty ⇒ the poller had none, so the gap cost no passive data. Non-empty ⇒ whatever those
    /// listeners would have received in the window is **gone**: syslog, traps and flow exports are
    /// fire-and-forget, so unlike active polling there is no buffer to backfill from. (SNMP informs
    /// are the exception — the sender retries until acknowledged.)
    pub listeners: Vec<String>,
}

/// What the bus must check a connecting poller's secret against (ADR-065).
///
/// Absence of this value — `Option::None` from [`PollerRepo::auth_material`] — is the third answer
/// and the important one: the id is in no inventory, so it is refused.
/// What core's inventory knows about a connecting poller: how to admit it, and where it belongs.
///
/// The two travel together because the callout needs both at the same moment and there is exactly
/// one row to read them from. Keeping the pool out of it would mean a second query on the
/// connection path, or — the thing ADR-107 Inc.2 exists to stop — taking the pool from what the
/// connection says about itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PollerRegistration {
    /// The credential that admits this id.
    pub auth: PollerAuth,
    /// The pool core has it serving. `None` on a row written before the column meant anything,
    /// in which case the connection's own claim is still the only answer.
    pub pool: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PollerAuth {
    /// This poller has its own token; only `hex(sha256(token))` matching this admits it.
    Token(String),
    /// Registered but with no token yet — the deployment-wide bootstrap secret still admits it.
    Bootstrap,
}

/// The heartbeat write when an unknown id may register itself.
///
/// A named constant, with its sibling below, so the invariants both must hold —
/// **`anchor_node_id`, `first_seen` and now `pool` are never touched by a heartbeat** — are
/// assertable against the statements themselves rather than against a slice of this file's source.
/// The earlier test did the latter and broke the moment a second statement appeared beside the
/// first, which is exactly when it most needed to still work.
///
/// 🚨 **`pool` is written on INSERT and never on UPDATE, and that asymmetry is the whole of
/// ADR-107 Inc.2.** The insert is a poller introducing itself, where its own `YAGRA_POLLER_POOL`
/// is the only answer anyone has. Every beat after that is a *report*, and letting a report win
/// would put the pool back under the site's control: an operator's move would be reverted inside
/// the sixty-second write throttle, looking exactly like the click never took. Same shape, same
/// reason, as `anchor_node_id` — see [`PollerRepo::set_anchor`].
const SEEN_UPSERT_SQL: &str =
    "INSERT INTO pollers (id, pool, last_version, last_incarnation, mgmt_addrs) \
     VALUES ($1, $2, $3, $4, $5::text[]::inet[]) \
     ON CONFLICT (id) DO UPDATE SET \
       last_seen = now(), \
       last_version = EXCLUDED.last_version, \
       last_incarnation = EXCLUDED.last_incarnation, \
       mgmt_addrs = EXCLUDED.mgmt_addrs \
     RETURNING pool";

/// The heartbeat write when only an already-registered poller may be refreshed (ADR-065 Inc.3).
///
/// Four binds, not five: with `pool` no longer assignable there is nothing for the reported pool to
/// bind to, and PostgreSQL refuses a statement handed a parameter it does not name.
const SEEN_UPDATE_SQL: &str = "UPDATE pollers SET \
       last_seen = now(), last_version = $2, last_incarnation = $3, \
       mgmt_addrs = $4::text[]::inet[] \
     WHERE id = $1 \
     RETURNING pool";

/// PostgreSQL-backed durable poller inventory (`pollers`).
pub struct PollerRepo {
    pool: PgPool,
    /// Whether a heartbeat from an id with no row creates one. See [`Self::with_auto_register`].
    auto_register: bool,
}

impl PollerRepo {
    #[must_use]
    pub fn new(pool: PgPool) -> Self {
        Self {
            pool,
            // The historical behaviour, and the right default: on a deployment whose bus does not
            // gate connections, heartbeating is the only way a poller can appear at all.
            auto_register: true,
        }
    }

    /// Set whether an unknown poller id may create its own inventory row (ADR-065 Inc.3).
    ///
    /// Turned **off** on a deployment where the Auth Callout already refuses an unregistered id.
    /// There, an insert on the heartbeat path is the one thing that puts a row in the inventory
    /// without anybody deciding to — and it had a concrete consequence: **deleting a poller in the
    /// WebUI did not remove it**, because the live connection kept heartbeating and the upsert
    /// recreated the row within ten seconds. NATS does not re-authenticate an established
    /// connection, so refusing the reconnect is not enough on its own.
    ///
    /// A setting on the repo rather than a parameter on the write, because the answer is a property
    /// of the deployment and is read in one place — the coordinator calling it every ten seconds
    /// should not have to carry the reason with it.
    #[must_use]
    pub fn with_auto_register(mut self, yes: bool) -> Self {
        self.auto_register = yes;
        self
    }

    /// Record that a poller was seen: insert it (first contact) or refresh `last_seen` and the
    /// pool/version/incarnation/addresses of its latest heartbeat. `first_seen` is preserved on
    /// update, and so is `anchor_node_id` — that column is the operator's, not the poller's, and a
    /// heartbeat overwriting it would silently undo the fix for an unplaceable poller on the next
    /// beat.
    ///
    /// **`pool` is written only when the row is created** (ADR-107 Inc.2). After that core owns it
    /// and the beat's `pool` argument is used for nothing but the insert — see [`SEEN_UPSERT_SQL`].
    ///
    /// Returns the pool the inventory now holds, which is what the caller must actually serve. On
    /// a single core that is the value it just set itself; **across an HA pair it is how the leader
    /// learns about a move made on the follower**, since the two share only PostgreSQL. That makes
    /// convergence bounded by the caller's throttle rather than instant — say so wherever the
    /// feature promises "immediately".
    ///
    /// `Ok(None)` ⇒ no row and none created (auto-register off).
    ///
    /// Call-site throttling (so a 10s heartbeat isn't a write per beat) is the coordinator's job.
    ///
    /// Addresses travel as text and are cast server-side: `INET` has no `sqlx` codec compiled in
    /// here (the node table reads its address through `host()` for the same reason), and the cast
    /// still makes PostgreSQL reject a malformed address rather than storing it.
    /// Whether an unknown id creates a row is [`Self::with_auto_register`]'s call, not this one's.
    pub async fn upsert_seen(
        &self,
        id: &str,
        pool: &str,
        version: &str,
        incarnation: Uuid,
        mgmt_addrs: &[String],
    ) -> anyhow::Result<Option<String>> {
        // Two statements rather than one with a conditional clause: the difference IS whether a row
        // may be created, and expressing that as `WHERE EXISTS` inside an upsert is the kind of SQL
        // that reads as equivalent to a future editor and is not.
        //
        // They no longer take the same binds either: only the insert names `pool`.
        let row = if self.auto_register {
            sqlx::query(SEEN_UPSERT_SQL)
                .bind(id)
                .bind(pool)
                .bind(version)
                .bind(incarnation)
                .bind(mgmt_addrs)
                .fetch_optional(&self.pool)
                .await?
        } else {
            sqlx::query(SEEN_UPDATE_SQL)
                .bind(id)
                .bind(version)
                .bind(incarnation)
                .bind(mgmt_addrs)
                .fetch_optional(&self.pool)
                .await?
        };
        let Some(row) = row else {
            // Not an error: the poller was deleted while its connection was still up, which is
            // exactly what this mode exists to make stick. Logged so it is not a silent no-op.
            tracing::debug!(
                poller = id,
                "heartbeat from a poller that is not in the inventory — not recreating it"
            );
            return Ok(None);
        };
        Ok(row.try_get::<Option<String>, _>("pool")?)
    }

    /// Point a poller at the node it attaches to, or clear it (`None`).
    ///
    /// Separate from [`Self::upsert_seen`] on purpose: this column is written by an operator and
    /// that one by a heartbeat arriving every ten seconds. One statement writing both would make the
    /// poller the last writer, and the anchor would revive its old value within one beat.
    pub async fn set_anchor(&self, id: &str, node_id: Option<Uuid>) -> anyhow::Result<bool> {
        let res = sqlx::query("UPDATE pollers SET anchor_node_id = $2 WHERE id = $1")
            .bind(id)
            .bind(node_id)
            .execute(&self.pool)
            .await?;
        Ok(res.rows_affected() > 0)
    }

    /// Every poller in the inventory, ordered by id.
    pub async fn list(&self) -> anyhow::Result<Vec<PollerRow>> {
        let rows = sqlx::query(
            // `token_hash IS NOT NULL` rather than the column itself: the digest has no use outside
            // the callout, and a list endpoint is the wrong place to start carrying one around.
            "SELECT id, pool, first_seen, last_seen, last_version, last_incarnation, \
                    anchor_node_id, token_issued_at, (token_hash IS NOT NULL) AS has_token, \
                    ARRAY(SELECT host(a) FROM unnest(mgmt_addrs) AS a) AS mgmt_addrs \
             FROM pollers ORDER BY id",
        )
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter()
            .map(|row| {
                let first_seen: DateTime<Utc> = row.try_get("first_seen")?;
                let last_seen: DateTime<Utc> = row.try_get("last_seen")?;
                let issued: Option<DateTime<Utc>> = row.try_get("token_issued_at")?;
                Ok(PollerRow {
                    id: row.try_get("id")?,
                    pool: row.try_get("pool")?,
                    first_seen: first_seen.to_rfc3339(),
                    last_seen: last_seen.to_rfc3339(),
                    last_version: row.try_get("last_version")?,
                    last_incarnation: row.try_get("last_incarnation")?,
                    mgmt_addrs: row.try_get("mgmt_addrs")?,
                    anchor_node_id: row.try_get("anchor_node_id")?,
                    has_token: row.try_get("has_token")?,
                    token_issued_at: issued.map(|t| t.to_rfc3339()),
                })
            })
            .collect()
    }

    /// The distinct pools that have at least one registered poller (ADR-009 Inc.1).
    ///
    /// 🚨 **Grounds for *waiting*, never for believing a poller is live.** A row here says only
    /// that some poller with this id once heartbeated — it survives the poller being switched off,
    /// decommissioned, or unreachable for a week. The scheduler uses it for exactly one question:
    /// on a cold start, is there any reason to expect a beat before falling back to legacy per-job
    /// dispatch? Liveness stays the coordinator's in-memory registry and nothing else.
    ///
    /// ⚠️ A poller too old to heartbeat has no row, so its pool is never waited on — which is what
    /// keeps the N/N-1 legacy fallback byte-identical for it.
    pub async fn pools(&self) -> anyhow::Result<Vec<String>> {
        let rows = sqlx::query("SELECT DISTINCT pool FROM pollers")
            .fetch_all(&self.pool)
            .await?;
        rows.into_iter()
            .map(|row| Ok(row.try_get("pool")?))
            .collect()
    }

    /// What the Auth Callout must check this poller's presented secret against (ADR-065).
    ///
    /// Three answers, and the third is the one that closes the hole: an id with no row is refused
    /// outright, so a leaked bootstrap secret can only impersonate a poller somebody registered
    /// rather than any id the holder invents. See `migrations/0090_poller_token.sql` for the whole
    /// rule and why the middle case still exists.
    ///
    /// On a database error this returns `Ok(None)` — *deny*. A callout that cannot reach PostgreSQL
    /// must not fall open onto the shared secret: that would turn a database blip into the exact
    /// pre-token behaviour, silently, at the moment nobody is watching.
    pub async fn auth_material(&self, id: &str) -> Option<PollerRegistration> {
        match sqlx::query("SELECT token_hash, pool FROM pollers WHERE id = $1")
            .bind(id)
            .fetch_optional(&self.pool)
            .await
        {
            Ok(Some(row)) => {
                let auth = match row.try_get::<Option<String>, _>("token_hash") {
                    Ok(Some(hash)) => PollerAuth::Token(hash),
                    Ok(None) => PollerAuth::Bootstrap,
                    Err(_) => return None,
                };
                // A read failure here is not a denial: the credential check above already
                // succeeded, and refusing the whole connection because one column would not decode
                // would take a site offline over a display value. Falling back to the claimed pool
                // is the pre-ADR-107 behaviour.
                let pool = row.try_get::<Option<String>, _>("pool").ok().flatten();
                Some(PollerRegistration { auth, pool })
            }
            Ok(None) => None,
            Err(e) => {
                tracing::warn!(error = %e, poller = id, "could not read poller auth material — denying");
                None
            }
        }
    }

    /// Store a token for `id`, creating the inventory row if the poller has not connected yet.
    ///
    /// Takes the **digest**, never the token: the caller generates and displays the secret, and this
    /// module is not a place a plaintext credential should ever be in scope.
    ///
    /// Creating the row here is what makes "register a site before it exists" work — the poller is
    /// refused until a row names it, so issuing the token has to be able to name it first.
    pub async fn issue_token(
        &self,
        id: &str,
        pool: &str,
        hash: &str,
        by: Option<Uuid>,
    ) -> anyhow::Result<()> {
        sqlx::query(
            "INSERT INTO pollers (id, pool, token_hash, token_issued_at, token_issued_by) \
             VALUES ($1, $2, $3, now(), $4) \
             ON CONFLICT (id) DO UPDATE SET \
               token_hash = EXCLUDED.token_hash, \
               token_issued_at = now(), \
               token_issued_by = EXCLUDED.token_issued_by",
        )
        .bind(id)
        .bind(pool)
        .bind(hash)
        .bind(by)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Make sure a poller has an inventory row, without giving it a token (ADR-065 Inc.7).
    ///
    /// 🚨 This exists for exactly one caller and one moment: the remote-poller switch, applied to
    /// the ids the updater reports as belonging to its own compose project. Turning that switch on
    /// also turns [`Self::with_auto_register`] off, so from that moment a heartbeat from an id with
    /// no row no longer creates one — and a co-located poller that had never heartbeated (a fresh
    /// deployment where the switch is the first thing pressed) would connect, be authorized by the
    /// static account it bypasses the callout with, and then sit there with no assignment forever.
    /// Nothing would say so: the Pollers page would show an empty fleet on a deployment that had
    /// just reported the change as succeeding.
    ///
    /// `DO NOTHING`, not an upsert: a poller that already has a row keeps its pool, its anchor, its
    /// token and its history. This may only ever *add*.
    ///
    /// Returns how many rows it created, so the caller can log a number rather than an intention.
    ///
    /// # Errors
    /// A database failure. Callers treat it as non-fatal — the switch has a bus to reconfigure and
    /// this is preparation for it, not the change itself.
    pub async fn ensure_registered(&self, ids: &[String], pool: &str) -> anyhow::Result<u64> {
        let mut created = 0;
        for id in ids.iter().filter(|i| !i.trim().is_empty()) {
            created += sqlx::query(
                "INSERT INTO pollers (id, pool) VALUES ($1, $2) ON CONFLICT (id) DO NOTHING",
            )
            .bind(id.trim())
            .bind(pool)
            .execute(&self.pool)
            .await?
            .rows_affected();
        }
        Ok(created)
    }

    /// Drop a poller's token, returning it to the deployment-wide bootstrap secret.
    ///
    /// Deliberately **not** the same thing as deleting the poller: an operator revoking a leaked
    /// token wants the site back on a new one, not the inventory row (and its anchor, and its
    /// history) gone. Returns whether a row was changed.
    pub async fn revoke_token(&self, id: &str) -> anyhow::Result<bool> {
        let res = sqlx::query(
            "UPDATE pollers SET token_hash = NULL, token_issued_at = NULL, \
                                token_issued_by = NULL \
             WHERE id = $1",
        )
        .bind(id)
        .execute(&self.pool)
        .await?;
        Ok(res.rows_affected() > 0)
    }

    /// Delete a poller by id (operator removing a decommissioned poller). Returns whether a row
    /// was removed.
    pub async fn delete(&self, id: &str) -> anyhow::Result<bool> {
        let res = sqlx::query("DELETE FROM pollers WHERE id = $1")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(res.rows_affected() > 0)
    }

    /// Record one detected monitoring gap (a known poller reappeared after being offline). `started`
    /// and `ended` are Unix milliseconds. Best-effort: a failed insert just means the gap isn't
    /// listed. The coordinator calls this once per offline→online transition (one row per gap).
    pub async fn insert_monitoring_gap(
        &self,
        poller_id: &str,
        pool: &str,
        started_ms: i64,
        ended_ms: i64,
        listeners: &[String],
    ) -> anyhow::Result<()> {
        sqlx::query(
            "INSERT INTO monitoring_gaps \
             (poller_id, pool, started_at_unix_ms, ended_at_unix_ms, listeners) \
             VALUES ($1, $2, $3, $4, $5)",
        )
        .bind(poller_id)
        .bind(pool)
        .bind(started_ms)
        .bind(ended_ms)
        .bind(listeners.join(","))
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Drop recorded gaps older than `retention_secs`.
    ///
    /// `retention::Subject::MonitoringGaps`, on the **alert-linked** window rather than a window of
    /// its own: a gap says "no alert fired here because monitoring was blind", which is only
    /// readable beside the alert history it explains. A poller that flaps writes one row per
    /// transition, so before this nothing bounded the table.
    pub async fn prune_monitoring_gaps(&self, retention_secs: i64) -> anyhow::Result<u64> {
        let res = sqlx::query(
            "DELETE FROM monitoring_gaps WHERE recorded_at < now() - make_interval(secs => $1)",
        )
        .bind(retention_secs as f64)
        .execute(&self.pool)
        .await?;
        Ok(res.rows_affected())
    }

    /// The most recent monitoring gaps, newest first (capped). Powers the Pollers page's "Recent
    /// monitoring gaps" section.
    pub async fn list_monitoring_gaps(&self, limit: i64) -> anyhow::Result<Vec<MonitoringGapRow>> {
        let rows = sqlx::query(
            "SELECT id, poller_id, pool, started_at_unix_ms, ended_at_unix_ms, listeners, \
             recorded_at FROM monitoring_gaps ORDER BY recorded_at DESC LIMIT $1",
        )
        .bind(limit.clamp(1, 1000))
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter()
            .map(|row| {
                let started: i64 = row.try_get("started_at_unix_ms")?;
                let ended: i64 = row.try_get("ended_at_unix_ms")?;
                let recorded_at: DateTime<Utc> = row.try_get("recorded_at")?;
                Ok(MonitoringGapRow {
                    id: row.try_get("id")?,
                    poller_id: row.try_get("poller_id")?,
                    pool: row.try_get("pool")?,
                    started_at: ms_to_rfc3339(started),
                    ended_at: ms_to_rfc3339(ended),
                    duration_secs: (ended - started).max(0) / 1000,
                    recorded_at: recorded_at.to_rfc3339(),
                    listeners: row
                        .try_get::<String, _>("listeners")?
                        .split(',')
                        .filter(|s| !s.is_empty())
                        .map(str::to_owned)
                        .collect(),
                })
            })
            .collect()
    }
}

/// Give this deployment's **own** pollers an inventory row, if anything can name them.
///
/// The one place that decision is made, called from the two moments it has to hold at
/// (ADR-065 Inc.8): core startup, and the "Accept remote pollers" switch. Both matter and neither
/// covers the other — the switch is the moment auto-registration stops, and startup is every
/// *later* moment, because an upgrade replaces the composition without anyone pressing the switch.
///
/// # Why a co-located poller needs this at all
///
/// Accepting remote pollers turns [`PollerRepo::with_auto_register`] off, on the grounds that the
/// Auth Callout now refuses an id nobody registered. 🚨 **Those grounds do not cover the pollers
/// inside this composition.** They connect on the static `poller` account, which the callout
/// deliberately bypasses (`auth_users`), so the bus never refuses them — leaving them neither
/// refused nor registered. They keep polling off their Redis liveness and disappear from
/// Settings ▸ Pollers, which is the one outcome the gate's own comment set out to prevent.
///
/// # Two sources, and the second one alone is not enough
///
/// `YAGRA_LOCAL_POLLER_ID` is the id the composition gives its own poller — the same expression the
/// poller itself is given, so the two cannot disagree. The updater's `local_pollers` is the only
/// thing that can name *several*, or one whose id an operator set per container.
///
/// 🚨 **The updater's list is not usable on its own, and this was measured rather than reasoned.**
/// Its heartbeat is a file refreshed on a timer, so at core startup it still names the container
/// that compose has just replaced. On 192.168.1.211 (2026-08-26) that created a row for a dead id —
/// `last_version` NULL, `last_seen` equal to the adoption instant — while the live poller stayed
/// unregistered with 13 nodes assigned to it. Reading the env var first is what makes startup
/// adoption correct on the one run where it matters: the upgrade that changes the id.
///
/// Ids are unioned, not preferred: `ensure_registered` is `ON CONFLICT DO NOTHING`, so naming an id
/// twice costs nothing and naming a stale one costs a row an operator can delete — whereas missing
/// the live one costs a poller that no page in the product lists.
///
/// `Ok(None)` means nothing named a poller at all: no env var and no updater. Failure is the
/// caller's to log, never fatal — this is preparation, not the change itself.
pub async fn register_local(
    pollers: &PollerRepo,
    upgrade: &crate::upgrade::UpgradeRepo,
) -> anyhow::Result<Option<(u64, usize)>> {
    let mut ids: Vec<String> = crate::config::local_poller_id().into_iter().collect();
    if let Some(from_updater) = upgrade.heartbeat().and_then(|h| h.local_pollers) {
        for id in from_updater {
            if !ids.iter().any(|k| k == &id) {
                ids.push(id);
            }
        }
    }
    if ids.is_empty() {
        return Ok(None);
    }
    let created = pollers
        .ensure_registered(&ids, yagra_bus::DEFAULT_POOL)
        .await?;
    Ok(Some((created, ids.len())))
}

/// Format Unix milliseconds as RFC 3339 UTC (matching how the rest of this repo exposes timestamps).
fn ms_to_rfc3339(ms: i64) -> String {
    DateTime::<Utc>::from_timestamp_millis(ms)
        .map(|d| d.to_rfc3339())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// This module's code, comments stripped — see
    /// [`crate::module_source::code_no_comments`] for why both.
    fn production_source() -> String {
        crate::module_source::code_no_comments("src", "pollers")
    }

    #[test]
    fn a_timestamp_renders_as_rfc3339_utc() {
        assert_eq!(ms_to_rfc3339(0), "1970-01-01T00:00:00+00:00");
        assert!(ms_to_rfc3339(1_700_000_000_000).starts_with("2023-11-14T"));
    }

    #[test]
    fn an_unrepresentable_timestamp_renders_empty_rather_than_panicking() {
        // The column is an i64 written by a poller, so a clock fault or a corrupt row can land
        // outside what a DateTime can hold. The Pollers page showing a blank cell beats core
        // panicking on a list read.
        assert_eq!(ms_to_rfc3339(i64::MAX), "");
        assert_eq!(ms_to_rfc3339(i64::MIN), "");
    }

    #[test]
    fn the_gap_insert_and_read_agree_on_the_listeners_column() {
        // The column is written by one SQL string and read by another; nothing makes them agree,
        // and a mismatch means rows the writer produces are rows the reader cannot parse. Both
        // needles are assembled at runtime so this test does not match its own source.
        let src = production_source();
        let col = format!("{}s", "listener");
        assert!(
            src.contains(&format!("ended_at_unix_ms, {col})")),
            "insert omits the column"
        );
        assert!(
            src.contains(&format!("ended_at_unix_ms, {col}, ")),
            "select omits the column"
        );
    }
    #[test]
    fn a_gaps_duration_is_never_negative() {
        // `started`/`ended` are both poller-supplied, so a clock stepping backwards mid-gap would
        // otherwise report a negative outage — which reads as nonsense in the UI and sorts wrong.
        let duration = |started: i64, ended: i64| (ended - started).max(0) / 1000;
        assert_eq!(duration(1_000, 61_000), 60);
        assert_eq!(duration(61_000, 1_000), 0);
        assert_eq!(duration(1_000, 1_000), 0);
        // Sub-second gaps floor to zero rather than rounding up to a second that did not happen.
        assert_eq!(duration(1_000, 1_999), 0);
        assert!(production_source().contains("(ended - started).max(0) / 1000"));
    }

    #[test]
    fn the_inventory_lists_deterministically_and_the_gap_log_newest_first() {
        // Without an ORDER BY the same page can come back in a different order each fetch, which
        // reads as pollers shuffling on every refresh.
        let src = production_source();
        assert!(src.contains("FROM pollers ORDER BY id"));
        assert!(src.contains("FROM monitoring_gaps ORDER BY recorded_at DESC LIMIT $1"));
    }

    #[test]
    fn heartbeats_upsert_rather_than_accumulating_a_row_per_beat() {
        // A poller heartbeats continuously; an INSERT per beat would grow the table without bound.
        let src = production_source();
        assert!(src.contains("ON CONFLICT"));
        assert!(src.contains("last_seen = now()"));
    }

    /// **The anchor is the operator's column.** A heartbeat arrives every ten seconds; if its
    /// upsert touched `anchor_node_id`, the fix an operator just applied to an unplaceable poller
    /// would be reverted within one beat and derived suppression would stay blocked with no
    /// indication why. The two writers are separate statements, and this pins that.
    #[test]
    fn a_heartbeat_upsert_never_writes_the_operator_set_anchor() {
        let src = production_source();
        let col = format!("anchor_node{}", "_id");
        // Both heartbeat statements, not just the one that existed first. The second was added with
        // ADR-065 Inc.3 and is the one a gated deployment actually runs, so a rule checked on only
        // the upsert would be checked on the path most deployments stop using.
        for (name, sql) in [
            ("SEEN_UPSERT_SQL", SEEN_UPSERT_SQL),
            ("SEEN_UPDATE_SQL", SEEN_UPDATE_SQL),
        ] {
            assert!(
                !sql.contains(&col),
                "{name} overwrites the anchor, which is the operator's column: {sql}"
            );
            assert!(
                !sql.contains("first_seen"),
                "{name} rewrites first_seen, so a poller's history restarts on every beat: {sql}"
            );
            assert!(
                !sql.contains("token_hash"),
                "{name} touches the poller's token; a heartbeat must never be able to change a \
                 credential: {sql}"
            );
        }
        // The two differ in exactly one way — whether an unknown id gets a row.
        assert!(SEEN_UPSERT_SQL.starts_with("INSERT INTO pollers"));
        assert!(SEEN_UPDATE_SQL.starts_with("UPDATE pollers SET"));
        // And the operator's anchor writer exists and touches only that column.
        assert!(src.contains(&format!("UPDATE pollers SET {col} = $2 WHERE id = $1")));
    }

    /// **The pool became the operator's column too** (ADR-107 Inc.2), and it is a harder case than
    /// the anchor: the insert *must* write it, because a poller introducing itself is the only
    /// source there is. So the rule is not "never named" but "named on the way in and never again".
    ///
    /// 🚨 What this stops is subtle and was the whole reason the move could not work before. A
    /// heartbeat arrives every ten seconds; if its update assigned `pool`, an operator's move would
    /// be reverted within the sixty-second write throttle and the UI would look as though the click
    /// had simply not registered — no error, no log line, nothing to search for.
    ///
    /// Both directions, deliberately. The refusal alone would pass on a pair of statements that
    /// never mention `pool` at all, and a poller that never records a pool is one core can never
    /// assign work to.
    #[test]
    fn only_a_first_contact_may_set_the_pool_a_later_heartbeat_never_does() {
        // Built at runtime so this needle cannot match the line it is written on
        // (`self-matching-needle-has-two-directions`).
        let col = format!("po{}", "ol");

        // Refusal: no assignment to the column in either statement, by any route.
        for (name, sql) in [
            ("SEEN_UPSERT_SQL", SEEN_UPSERT_SQL),
            ("SEEN_UPDATE_SQL", SEEN_UPDATE_SQL),
        ] {
            for bad in [
                format!("{col} = EXCLUDED.{col}"),
                format!("{col} = $"),
                format!("{col} = pollers.{col}"),
            ] {
                assert!(
                    !sql.contains(&bad),
                    "{name} assigns the pool on a heartbeat (\"{bad}\"), so the poller overrules \
                     core's assignment within one write throttle: {sql}"
                );
            }
        }

        // Acceptance 1: the insert still carries it, so a brand-new poller lands in the pool its
        // own environment names. Without this the two assertions above are satisfied by a pair of
        // statements that never record a pool at all.
        assert!(
            SEEN_UPSERT_SQL.contains(&format!("INSERT INTO pollers (id, {col},")),
            "first contact no longer records the poller's own pool: {SEEN_UPSERT_SQL}"
        );

        // Acceptance 2: both statements hand the authoritative value back, which is how an HA
        // leader learns about a move a follower took.
        for (name, sql) in [
            ("SEEN_UPSERT_SQL", SEEN_UPSERT_SQL),
            ("SEEN_UPDATE_SQL", SEEN_UPDATE_SQL),
        ] {
            assert!(
                sql.trim_end().ends_with(&format!("RETURNING {col}")),
                "{name} no longer returns the authoritative pool: {sql}"
            );
        }

        // Acceptance 3: the writer exists — **in repo/pools.rs, not here**, and that placement is
        // itself the rule. A pool change may travel with a node/folder re-pointing, and the two
        // have to commit together: a poller moved without its nodes leaves them in a pool with no
        // poller, which is a silent monitoring hole. So the only writer is inside a transaction,
        // and this file must not offer a second, non-transactional one.
        let src = production_source();
        assert!(
            !src.contains(&format!("UPDATE pollers SET {col}")),
            "a second, non-transactional pool writer has appeared here; the move must stay atomic \
             with the node/folder re-pointing in repo/pools.rs"
        );
        let repo = crate::module_source::files(&crate::module_source::roots("src", "repo"))
            .into_iter()
            .map(|(_, text)| text)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            repo.contains(&format!("UPDATE pollers SET {col} = $2 WHERE id = $1")),
            "the transactional pool writer is gone from repo/, so nothing can move a poller"
        );

        // And the bind counts differ, because only one statement names the pool. Getting this
        // wrong is not a compile error — PostgreSQL refuses at runtime with "bind message supplies
        // N parameters, but prepared statement requires M", on the heartbeat path, in production.
        let binds = |sql: &str| (1..=9).filter(|i| sql.contains(&format!("${i}"))).count();
        assert_eq!(binds(SEEN_UPSERT_SQL), 5, "{SEEN_UPSERT_SQL}");
        assert_eq!(binds(SEEN_UPDATE_SQL), 4, "{SEEN_UPDATE_SQL}");
    }

    /// The truth table those two `CASE` expressions encode, so the intent is pinned in Rust and not
    /// only in a SQL string a reader has to evaluate in their head.
    #[test]
    fn a_destination_clears_on_arrival_and_survives_every_other_beat() {
        // `reported` is what the beat says; `desired` is what the operator recorded.
        let after = |reported: &str, desired: Option<&str>| -> Option<String> {
            match desired {
                Some(d) if d == reported => None,
                other => other.map(str::to_owned),
            }
        };
        // Arrived ⇒ the badge goes away by itself. This is the only write a beat may make.
        assert_eq!(after("tokyo", Some("tokyo")), None);
        // Still at the old pool ⇒ the destination survives, beat after beat, for as long as it
        // takes somebody to reach the site. This is the case a bind would have destroyed.
        assert_eq!(after("default", Some("tokyo")), Some("tokyo".to_owned()));
        // Moved somewhere else entirely ⇒ still not the poller's call to erase the record.
        assert_eq!(after("osaka", Some("tokyo")), Some("tokyo".to_owned()));
        // No move pending ⇒ nothing to do, and nothing invented.
        assert_eq!(after("default", None), None);
    }

    #[test]
    fn addresses_are_stored_as_inet_so_a_malformed_one_is_rejected_at_write_time() {
        // Bound as text and cast, because there is no INET codec compiled in — but the cast is what
        // makes PostgreSQL reject `"not-an-address"` instead of storing it and failing later inside
        // anchor resolution, where the failure would be silent.
        let src = production_source();
        assert!(src.contains("$5::text[]::inet[]"));
        assert!(src.contains("ARRAY(SELECT host(a) FROM unnest(mgmt_addrs) AS a)"));
    }

    #[test]
    fn every_statement_binds_its_values_instead_of_interpolating_them() {
        // The poller id is caller-supplied (`YAGRA_POLLER_ID`) and reaches this store unvalidated.
        let src = production_source();
        for builder in ["format!(", "push_str("] {
            assert!(
                !src.contains(builder),
                "SQL may be being built by string concatenation ({builder}); bind the value instead"
            );
        }
    }
}
