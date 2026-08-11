// SPDX-License-Identifier: AGPL-3.0-only
//! NATS subject scheme for core⇄poller messaging.
//!
//! Jobs are published per **poller pool** (ADR-009) so pollers subscribe only to the pool(s)
//! they serve; results come back on a single subject core consumes. Centralising the naming
//! here keeps the wire contract in one place. The actual NATS connection is an I/O adapter
//! (live-only); this addressing logic is pure and testable.

/// Root subject namespace.
pub(crate) const ROOT: &str = "yagra";

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

/// Subject pollers replay **buffered** results on after a core↔poller partition heals
/// (store-and-forward, Phase 3). Kept **separate** from [`results`] on purpose: core imports these
/// to the TSDB at their original `at_unix_ms` (backfill) but must **not** run alert evaluation over
/// them (replaying a burst of stale samples would re-fire resolved alerts). The separate subject is
/// also the N-1 safety valve — an older core that predates store-and-forward simply never subscribes
/// here, so a newer poller's backfill is silently dropped (a metrics gap) instead of flooding alerts.
#[must_use]
pub fn results_backfill() -> String {
    format!("{ROOT}.results.backfill")
}

/// Subject pollers publish passive events (syslog/traps) on, consumed by core.
#[must_use]
pub fn events() -> String {
    format!("{ROOT}.events")
}

/// Subject pollers publish **edge-aggregated flow batches** (NetFlow/IPFIX/sFlow) on, consumed by
/// core and written to ClickHouse (ADR-031). Kept off the [`results`]/[`events`] streams on purpose:
/// flow is a high-volume, best-effort, loss-tolerant tier, and the separate subject is the N-1
/// safety valve — an older core that predates flow simply never subscribes here, so a newer poller's
/// flow batches are silently dropped instead of erroring.
#[must_use]
pub fn flows() -> String {
    format!("{ROOT}.flows")
}

