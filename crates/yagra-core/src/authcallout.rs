// SPDX-License-Identifier: AGPL-3.0-only
//! NATS **Auth Callout** responder (ADR-030): core as the NATS auth service.
//!
//! When per-poller credential scoping is enabled (a callout account seed is mounted, see
//! `config::Config::nats_callout_seed_file`), NATS delegates every poller connection to core over the
//! system subject `$SYS.REQ.USER.AUTH`. For each request we validate the shared bootstrap secret and
//! mint a NATS user JWT scoped to only that poller's own subjects (`yagra.poller.assign.{id}` + its
//! pool's jobs/discovery, built by `yagra_authz`'s internal `allow_list`), so a compromised poller
//! cannot subscribe to
//! another poller's — i.e. another device's — credentials off the bus.
//!
//! Runs on **every** core (queue-subscribed), not just the leader, so authentication survives a
//! failover. The signing/JWT/allow-list logic is the pure [`yagra_authz`] crate; this module is just
//! the async-nats request/reply plumbing. **No secret or minted JWT is ever logged** (security.md) —
//! only issue/deny counts.

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use async_nats::Client;
use futures::StreamExt;
use yagra_authz::{AccountSigner, Decision, Expected};

/// The NATS system subject the server publishes auth-callout requests on.
const AUTH_SUBJECT: &str = "$SYS.REQ.USER.AUTH";
/// Queue group so multiple cores share the work (and one always answers during a failover).
const AUTH_QUEUE: &str = "yagra-authz";

fn unix_now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Start the responder when a callout account seed is mounted **and** the poller bootstrap secret
/// is set. Moved here from `run_live` by ADR-090, together with the seed-file read.
///
/// **Runs on EVERY core, deliberately not leader-gated** — it is queue-subscribed (see
/// [`AUTH_QUEUE`]), so one core always answers and authentication survives a failover.
///
/// Either input unset ⇒ NATS uses its static account config and this is a no-op, byte-identical to
/// a deployment that never enabled ADR-030. A seed that is unreadable or invalid is logged at
/// `error` and leaves scoping disabled rather than aborting startup: refusing to boot would take
/// the whole deployment down over a feature that is off by default.
///
/// ⚠️ The seed itself is never logged. What *is* logged is the issuer **public** key, which is not a
/// secret — the operator has to paste it into `nats-server.conf`'s `auth_callout { issuer }`, and
/// startup is the only place it can be read from.
pub(crate) fn start(
    seed_file: Option<&str>,
    account: &str,
    poller_password: Option<String>,
    client: Client,
    pollers: Arc<crate::pollers::PollerRepo>,
    shutdown: &yagra_telemetry::CancellationToken,
) {
    let (Some(seed_path), Some(secret)) = (seed_file, poller_password) else {
        return;
    };
    let seed = match std::fs::read_to_string(seed_path) {
        Ok(s) => s,
        Err(e) => {
            tracing::error!(
                error = %e,
                path = %seed_path,
                "auth-callout: cannot read seed file; per-poller credential scoping disabled"
            );
            return;
        }
    };
    let signer = match AccountSigner::from_seed(seed.trim(), account.to_owned()) {
        Ok(s) => s,
        Err(e) => {
            tracing::error!(
                error = %e,
                "auth-callout: invalid account seed; per-poller credential scoping disabled"
            );
            return;
        }
    };
    tracing::info!(
        issuer = %signer.issuer_public_key(),
        account = %account,
        "auth-callout enabled (ADR-030) — set this issuer in nats-server.conf auth_callout"
    );
    yagra_telemetry::spawn_cancellable(
        shutdown,
        // The inventory is part of the decision (ADR-065 Inc.3): an id with no row here is refused,
        // and one with a token of its own is no longer opened by the deployment-wide secret.
        run_auth_callout(client, Arc::new(signer), secret, pollers),
    );
}

