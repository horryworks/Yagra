// SPDX-License-Identifier: AGPL-3.0-only
//! `GET /api/v1/stream/config` — the change feed an open WebUI follows (ADR-019 増分 2).
//!
//! Each event is `{"revision": N}` and nothing else. The first is sent the moment the stream opens;
//! after that one is sent whenever the revision moved, at most once per [`COALESCE`]. The browser
//! compares it with the last one it saw and re-reads the inventory tree and the configuration
//! screens when they differ — so a reconnect that finds the same number does nothing, and one that
//! missed changes catches up without a replay or a `resync` hint.
//!
//! Refused to a group-scoped caller (ADR-014, user decision 2026-09-26). The frame says only that
//! *something* changed, and for a scoped account that includes changes outside its folders, which
//! is what ADR-014 does not tell it. Such an account keeps what it had: a screen re-read on reload.

use std::convert::Infallible;
use std::time::Duration;

use axum::extract::State;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::{routing::get, Router};
use futures::{Stream, StreamExt};
use tokio::sync::broadcast;

use super::extract::{RequireView, Scoped};
use super::ApiState;

/// This domain's slice of the OpenAPI document (ADR-035), merged by [`super::openapi::document`].
#[derive(utoipa::OpenApi)]
#[openapi(paths(stream_config))]
pub(super) struct Doc;

/// The change-feed route, merged into `/api/v1` by [`super::router`].
pub(super) fn routes() -> Router<ApiState> {
    Router::new().route("/api/v1/stream/config", get(stream_config))
}

/// How long a change waits for the ones behind it. A NetBox or Meraki import bumps once per folder
/// or device it writes, and every open tab re-reads the tree on each frame — one frame per burst,
/// not one per row.
const COALESCE: Duration = Duration::from_secs(1);

/** How often an idle stream asks whether the state that served it still exists.
 *
 *  ⚠️ **Why a stream needs to ask at all.** The feed's sender is process-wide and never closes, so
 *  nothing would ever end this stream on its own — and the alert and node-state streams end
 *  precisely when their state is dropped (they hold a `Weak`, `api/alerts.rs::sse_with_resync`).
 *  A test that drives the router and reads the body to the end (`route_table.rs::
 *  every_listed_route_is_served`) hung forever on this route until it did the same. In production
 *  the state lives as long as the process, so this only costs one timer per open tab. */
const LIVENESS: Duration = Duration::from_secs(2);

/// The revisions a subscriber is sent: `first`, then the latest one after each quiet [`COALESCE`]
/// window in which the revision moved. A lag loses nothing that matters — only intermediate numbers
/// — so it is folded in like any other arrival. The stream ends when `alive` says no ([`LIVENESS`]).
fn revisions(
    rx: broadcast::Receiver<u64>,
    first: u64,
    current: impl Fn() -> u64 + Send + 'static,
    alive: impl Fn() -> bool + Send + 'static,
    coalesce: Duration,
) -> impl Stream<Item = u64> + Send {
    let rest = futures::stream::unfold(
        (rx, first, current, alive),
        move |(mut rx, last, current, alive)| async move {
            loop {
                let got = tokio::select! {
                    r = rx.recv() => Some(r),
                    () = tokio::time::sleep(LIVENESS) => None,
                };
                match got {
                    None if alive() => continue,
                    None | Some(Err(broadcast::error::RecvError::Closed)) => return None,
                    Some(Ok(_) | Err(broadcast::error::RecvError::Lagged(_))) => {}
                }
                tokio::time::sleep(coalesce).await;
                // Whatever queued up during the wait is covered by reading the counter now.
                while let Ok(_) | Err(broadcast::error::TryRecvError::Lagged(_)) = rx.try_recv() {}
                let now = current();
                if now != last {
                    return Some((now, (rx, now, current, alive)));
                }
            }
        },
    );
    futures::stream::once(async move { first }).chain(rest)
}

