//! NATS subject scheme for core⇄poller messaging.
//!
//! Jobs are published per **poller pool** (ADR-009) so pollers subscribe only to the pool(s)
//! they serve; results come back on a single subject core consumes. Centralising the naming
//! here keeps the wire contract in one place. The actual NATS connection is an I/O adapter
//! (live-only); this addressing logic is pure and testable.

/// Root subject namespace.
pub const ROOT: &str = "yagra";

/// Subject a poller in `pool` subscribes to for its jobs, e.g. `yagra.jobs.tokyo`.
#[must_use]
pub fn jobs_for_pool(pool: &str) -> String {
    format!("{ROOT}.jobs.{pool}")
}

/// Wildcard subject matching jobs for every pool (`yagra.jobs.*`) — for a single-pool MVP
/// or an all-pools consumer.
#[must_use]
pub fn jobs_all() -> String {
    format!("{ROOT}.jobs.*")
}

/// Subject pollers publish results on, consumed by core.
#[must_use]
pub fn results() -> String {
    format!("{ROOT}.results")
}

/// Subject core publishes discovery sweep jobs on; pollers subscribe (queue group).
#[must_use]
pub fn discovery_jobs() -> String {
    format!("{ROOT}.discovery.jobs")
}

/// Subject pollers publish discovery results on, consumed by core.
#[must_use]
pub fn discovery_results() -> String {
    format!("{ROOT}.discovery.results")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pool_jobs_subject() {
        assert_eq!(jobs_for_pool("tokyo"), "yagra.jobs.tokyo");
    }

    #[test]
    fn wildcard_matches_pool_subject_namespace() {
        // The poller subject must sit under the wildcard's namespace.
        let wild = jobs_all();
        assert_eq!(wild, "yagra.jobs.*");
        assert!(jobs_for_pool("osaka").starts_with("yagra.jobs."));
    }

    #[test]
    fn results_subject_is_stable() {
        assert_eq!(results(), "yagra.results");
    }
}
