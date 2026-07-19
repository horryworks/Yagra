//! Runtime configuration from the environment.
//!
//! The three store/bus URLs decide the run mode: if all are present the core runs
//! **live** (PostgreSQL + NATS + VictoriaMetrics, real polling); if any is missing it
//! falls back to the in-memory **skeleton** so a bare `cargo run` still serves the API.
//! Compose always injects all three.

/// Default polling interval when `YAGRA_POLL_INTERVAL_SECS` is unset/invalid, and the fallback
/// when the DB-backed `app_settings` row is somehow absent (skeleton mode / pre-seed).
pub const DEFAULT_POLL_INTERVAL_SECS: u32 = 30;
/// Smallest polling interval (seconds) an operator may configure. A tight floor protects both the
/// monitored devices and Yagra, and keeps the anti-stampede jitter window from collapsing.
pub const MIN_POLL_INTERVAL_SECS: u32 = 10;
/// Largest polling interval (seconds) an operator may configure (1 hour).
pub const MAX_POLL_INTERVAL_SECS: u32 = 3600;

// ── Cisco Meraki cadence bounds (own band, NOT the per-node 1h cap — slow tiers must not be
// blocked). Mirror the CHECK constraints in migration 0038; the API validates against these. ──
/// Availability/uplink tier cadence bounds (seconds).
pub const MERAKI_FAST_MIN_SECS: i32 = 60;
pub const MERAKI_FAST_MAX_SECS: i32 = 3600;
/// Traffic tier cadence bounds (seconds).
pub const MERAKI_TRAFFIC_MIN_SECS: i32 = 300;
pub const MERAKI_TRAFFIC_MAX_SECS: i32 = 86_400;
/// Inventory tier cadence bounds (seconds).
pub const MERAKI_INVENTORY_MIN_SECS: i32 = 900;
pub const MERAKI_INVENTORY_MAX_SECS: i32 = 604_800;
/// Hard cap on the per-org request-rate budget (requests/sec) — a safeguard so an operator can't
/// dial polling up to a level that would starve the customer's own Dashboard API usage.
pub const MERAKI_TARGET_RPS_MAX: f64 = 10.0;

/// Default API bind address.
const DEFAULT_API_ADDR: &str = "0.0.0.0:8080";

/// Live-mode configuration. Absent ⇒ skeleton mode.
#[derive(Debug, Clone)]
pub struct Config {
    /// PostgreSQL connection URL (metadata store).
    pub database_url: String,
    /// NATS connection URL (core⇄poller bus).
    pub bus_url: String,
    /// VictoriaMetrics base URL (TSDB).
    pub tsdb_url: String,
    /// Base polling interval, seconds (jitter is applied per node).
    pub poll_interval_secs: u32,
    /// API bind address.
    pub api_addr: String,
    /// When true, read-only endpoints (node list, metrics, alerts) are served without
    /// authentication — a public, read-only dashboard. Default `false`: viewing requires
    /// a valid session (Viewer role), matching the RBAC design.
    pub public_dashboard: bool,
    /// Redis connection URL for volatile poller liveness/assignment mirroring (ADR-004/009).
    /// Optional and **independent of the live/skeleton decision**: Redis is rebuildable, so its
    /// absence only downgrades that mirror to a no-op (ADR-017), it never forces skeleton mode.
    /// Consumed by `run_live` to build the coordinator's [`crate::volatile::VolatileStore`].
    pub redis_url: Option<String>,
    /// VictoriaLogs base URL for the passive-event log store (ADR-024, the 4th data class).
    /// Optional and **independent of the live/skeleton decision** — when set, events are searched
    /// from and written to VictoriaLogs (PostgreSQL then keeps only alert-linked rows); when unset,
    /// events stay entirely in PostgreSQL (backward-compatible). Consumed by `run_live`.
    pub logs_url: Option<String>,
    /// ClickHouse HTTP base URL for the flow store (ADR-031, the 4th store — traffic-flow tier).
    /// Optional and **independent of the live/skeleton decision**, additive/default-OFF: when set,
    /// core subscribes to `yagra.flows`, ensures the ClickHouse schema, and serves the flow-query
    /// API; when unset, the flow receiver is disabled and the API returns `service_unavailable`
    /// (byte-identical to pre-flow behavior). Consumed by `run_live`.
    pub flow_url: Option<String>,
    /// Flow retention in days for the ClickHouse TTL (`YAGRA_FLOW_RETENTION_DAYS`, ADR-031).
    /// Defaults to [`crate::flowstore::DEFAULT_FLOW_RETENTION_DAYS`] when unset/invalid.
    pub flow_retention_days: u32,
    /// High-availability leader election (ADR-016). Default `false`: single active core, all
    /// background work runs unconditionally (byte-identical to pre-HA behavior). When `true`, this
    /// core races for a PostgreSQL advisory lock and runs the coordinator + ingest + alert/notify
    /// singletons **only while it holds the lock**; standbys serve `/healthz` (and 503 `/readyz`)
    /// until they win it. Opt-in so the common single-core deployment is unaffected.
    pub enable_ha: bool,
    /// Human-readable identifier for this core instance, for HA logs / diagnostics
    /// (`YAGRA_CORE_ID`). Not required; `None` falls back to a generic label in logs.
    pub core_id: Option<String>,
    /// Path to the mounted **account nkey seed** file used by the NATS Auth Callout responder to sign
    /// per-poller-scoped user JWTs (ADR-030). Like the KEK (`YAGRA_KEK_FILE`), the *path* is the env
    /// var; the secret itself is a mounted file, never an env var (security.md/ADR-018). `None` (unset)
    /// ⇒ the callout responder is not started and NATS falls back to its static account config
    /// (byte-identical to today). Consumed by `run_live`.
    pub nats_callout_seed_file: Option<String>,
    /// The NATS account minted poller users are placed into (the user JWT audience,
    /// `YAGRA_NATS_CALLOUT_ACCOUNT`). Must match the server's `auth_callout` account. Defaults to the
    /// implicit global account `$G`.
    pub nats_callout_account: String,
    /// The shared poller **bootstrap secret** the callout validates a connecting poller against
    /// (`YAGRA_NATS_POLLER_PASSWORD`, already injected on the remote-bus deployment). `None` ⇒ the
    /// callout cannot authenticate anyone, so the responder is not started. The trust anchor is
    /// swappable later (platform workload identity) without changing this wiring.
    pub nats_poller_password: Option<String>,
    /// Path to the mounted **session signing key** file (`YAGRA_SESSION_KEY_FILE`, 32 bytes / 64 hex).
    /// When set, logins mint stateless HMAC-signed session tokens that any core sharing the key can
    /// verify — the Core HA active/active session substrate (ADR-016 Increment 2a). Like the KEK
    /// (`YAGRA_KEK_FILE`), the *path* is the env var; the key is a mounted file, never an env var
    /// (security.md/ADR-018). `None` (unset) ⇒ opaque per-process tokens, **byte-identical to today**.
    /// A configured-but-unreadable/invalid key is a hard startup error (fail-closed).
    pub session_key_file: Option<String>,
}

