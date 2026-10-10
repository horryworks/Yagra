// SPDX-License-Identifier: AGPL-3.0-only
//! The outbound HTTP client core builds for its own use (ADR-184).
//!
//! Ten places in this crate built a `reqwest` client, each spelling the same three decisions —
//! how long to wait, whether to follow a redirect, what to call itself — and they had drifted:
//! the VictoriaLogs store had **no timeout at all**, so a hung socket there held the event writer
//! for as long as the socket stayed open, and two notification clients carried a comment claiming
//! their fallback kept the no-redirect policy, which it did not.
//!
//! So both decisions are **arguments with no default**. A new caller has to say how long it will
//! wait and whether a redirect may be followed, rather than inherit an answer.
//!
//! ## What is *not* built here
//!
//! The clients that talk to **monitored devices** — `yagra-transport`'s HTTP check and its Meraki
//! client. Their TLS policy belongs to the operator (a URL check may be told to accept a
//! self-signed certificate), and mixing that into the client core uses for its own stores and
//! integrations is how a relaxed setting reaches a place it was never meant for. There is
//! deliberately no `danger_accept_invalid_certs` anywhere in this module.

use std::time::Duration;

/// Whether a redirect may be followed.
///
/// `None` is the answer wherever the URL came from an operator or a third party: a 30x is the
/// classic way an allowed address becomes a request to a loopback or metadata address, and the
/// check at the API edge only ever saw the first hop (SSRF, `security.md`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Redirects {
    /// Never follow one.
    None,
    /// Follow them, as `reqwest` does by default. For core's own stores, whose address is static
    /// configuration.
    Follow,
}

impl Redirects {
    fn policy(self) -> reqwest::redirect::Policy {
        match self {
            Self::None => reqwest::redirect::Policy::none(),
            Self::Follow => reqwest::redirect::Policy::default(),
        }
    }
}

/// What every client built here calls itself.
const USER_AGENT: &str = "Yagra-core";

/// A builder with the three shared decisions made. For a caller that has more to add (a private
/// CA) or that reports a build failure to its own caller; everyone else wants [`client`].
pub fn builder(timeout: Duration, redirects: Redirects) -> reqwest::ClientBuilder {
    reqwest::Client::builder()
        .timeout(timeout)
        .redirect(redirects.policy())
        .user_agent(USER_AGENT)
}

/// A client, for a caller with nowhere to report a build failure to.
///
/// Building can only fail when the TLS backend cannot start, which is not something a retry or a
/// different option changes. The fallback is therefore a second attempt that **keeps the redirect
/// policy and drops everything else**: losing a timeout is survivable, following a redirect the
/// caller forbade is not. If that attempt fails too, this panics rather than hand back
/// `reqwest::Client::default()`, which follows redirects. This is the only place in the crate a
/// build failure falls back.
pub fn client(timeout: Duration, redirects: Redirects) -> reqwest::Client {
    builder(timeout, redirects).build().unwrap_or_else(|error| {
        tracing::error!(%error, "could not build an outbound HTTP client; retrying without a timeout");
        reqwest::Client::builder()
            .redirect(redirects.policy())
            .build()
            .expect("the TLS backend cannot start, so no outbound HTTP client can be built")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A stand-in that answers every path with a redirect to `/landed`, and `/landed` with 200.
    async fn redirecting_server() -> String {
        use axum::http::{header, StatusCode};
        let app = axum::Router::new()
            .route(
                "/landed",
                axum::routing::get(|headers: axum::http::HeaderMap| async move {
                    // Echo the User-Agent so a test can see what the client called itself.
                    headers
                        .get(header::USER_AGENT)
                        .and_then(|v| v.to_str().ok())
                        .unwrap_or_default()
                        .to_owned()
                }),
            )
            .fallback(|| async { (StatusCode::FOUND, [(header::LOCATION, "/landed")]) });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.ok();
        });
        format!("http://{addr}")
    }

    #[tokio::test]
    async fn a_client_told_not_to_follow_a_redirect_does_not() {
        let base = redirecting_server().await;
        let resp = client(Duration::from_secs(5), Redirects::None)
            .get(format!("{base}/start"))
            .send()
            .await
            .unwrap();
        assert_eq!(
            resp.status().as_u16(),
            302,
            "the redirect itself is the answer"
        );
    }

    #[tokio::test]
    async fn a_client_allowed_to_follow_a_redirect_does_and_names_itself() {
        let base = redirecting_server().await;
        let resp = client(Duration::from_secs(5), Redirects::Follow)
            .get(format!("{base}/start"))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status().as_u16(), 200);
        assert_eq!(resp.text().await.unwrap(), USER_AGENT);
    }

    #[tokio::test]
    async fn a_server_that_never_answers_is_given_up_on() {
        // Accepts the connection and then says nothing — the hung socket the VictoriaLogs client
        // used to wait on for ever.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let mut held = Vec::new();
            while let Ok((socket, _)) = listener.accept().await {
                held.push(socket);
            }
        });
        let started = std::time::Instant::now();
        let err = client(Duration::from_millis(300), Redirects::Follow)
            .get(format!("http://{addr}/"))
            .send()
            .await
            .expect_err("nothing was ever sent back");
        assert!(err.is_timeout(), "{err}");
        assert!(started.elapsed() < Duration::from_secs(5));
    }
}