/// Subject pollers publish **verbatim flow-export datagrams** on, consumed by core's forwarder
/// (ADR-034 Increment 2). Deliberately separate from [`flows`]: that carries edge-aggregated batches
/// for ClickHouse, which are lossy by construction (1-minute buckets, top-N, folded 5-tuples) and so
/// can never reconstruct what the exporter sent. Forwarding needs the bytes themselves.
///
/// The separate subject is also the N-1 safety valve — a core that predates forwarding never
/// subscribes here, so a newer poller's datagrams are dropped by the broker rather than
/// mis-consumed. Note `yagra.flows` is subscribed as an **exact** token, so it does not match this.
#[must_use]
pub fn flows_raw() -> String {
    format!("{ROOT}.flows.raw")
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

// ── Distributed poller pool (ADR-009/020) — control plane subjects ──────────────────

/// Subject pollers publish liveness/telemetry heartbeats on; core consumes (ADR-009). A single
/// shared subject (no queue group) — every heartbeat reaches core's registry.
#[must_use]
pub fn heartbeat() -> String {
    format!("{ROOT}.poller.heartbeat")
}

/// Subject pollers publish snapshot requests on; core consumes (ADR-020). Plain pub/sub, **not**
/// request-reply — the reply is several snapshot chunks pushed on the poller's assignment subject.
#[must_use]
pub fn sync_request() -> String {
    format!("{ROOT}.poller.sync_request")
}

/// Subject core publishes a specific poller's working-set sync on — snapshot chunks and deltas
/// (ADR-020). One subject **per poller** preserves the ordering that `seq` gap-detection relies on;
/// it is addressed to a single poller (no queue group). The id is sanitized via [`sanitize_token`]
/// so an FQDN or arbitrary id is a legal single NATS token, e.g. `yagra.poller.assign.edge-1`.
#[must_use]
pub fn assignment_for(poller: &str) -> String {
    format!("{ROOT}.poller.assign.{}", sanitize_token(poller))
}

/// Subject core publishes one poller's upgrade command on, e.g. `yagra.poller.upgrade.edge-1`
/// (ADR-051).
///
/// **Its own token, not a variant on [`assignment_for`]'s stream, and that is the version gate.** A
/// poller build with no upgrade support never subscribes here, so the command is delivered to
/// nobody and the site stays where it is — which is precisely the desired N-1 behaviour, obtained
/// structurally rather than by a check anyone could forget. Riding on the assignment subject would
/// instead put a control message into the stream `seq` gap-detection guards, where an older poller
/// dropping the envelope would take that round's *working-set update* down with it.
///
/// ⚠️ `yagra.poller.assign.>` does **not** cover this. Both bus allow-lists need their own entry —
/// `yagra-authz`'s per-poller grant and the static account in `docker/nats/nats-server.conf` — and
/// missing either is a runtime denial with no compile error.
#[must_use]
pub fn upgrade_for(poller: &str) -> String {
    format!("{ROOT}.poller.upgrade.{}", sanitize_token(poller))
}

/// Subject core publishes pool-scoped discovery jobs on (the discovery analogue of
/// [`jobs_for_pool`]), e.g. `yagra.discovery.jobs.tokyo`. Pollers in the pool queue-subscribe.
#[must_use]
pub fn discovery_jobs_for_pool(pool: &str) -> String {
    format!("{ROOT}.discovery.jobs.{pool}")
}

// ── Core⇄core control plane (ADR-016 Increment 2 — active/active API) ────────────────

/// Subject a core publishes **session-revocation** notices on (logout / user disable-demote-reset-
/// delete), consumed by every *other* core so a stateless signed token revoked on one core is denied
/// on all (Core HA active/active, ADR-016 Increment 2a). Plain `.subscribe()` fan-out — every core
/// receives every revocation. Additive: an N-1 core never subscribes here, so nothing breaks; the
/// durable `auth_revocations` table is the source of truth a restarted/promoted core cold-loads from.
/// Carries only token *hashes* / user ids — never a raw token or password (security.md).
#[must_use]
pub fn auth_revoke() -> String {
    format!("{ROOT}.auth.revoke")
}

/// Make `raw` a legal single NATS subject token: keep `[A-Za-z0-9_-]`, replace every other
/// character (notably the dots in an FQDN — NATS treats `.` as a token separator) with `-`. An
/// empty input maps to `"unknown"` so the subject never collapses to an empty token.
#[must_use]
pub fn sanitize_token(raw: &str) -> String {
    if raw.is_empty() {
        return "unknown".to_owned();
    }
    raw.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '-'
            }
        })
        .collect()
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

    #[test]
    fn results_backfill_subject_is_stable_and_distinct() {
        assert_eq!(results_backfill(), "yagra.results.backfill");
        // Must not collide with the live subject — an old core subscribed to `yagra.results`
        // must NOT receive backfill (that separation is the N-1 safety valve).
        assert_ne!(results_backfill(), results());
        // ...and it must not sit under a `yagra.results.*` wildcard the live consumer might use.
        // The live consumer subscribes the exact token `yagra.results`, so `.backfill` is isolated.
        assert!(results_backfill().starts_with("yagra.results."));
    }

    #[test]
    fn events_subject_is_stable() {
        assert_eq!(events(), "yagra.events");
    }

    #[test]
    fn flows_subject_is_stable_and_distinct() {
        assert_eq!(flows(), "yagra.flows");
        // Must be isolated from results/events so an N-1 core (no flow subscriber) drops flow
        // batches instead of mis-consuming them.
        assert_ne!(flows(), results());
        assert_ne!(flows(), events());
        assert!(!flows().starts_with("yagra.results"));
    }

    #[test]
    fn raw_flow_subject_is_stable_and_isolated_from_the_aggregate_stream() {
        assert_eq!(flows_raw(), "yagra.flows.raw");
        // The ClickHouse consumer subscribes the exact token `yagra.flows`, which in NATS does not
        // match `yagra.flows.raw` — so raw datagrams can never be mis-parsed as FlowBatches.
        assert_ne!(flows_raw(), flows());
        assert!(flows_raw().starts_with("yagra.flows."));
        assert_ne!(flows_raw(), events());
    }

    #[test]
    fn control_plane_subjects_are_stable() {
        assert_eq!(heartbeat(), "yagra.poller.heartbeat");
        assert_eq!(sync_request(), "yagra.poller.sync_request");
        assert_eq!(
            discovery_jobs_for_pool("tokyo"),
            "yagra.discovery.jobs.tokyo"
        );
    }

    #[test]
    fn assignment_subject_sanitizes_the_poller_id() {
        assert_eq!(assignment_for("edge-1"), "yagra.poller.assign.edge-1");
        // An FQDN's dots would otherwise split into extra NATS tokens — they become dashes.
        assert_eq!(
            assignment_for("poller.tokyo.example.com"),
            "yagra.poller.assign.poller-tokyo-example-com"
        );
    }

    #[test]
    fn sanitize_token_keeps_allowed_chars_and_replaces_the_rest() {
        // Allowed set passes through untouched.
        assert_eq!(sanitize_token("Edge_Poller-09"), "Edge_Poller-09");
        // Dots (FQDN), colons, slashes, spaces → dashes.
        assert_eq!(sanitize_token("host.name:10/eth0 a"), "host-name-10-eth0-a");
    }

    #[test]
    fn sanitize_token_maps_empty_to_unknown() {
        assert_eq!(sanitize_token(""), "unknown");
    }

    #[test]
    fn auth_revoke_subject_is_stable_and_distinct() {
        assert_eq!(auth_revoke(), "yagra.auth.revoke");
        // Additive core⇄core subject: must not collide with any poller-facing subject namespace.
        assert_ne!(auth_revoke(), results());
        assert_ne!(auth_revoke(), events());
        assert!(!auth_revoke().starts_with("yagra.jobs"));
        assert!(!auth_revoke().starts_with("yagra.poller"));
    }
}