impl Config {
    /// Build live config from the environment, or `None` if a required URL is missing.
    pub fn from_env() -> Option<Self> {
        let database_url = std::env::var("YAGRA_DATABASE_URL").ok()?;
        let bus_url = std::env::var("YAGRA_BUS_URL").ok()?;
        let tsdb_url = std::env::var("YAGRA_TSDB_URL").ok()?;
        Some(Self {
            database_url,
            bus_url,
            tsdb_url,
            poll_interval_secs: parse_interval(std::env::var("YAGRA_POLL_INTERVAL_SECS").ok()),
            api_addr: std::env::var("YAGRA_API_ADDR")
                .unwrap_or_else(|_| DEFAULT_API_ADDR.to_owned()),
            public_dashboard: parse_bool(std::env::var("YAGRA_PUBLIC_DASHBOARD").ok()),
            // Read but *not* required: a missing/blank URL leaves Redis mirroring disabled without
            // affecting the live/skeleton gating above.
            redis_url: parse_optional(std::env::var("YAGRA_REDIS_URL").ok()),
            // Optional, independent of live/skeleton: unset ⇒ events stay in PostgreSQL (ADR-024).
            logs_url: parse_optional(std::env::var("YAGRA_LOGS_URL").ok()),
            // Flow store (ADR-031): opt-in. Unset ⇒ flow receiver/API disabled (default-OFF).
            flow_url: parse_optional(std::env::var("YAGRA_CLICKHOUSE_URL").ok()),
            flow_retention_days: parse_retention_days(
                std::env::var("YAGRA_FLOW_RETENTION_DAYS").ok(),
            ),
            // HA (ADR-016): opt-in. Unset ⇒ single-core behavior, no advisory lock.
            enable_ha: parse_bool(std::env::var("YAGRA_ENABLE_HA").ok()),
            core_id: parse_optional(std::env::var("YAGRA_CORE_ID").ok()),
            // Auth Callout (ADR-030): opt-in. Both the seed file and the bootstrap secret must be
            // present for the responder to start; otherwise NATS uses its static account config.
            nats_callout_seed_file: parse_optional(
                std::env::var("YAGRA_NATS_CALLOUT_SEED_FILE").ok(),
            ),
            nats_callout_account: parse_optional(std::env::var("YAGRA_NATS_CALLOUT_ACCOUNT").ok())
                .unwrap_or_else(|| "$G".to_owned()),
            nats_poller_password: parse_optional(std::env::var("YAGRA_NATS_POLLER_PASSWORD").ok()),
            // Signed session tokens (ADR-016 Increment 2a): opt-in via a mounted key file. Unset ⇒
            // opaque per-process tokens (byte-identical to today).
            session_key_file: parse_optional(std::env::var("YAGRA_SESSION_KEY_FILE").ok()),
        })
    }
}

