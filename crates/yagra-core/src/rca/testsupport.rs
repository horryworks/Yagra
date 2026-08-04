// SPDX-License-Identifier: AGPL-3.0-only
//! A minimal HTTP stand-in shared by the provider-adapter tests.
//!
//! The adapters pin their hosts to constants, so the only way to exercise request shaping end to
//! end is to point a test-only URL override at a local socket. This is deliberately the same shape
//! as the fake in `bigquery.rs` — enough to answer `reqwest`, not a general server.
//!
//! No `#![cfg(test)]` here: `rca/mod.rs` already declares this module under `#[cfg(test)]`, so the
//! inner attribute gated nothing and only duplicated the outer one. clippy 0.1.90 fails the build
//! on it (`clippy::duplicated_attributes`) while 0.1.95 does not — which is how it surfaced, but
//! the redundancy was real either way.

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use tokio::net::TcpListener;

/// One request as the fake saw it.
pub(crate) struct Seen {
    /// Request line + headers, verbatim, for asserting on auth headers.
    pub head: String,
    /// Request body.
    pub body: String,
}

/// Start a server that answers from a scripted queue of `(status, body)` and records what it was
/// asked. Returns its address and the shared log.
pub(crate) async fn serve(replies: Vec<(u16, String)>) -> (SocketAddr, Arc<Mutex<Vec<Seen>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let log = seen.clone();
    let queue = Arc::new(Mutex::new(replies));
    tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else {
                return;
            };
            let log = log.clone();
            let queue = queue.clone();
            tokio::spawn(async move {
                use tokio::io::{AsyncReadExt, AsyncWriteExt};
                let mut buf = Vec::new();
                let mut chunk = [0u8; 4096];
                // Read until headers + Content-Length bytes have arrived.
                loop {
                    let n = sock.read(&mut chunk).await.unwrap_or(0);
                    if n == 0 {
                        break;
                    }
                    buf.extend_from_slice(&chunk[..n]);
                    let text = String::from_utf8_lossy(&buf).to_string();
                    let Some(head_end) = text.find("\r\n\r\n") else {
                        continue;
                    };
                    let len: usize = text
                        .to_ascii_lowercase()
                        .split("content-length:")
                        .nth(1)
                        .and_then(|r| r.split("\r\n").next())
                        .and_then(|v| v.trim().parse().ok())
                        .unwrap_or(0);
                    if buf.len() >= head_end + 4 + len {
                        break;
                    }
                }
                let text = String::from_utf8_lossy(&buf).to_string();
                let mut parts = text.splitn(2, "\r\n\r\n");
                let head = parts.next().unwrap_or_default().to_owned();
                let body = parts.next().unwrap_or_default().to_owned();
                log.lock().unwrap().push(Seen { head, body });
                let (status, payload) = {
                    let mut q = queue.lock().unwrap();
                    if q.is_empty() {
                        (200, "{}".to_owned())
                    } else {
                        q.remove(0)
                    }
                };
                let res = format!(
                    "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{payload}",
                    payload.len()
                );
                let _ = sock.write_all(res.as_bytes()).await;
                let _ = sock.shutdown().await;
            });
        }
    });
    (addr, seen)
}
