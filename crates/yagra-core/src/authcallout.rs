// SPDX-License-Identifier: AGPL-3.0-only
//! NATS **Auth Callout** responder (ADR-030): core as the NATS auth service.
//!
//! When per-poller credential scoping is enabled (a callout account seed is mounted, see
//! `config::Config::nats_callout_seed_file`), NATS delegates every poller connection to core over the
//! system subject `$SYS.REQ.USER.AUTH`. For each request we validate the shared bootstrap secret and
//! mint a NATS user JWT scoped to only that poller's own subjects (`yagra.poller.assign.{id}` + its
//! pool's jobs/discovery, [`yagra_authz::allow_list`]), so a compromised poller cannot subscribe to
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
use yagra_authz::{AccountSigner, Decision};

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

/// Subscribe to `$SYS.REQ.USER.AUTH` and answer each request with a per-poller-scoped user JWT (or a
/// signed rejection). Loops until the subscription ends or the task is cancelled on shutdown.
pub async fn run_auth_callout(
    client: Client,
    signer: Arc<AccountSigner>,
    bootstrap_secret: String,
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
        match signer.handle_request(request_jwt, &bootstrap_secret, unix_now_secs()) {
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
                // Unparseable request: we can't even address a scoped reply. Count and drop.
                metrics::counter!("yagra_core_authz_denied_total", "reason" => "parse_error")
                    .increment(1);
            }
        }
    }
    tracing::info!("auth-callout responder stopped");
}
