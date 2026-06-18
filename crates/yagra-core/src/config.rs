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
        })
    }
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