/// Normalize an optional string env var: unset, empty, or all-whitespace ⇒ `None`; otherwise the
/// trimmed value. Keeps a blank `YAGRA_REDIS_URL=` from being treated as a real URL.
fn parse_optional(raw: Option<String>) -> Option<String> {
    raw.map(|s| s.trim().to_owned()).filter(|s| !s.is_empty())
}

/// Parse the flow-retention days (`YAGRA_FLOW_RETENTION_DAYS`), defaulting on unset/invalid and
/// clamping to a sane band so a typo can't set a 0-day (immediate-expiry) or absurd TTL.
fn parse_retention_days(raw: Option<String>) -> u32 {
    raw.and_then(|s| s.trim().parse::<u32>().ok())
        .filter(|&n| n > 0)
        .map(|n| n.clamp(1, 3650))
        .unwrap_or(crate::flowstore::DEFAULT_FLOW_RETENTION_DAYS)
}

/// Parse a boolean flag. Truthy: `1`/`true`/`yes`/`on` (case-insensitive); everything
/// else (including unset) is `false`.
fn parse_bool(raw: Option<String>) -> bool {
    matches!(
        raw.as_deref()
            .map(str::trim)
            .map(str::to_ascii_lowercase)
            .as_deref(),
        Some("1" | "true" | "yes" | "on")
    )
}

/// Whether a polling interval (seconds) is within the operator-configurable band `[MIN, MAX]`.
/// Shared by every write path (API profile + default-interval validation) so the bound lives in
/// one place.
#[must_use]
pub fn interval_in_bounds(secs: u32) -> bool {
    (MIN_POLL_INTERVAL_SECS..=MAX_POLL_INTERVAL_SECS).contains(&secs)
}

/// Parse a polling interval and clamp it into the allowed `[MIN, MAX]` band, defaulting on bad
/// input. This is only the *initial* default (seeded into `app_settings` on first boot); the
/// runtime-editable value lives in the DB thereafter.
fn parse_interval(raw: Option<String>) -> u32 {
    raw.and_then(|s| s.parse::<u32>().ok())
        .filter(|&n| n > 0)
        .map(|n| n.clamp(MIN_POLL_INTERVAL_SECS, MAX_POLL_INTERVAL_SECS))
        .unwrap_or(DEFAULT_POLL_INTERVAL_SECS)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interval_defaults_when_absent_or_invalid() {
        assert_eq!(parse_interval(None), DEFAULT_POLL_INTERVAL_SECS);
        assert_eq!(
            parse_interval(Some("abc".into())),
            DEFAULT_POLL_INTERVAL_SECS
        );
        assert_eq!(parse_interval(Some("0".into())), DEFAULT_POLL_INTERVAL_SECS);
    }

    #[test]
    fn interval_parses_valid_value() {
        assert_eq!(parse_interval(Some("60".into())), 60);
    }

    #[test]
    fn interval_bounds_check() {
        assert!(!interval_in_bounds(9));
        assert!(interval_in_bounds(MIN_POLL_INTERVAL_SECS)); // 10
        assert!(interval_in_bounds(1800));
        assert!(interval_in_bounds(MAX_POLL_INTERVAL_SECS)); // 3600
        assert!(!interval_in_bounds(3601));
        assert!(!interval_in_bounds(0));
    }

    #[test]
    fn interval_clamps_into_allowed_band() {
        // Below the floor and above the ceiling are pulled into [MIN, MAX].
        assert_eq!(parse_interval(Some("5".into())), MIN_POLL_INTERVAL_SECS);
        assert_eq!(
            parse_interval(Some("999999".into())),
            MAX_POLL_INTERVAL_SECS
        );
        assert_eq!(parse_interval(Some("10".into())), MIN_POLL_INTERVAL_SECS);
        assert_eq!(parse_interval(Some("3600".into())), MAX_POLL_INTERVAL_SECS);
    }

    #[test]
    fn optional_is_none_when_absent_or_blank() {
        assert_eq!(parse_optional(None), None);
        assert_eq!(parse_optional(Some(String::new())), None);
        assert_eq!(parse_optional(Some("   ".into())), None);
        assert_eq!(
            parse_optional(Some("  redis://cache:6379  ".into())),
            Some("redis://cache:6379".to_owned())
        );
    }

    #[test]
    fn bool_is_false_unless_explicitly_truthy() {
        assert!(!parse_bool(None));
        assert!(!parse_bool(Some("".into())));
        assert!(!parse_bool(Some("false".into())));
        assert!(!parse_bool(Some("0".into())));
        assert!(parse_bool(Some("1".into())));
        assert!(parse_bool(Some("true".into())));
        assert!(parse_bool(Some(" YES ".into())));
        assert!(parse_bool(Some("On".into())));
    }
}