/// Subscribe to `$SYS.REQ.USER.AUTH` and answer each request with a per-poller-scoped user JWT (or a
/// signed rejection). Loops until the subscription ends or the task is cancelled on shutdown.
pub async fn run_auth_callout(
    client: Client,
    signer: Arc<AccountSigner>,
    bootstrap_secret: String,
    pollers: Arc<crate::pollers::PollerRepo>,
) {
    let mut sub = match client
        .queue_subscribe(AUTH_SUBJECT, AUTH_QUEUE.to_owned())
        .await
    {
        Ok(s) => s,
        Err(e) => {
            tracing::error!(error = %e, "auth-callout: subscribe failed; per-poller credential scoping is DOWN");
            return;
        }
    };
    tracing::info!("auth-callout responder active — per-poller NATS credential scoping (ADR-030)");

    while let Some(msg) = sub.next().await {
        // A callout request always carries a reply subject; anything else isn't ours.
        let Some(reply) = msg.reply.clone() else {
            continue;
        };
        let request_jwt = match std::str::from_utf8(&msg.payload) {
            Ok(s) => s,
            Err(_) => {
                metrics::counter!("yagra_core_authz_denied_total", "reason" => "bad_utf8")
                    .increment(1);
                continue;
            }
        };
        // Parse first, so the id the connection is claiming can be looked up before anything is
        // compared (ADR-065 Inc.3). The id is self-asserted — that is exactly why it is the key to
        // a row rather than something to trust.
        let pending = match AccountSigner::parse(request_jwt) {
            Ok(p) => p,
            Err(_) => {
                metrics::counter!("yagra_core_authz_denied_total", "reason" => "parse_error")
                    .increment(1);
                continue;
            }
        };
        // No id at all: hand it to `decide`, which refuses with the reason that names the actual
        // problem. Looking `None` up would report "unknown poller", which would send an operator
        // hunting for an inventory row for a connection that never named one.
        let material = match pending.poller_id() {
            Some(id) => pollers.auth_material(id).await,
            None => Some(crate::pollers::PollerAuth::Bootstrap),
        };
        let expected = match &material {
            Some(crate::pollers::PollerAuth::Token(hash)) => Expected::TokenHash(hash),
            Some(crate::pollers::PollerAuth::Bootstrap) => Expected::Shared(&bootstrap_secret),
            None => Expected::Unknown,
        };
        match signer.decide(&pending, expected, unix_now_secs()) {
            Ok(handled) => {
                match &handled.decision {
                    Decision::Issued { pool, .. } => {
                        metrics::counter!("yagra_core_authz_issued_total").increment(1);
                        tracing::debug!(%pool, "auth-callout: issued scoped credential");
                    }
                    Decision::Denied { reason } => {
                        metrics::counter!("yagra_core_authz_denied_total", "reason" => reason.as_str())
                            .increment(1);
                    }
                }
                if let Err(e) = client.publish(reply, handled.response_jwt.into()).await {
                    tracing::warn!(error = %e, "auth-callout: failed to publish response");
                }
            }
            Err(_) => {
                // Signing or encoding failed after a clean parse — we have a reply subject but
                // nothing to put on it. Count and drop rather than publishing a malformed answer.
                metrics::counter!("yagra_core_authz_denied_total", "reason" => "sign_error")
                    .increment(1);
            }
        }
    }
    tracing::info!("auth-callout responder stopped");
}

#[cfg(test)]
mod tests {
    use super::*;

    const SRC: &str = include_str!("authcallout.rs");

    /// Executable code above the tests, comments stripped — see `dns_check.rs` for why both.
    fn production_source() -> String {
        SRC.split("#[cfg(test)]")
            .next()
            .expect("split always yields a first element")
            .lines()
            .filter(|l| !l.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn the_clock_never_panics_and_never_goes_backwards_into_a_negative() {
        // The signed JWT's issued-at comes from here. A clock before the epoch would otherwise
        // panic the responder — taking every poller's authentication down with it — so it
        // saturates at 0 and lets the signer reject on its own terms.
        let now = unix_now_secs();
        assert!(now > 1_600_000_000, "the wall clock should be past 2020");
        assert_eq!(
            SystemTime::UNIX_EPOCH
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0),
            0
        );
    }

    #[test]
    fn the_responder_is_queue_subscribed_so_a_failover_still_authenticates() {
        // Every core runs this, not just the leader: if it were a plain subscribe, all of them
        // would answer the same request, and if it were leader-gated a failover would leave
        // pollers unable to connect at exactly the moment the fleet needs them.
        let src = production_source();
        assert!(src.contains("queue_subscribe(AUTH_SUBJECT, AUTH_QUEUE.to_owned())"));
        assert_eq!(AUTH_SUBJECT, "$SYS.REQ.USER.AUTH");
        assert_eq!(AUTH_QUEUE, "yagra-authz");
    }

    #[test]
    fn no_secret_or_minted_credential_is_ever_logged() {
        // security.md: the bootstrap secret and the minted JWT are the crown jewels on this path.
        // Only the pool name and counts may be recorded, so a `%request_jwt` or `%bootstrap_secret`
        // reaching a tracing macro is the failure this pins.
        let src = production_source();
        for forbidden in [
            "%request_jwt",
            "?request_jwt",
            "%bootstrap_secret",
            "?bootstrap_secret",
            "response_jwt = ",
            "%handled.response_jwt",
        ] {
            assert!(
                !src.contains(forbidden),
                "a credential ({forbidden}) is being logged — security.md forbids it"
            );
        }
    }

    #[test]
    fn every_rejection_path_is_counted_with_a_bounded_reason_label() {
        // The reason becomes a metric label, so it must come from a closed set — a free-text
        // reason here would be an unbounded label, the cardinality trap monitoring-conventions
        // names as the single biggest risk in this codebase.
        let src = production_source();
        for reason in ["\"bad_utf8\"", "\"parse_error\""] {
            assert!(
                src.contains(reason),
                "the {reason} denial is no longer counted"
            );
        }
        assert!(
            src.contains("reason.as_str()"),
            "the signer's reason must stay a typed token"
        );
        assert!(src.contains("yagra_core_authz_issued_total"));
    }

    #[test]
    fn a_request_without_a_reply_subject_is_skipped_rather_than_answered() {
        // Anything on this subject without a reply is not a callout request; answering it would
        // publish a signed credential to a subject nobody asked on.
        assert!(production_source().contains("let Some(reply) = msg.reply.clone() else"));
    }
}