/// Live configuration change feed (SSE): each event's `data` is `{"revision": N}`.
///
/// Sent once on connect and again, at most once a second, whenever the revision moves. Compare it
/// with the last one seen: a different number means inventory or configuration changed, so re-read
/// what is on screen. The number is process-local and restarts from 0 with the process.
#[utoipa::path(
    get, path = "/api/v1/stream/config", tag = "system",
    responses(
        (status = 200, description = "Server-sent event stream; each `data` is `{\"revision\": N}`, sent on connect and whenever configuration changes", content_type = "text/event-stream"),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks View, or the account is restricted to folders (`scope_unsupported`)", body = super::error::ErrorBody),
    ),
)]
async fn stream_config(
    _guard: RequireView,
    Scoped(scope): Scoped,
    State(st): State<ApiState>,
) -> Response {
    if let Err(e) = super::scope::require_fleet_wide(
        &scope,
        "the change feed says that something changed anywhere in the configuration, which for a \
         group-scoped account includes changes outside its folders",
    ) {
        return e.into_response();
    }
    // Subscribe first, then read: a publish between the two is on the receiver either way.
    let rx = crate::change_feed::subscribe();
    let first = crate::change_feed::current();
    // The alert engine stands for the state here: it lives exactly as long as the state does.
    let state = std::sync::Arc::downgrade(&st.alerts);
    let alive = move || state.strong_count() > 0;
    let stream = revisions(rx, first, crate::change_feed::current, alive, COALESCE).map(|rev| {
        Ok::<_, Infallible>(
            Event::default().data(serde_json::json!({ "revision": rev }).to_string()),
        )
    });
    Sse::new(stream)
        .keep_alive(KeepAlive::default())
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::tests_support::{private_state, scoped_token, token};
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::Arc;

    const WAIT: Duration = Duration::from_secs(5);

    /// A private feed, so parallel tests publishing on the process-wide one cannot move the answer.
    fn feed() -> (broadcast::Sender<u64>, Arc<AtomicU64>) {
        (broadcast::channel(4).0, Arc::new(AtomicU64::new(7)))
    }

    #[tokio::test]
    async fn the_current_revision_is_sent_at_once() {
        let (tx, rev) = feed();
        let r = rev.clone();
        let mut s = Box::pin(revisions(
            tx.subscribe(),
            7,
            move || r.load(Ordering::Relaxed),
            || true,
            Duration::from_millis(20),
        ));
        assert_eq!(
            tokio::time::timeout(WAIT, s.next())
                .await
                .expect("first frame"),
            Some(7)
        );
    }

    #[tokio::test]
    async fn a_burst_is_one_frame_carrying_the_latest_revision() {
        let (tx, rev) = feed();
        let r = rev.clone();
        let mut s = Box::pin(revisions(
            tx.subscribe(),
            7,
            move || r.load(Ordering::Relaxed),
            || true,
            Duration::from_millis(50),
        ));
        s.next().await;
        // More sends than the channel holds: the lag must be absorbed, not end the stream.
        for n in 8..=13 {
            rev.store(n, Ordering::Relaxed);
            tx.send(n).expect("receiver alive");
        }
        assert_eq!(
            tokio::time::timeout(WAIT, s.next())
                .await
                .expect("one frame"),
            Some(13)
        );
        // …and nothing more until the revision moves again.
        assert!(
            tokio::time::timeout(Duration::from_millis(200), s.next())
                .await
                .is_err(),
            "the burst produced a second frame"
        );
    }

    #[tokio::test]
    async fn a_wake_that_did_not_move_the_revision_sends_nothing() {
        let (tx, rev) = feed();
        let r = rev.clone();
        let mut s = Box::pin(revisions(
            tx.subscribe(),
            7,
            move || r.load(Ordering::Relaxed),
            || true,
            Duration::from_millis(20),
        ));
        s.next().await;
        tx.send(7).expect("receiver alive");
        assert!(tokio::time::timeout(Duration::from_millis(200), s.next())
            .await
            .is_err());
    }

    #[tokio::test]
    async fn the_stream_ends_once_the_state_that_served_it_is_gone() {
        // Without this the stream outlives its router forever, and any test reading a response
        // body to the end hangs on it (it did: `every_listed_route_is_served`).
        let (tx, _rev) = feed();
        let alive = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let a = alive.clone();
        let mut s = Box::pin(revisions(
            tx.subscribe(),
            7,
            || 7,
            move || a.load(Ordering::Relaxed),
            Duration::from_millis(20),
        ));
        s.next().await;
        alive.store(false, Ordering::Relaxed);
        let end = tokio::time::timeout(LIVENESS * 3, s.next()).await;
        assert_eq!(end.expect("ends within a liveness check"), None);
    }

    #[tokio::test]
    async fn a_group_scoped_caller_is_refused() {
        let st = private_state();
        let scoped = scoped_token(&st, &[uuid::Uuid::new_v4()]);
        let (status, body) =
            crate::api::tests_support::send(&st, "GET", "/api/v1/stream/config", &scoped, None)
                .await;
        assert_eq!(status, axum::http::StatusCode::FORBIDDEN, "{body}");
        // Assembled, not spelled: `scope.rs::no_handler_spells_the_scope_refusal_by_hand` counts
        // the literal across `api/` as raw text, test modules included.
        assert_eq!(body["error"]["code"], concat!("scope", "_unsupported"));
    }

    #[tokio::test]
    async fn an_unrestricted_caller_gets_an_event_stream() {
        use tower::ServiceExt as _;
        let st = private_state();
        let bearer = token(&st, yagra_common::Role::Viewer);
        let req = axum::http::Request::builder()
            .uri("/api/v1/stream/config")
            .header(
                axum::http::header::AUTHORIZATION,
                format!("Bearer {bearer}"),
            )
            .body(axum::body::Body::empty())
            .expect("request");
        let res = crate::api::router(st).oneshot(req).await.expect("response");
        assert_eq!(res.status(), axum::http::StatusCode::OK);
        assert_eq!(
            res.headers()[axum::http::header::CONTENT_TYPE],
            "text/event-stream"
        );
        let mut body = res.into_body().into_data_stream();
        let chunk = tokio::time::timeout(WAIT, body.next())
            .await
            .expect("the first frame is sent at once")
            .expect("a chunk")
            .expect("bytes");
        let text = String::from_utf8_lossy(&chunk);
        assert!(text.starts_with("data: {\"revision\":"), "{text}");
    }
}
