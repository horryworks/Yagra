// SPDX-License-Identifier: AGPL-3.0-only
//! The config-backup relay, core's half (ADR-210 stage 5 decisions 43-49): the bundled oxidized
//! logs in to every device through the poller assigned to it.
//!
//! oxidized is handed `core` and a port instead of the device's address ([`super::source`]). This
//! module listens on those ports, and for each connection opens a session over the bus with the
//! poller that polls the node; the poller connects to the device and the bytes go both ways
//! (`yagra-poller/src/relay.rs`). That is what reaches a device behind a site's NAT, and by the
//! user's decision every device goes this way, so there is one path rather than two.
//!
//! - [`Relay`] is the shared part: which poller a node goes through ([`Routes`]), the sessions in
//!   flight, and which nodes' relays failed recently (decision 47).
//! - [`run_listeners`] keeps one pair of listeners per configured node, from
//!   `config_backup_relay_ports` (decision 44), and [`run_reply_loop`] routes the pollers' frames.
//!
//! What it does not do: speak SSH or telnet, look at the bytes, or keep them. ⚠️ A telnet session
//! carries the device password in the clear through this module; nothing here may print a frame.
//!
//! Leader-only: the coordinator that knows which poller owns a node runs on the leader, so a
//! standby would answer every connection with "no poller".

use std::collections::{HashMap, HashSet};
use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::time::{timeout, Instant};
use uuid::Uuid;
use yagra_base::config_backup::ConfigBackupRepo;
use yagra_bus::{
    encode_raw, RelayBus, RelayFrame, RelayTarget, CAP_CONFIG_RELAY, CAP_CONFIG_RELAY_REFUSED,
    RELAY_CHUNK_BYTES, RELAY_CONNECT_SECS, RELAY_IDLE_SECS, RELAY_MAX_BYTES, RELAY_MAX_SECS,
};
use yagra_common::NodeId;

use crate::fleet::coordinator::Coordinator;

/// This core's own address on the network it shares with oxidized: what oxidized is handed for
/// every node it may log in to (decision 43).
///
/// ⚠️ An address, never a name. oxidized 0.37 keeps `ip` only when it parses as an IP literal; a
/// name such as `core` is replaced by the node's own name (its uuid) when `resolve_dns` is off, so
/// the connection never leaves oxidized and the fetch fails as a refused login would (measured on
/// a lab deployment, 2026-10-23). Found by asking which local address the route to oxidized leaves
/// from: a UDP `connect` sends nothing, it only chooses the route.
pub(crate) async fn relay_address(oxidized_host: &str) -> Option<IpAddr> {
    let target = tokio::net::lookup_host((oxidized_host, 8888))
        .await
        .ok()?
        .next()?;
    let bind = match target {
        SocketAddr::V4(_) => SocketAddr::new(IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED), 0),
        SocketAddr::V6(_) => SocketAddr::new(IpAddr::V6(std::net::Ipv6Addr::UNSPECIFIED), 0),
    };
    let sock = tokio::net::UdpSocket::bind(bind).await.ok()?;
    sock.connect(target).await.ok()?;
    Some(sock.local_addr().ok()?.ip())
}

/// The device ports, in the order of a node's two relay ports: SSH on the even one, telnet on the
/// one after it.
const DEVICE_SSH: u16 = 22;
const DEVICE_TELNET: u16 = 23;

/// How long a relay failure turns the next failed fetch of that node into "unreachable"
/// (decision 47). Longer than one fetch takes, shorter than the gap to the next one.
const FAILURE_MEMORY: Duration = Duration::from_secs(5 * 60);

/// How often the listeners are matched against the configured nodes.
const RECONCILE: Duration = Duration::from_secs(30);

/// Frames from the poller queued for one session before it is ended as too slow.
const SESSION_QUEUE: usize = 256;

/// Which poller a node's relay goes through, or why there is none.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Route {
    Via(String),
    /// No live poller polls the node: its pool has none, or none has been handed it yet.
    NoPoller,
    /// The poller that polls it predates the relay (no `config-relay` in its heartbeat).
    PollerTooOld,
    /// The poller carries the relay and the bus refuses it the subscription (decision 52): the
    /// bus's allow-list is older than the poller.
    RelayRefused,
}

/// Where a node's relay goes. A trait so the relay can be driven without a coordinator.
pub(crate) trait Routes: Send + Sync {
    fn route(&self, node: Uuid) -> Route;
}

impl Routes for Coordinator {
    fn route(&self, node: Uuid) -> Route {
        let now = std::time::Instant::now();
        let Some(poller) = self.owner_of(NodeId::from(node), now) else {
            return Route::NoPoller;
        };
        let caps = self.caps_of(&poller, now).unwrap_or_default();
        if caps.iter().any(|c| c == CAP_CONFIG_RELAY) {
            Route::Via(poller)
        } else if caps.iter().any(|c| c == CAP_CONFIG_RELAY_REFUSED) {
            Route::RelayRefused
        } else {
            Route::PollerTooOld
        }
    }
}

/// The relay's shared state.
pub(crate) struct Relay {
    routes: Arc<dyn Routes>,
    /// Each session's channel, with the node it carries a login to.
    sessions: Mutex<HashMap<Uuid, (Uuid, mpsc::Sender<RelayFrame>)>>,
    failures: Mutex<HashMap<Uuid, Instant>>,
    /// Nodes whose fetch a person cancelled (decision 54): new sessions are refused and the fetch's
    /// late report is not recorded, for [`FAILURE_MEMORY`].
    cancelled: Mutex<HashMap<Uuid, Instant>>,
}

impl Relay {
    pub(crate) fn new(routes: Arc<dyn Routes>) -> Arc<Self> {
        Arc::new(Self {
            routes,
            sessions: Mutex::new(HashMap::new()),
            failures: Mutex::new(HashMap::new()),
            cancelled: Mutex::new(HashMap::new()),
        })
    }

    pub(crate) fn route(&self, node: Uuid) -> Route {
        self.routes.route(node)
    }

    pub(crate) fn record_failure(&self, node: Uuid) {
        if let Ok(mut f) = self.failures.lock() {
            f.insert(node, Instant::now());
        }
    }

    /// Whether `node`'s relay failed within [`FAILURE_MEMORY`], forgetting it as it answers: a
    /// failure explains one failed fetch, not every later one.
    pub(crate) fn take_recent_failure(&self, node: Uuid) -> bool {
        let Ok(mut f) = self.failures.lock() else {
            return false;
        };
        f.remove(&node)
            .is_some_and(|at| at.elapsed() < FAILURE_MEMORY)
    }

    /// Cut every session carrying a login to `node`, and refuse new ones for [`FAILURE_MEMORY`]
    /// (decision 54). A session's channel is dropped, so its loop ends and tells the poller to close.
    pub(crate) fn cancel(&self, node: Uuid) {
        if let Ok(mut c) = self.cancelled.lock() {
            c.insert(node, Instant::now());
        }
        if let Ok(mut s) = self.sessions.lock() {
            s.retain(|_, (n, _)| *n != node);
        }
    }

    fn is_cancelled(&self, node: Uuid) -> bool {
        self.cancelled
            .lock()
            .is_ok_and(|c| c.get(&node).is_some_and(|at| at.elapsed() < FAILURE_MEMORY))
    }

    /// Whether `node`'s fetch was cancelled within [`FAILURE_MEMORY`], forgetting it as it answers:
    /// the report of the fetch a person cancelled is not recorded, since the cancel already was.
    pub(crate) fn take_cancelled(&self, node: Uuid) -> bool {
        let Ok(mut c) = self.cancelled.lock() else {
            return false;
        };
        c.remove(&node)
            .is_some_and(|at| at.elapsed() < FAILURE_MEMORY)
    }

    fn open_session(&self, id: Uuid, node: Uuid) -> mpsc::Receiver<RelayFrame> {
        let (tx, rx) = mpsc::channel(SESSION_QUEUE);
        if let Ok(mut s) = self.sessions.lock() {
            s.insert(id, (node, tx));
        }
        rx
    }

    fn end_session(&self, id: Uuid) {
        if let Ok(mut s) = self.sessions.lock() {
            s.remove(&id);
        }
    }

    /// Hand a poller's frame to its session. A frame for a session that has ended is dropped, and a
    /// session that cannot keep up is ended (its receiver then sees the channel close).
    fn deliver(&self, frame: RelayFrame) {
        let Ok(mut s) = self.sessions.lock() else {
            return;
        };
        let id = frame.session;
        let Some((_, tx)) = s.get(&id) else {
            return;
        };
        if tx.try_send(frame).is_err() {
            s.remove(&id);
        }
    }
}

/// Route the pollers' frames to their sessions, for the lifetime of the process.
pub(crate) async fn run_reply_loop<S>(relay: Arc<Relay>, mut stream: S)
where
    S: futures::Stream<Item = RelayFrame> + Unpin,
{
    use futures::StreamExt;
    while let Some(frame) = stream.next().await {
        relay.deliver(frame);
    }
}

/// Why a connection ended, as the label on `yagra_config_relay_sessions_total`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Outcome {
    Closed,
    NoPoller,
    PollerTooOld,
    RelayRefused,
    /// A person cancelled the node's fetch (decision 54).
    Cancelled,
    /// The poller refused, did not answer, or could not reach the device.
    NotOpened,
    /// A limit, a lost frame, or the bus.
    Cut,
    /// Not from oxidized.
    Stranger,
}

impl Outcome {
    fn as_str(self) -> &'static str {
        match self {
            Self::Closed => "closed",
            Self::NoPoller => "no_poller",
            Self::PollerTooOld => "poller_too_old",
            Self::RelayRefused => "relay_refused",
            Self::Cancelled => "cancelled",
            Self::NotOpened => "not_opened",
            Self::Cut => "cut",
            Self::Stranger => "stranger",
        }
    }

    /// Whether the fetch this connection carried failed because of the relay rather than the
    /// device's answer.
    fn is_relay_failure(self) -> bool {
        match self {
            Self::Closed | Self::Stranger => false,
            Self::NoPoller
            | Self::PollerTooOld
            | Self::RelayRefused
            | Self::Cancelled
            | Self::NotOpened
            | Self::Cut => true,
        }
    }
}

/// One node's relay, as a pair of listeners serves it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Endpoint {
    node: Uuid,
    address: IpAddr,
}

/// The SSH relay port of each node that may be fetched, keyed by port. A node whose address
/// changed since it was saved has no endpoint: its login is withheld, and so is its relay.
async fn desired(repo: &ConfigBackupRepo) -> anyhow::Result<HashMap<u16, Endpoint>> {
    let targets: Vec<_> = repo
        .targets()
        .await?
        .into_iter()
        .filter(|t| !t.address_changed)
        .collect();
    let ids: Vec<Uuid> = targets.iter().map(|t| t.node_id).collect();
    let ports = repo.relay_ports(&ids).await?;
    Ok(targets
        .into_iter()
        .filter_map(|t| {
            ports.get(&t.node_id).map(|p| {
                (
                    *p,
                    Endpoint {
                        node: t.node_id,
                        address: t.address,
                    },
                )
            })
        })
        .collect())
}

/// The addresses a connection may come from: the bundled oxidized's own, resolved from the host
/// core reaches its REST API on. Anything else on the network is refused before a session opens.
async fn oxidized_addresses(oxidized_host: &str) -> HashSet<IpAddr> {
    tokio::net::lookup_host((oxidized_host, 0))
        .await
        .map(|it| it.map(|a| a.ip()).collect())
        .unwrap_or_default()
}

/// The host part of oxidized's REST URL (`http://oxidized:8888` → `oxidized`).
pub(crate) fn host_of(url: &str) -> Option<String> {
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    let authority = rest.split('/').next()?;
    let host = if let Some(v6) = authority.strip_prefix('[') {
        v6.split(']').next()?
    } else {
        authority.split(':').next()?
    };
    (!host.is_empty()).then(|| host.to_owned())
}

/// Keep one pair of listeners per node that may be fetched, until cancelled.
pub(crate) async fn run_listeners(
    relay: Arc<Relay>,
    repo: Arc<ConfigBackupRepo>,
    bus: Arc<dyn RelayBus>,
    oxidized_host: String,
) {
    let mut open: HashMap<u16, (Endpoint, [JoinHandle<()>; 2])> = HashMap::new();
    let mut every = tokio::time::interval(RECONCILE);
    every.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        every.tick().await;
        let want = match desired(&repo).await {
            Ok(w) => w,
            Err(e) => {
                // A failed read changes nothing: closing every relay on a database hiccup would
                // turn one slow query into a round of failed fetches.
                tracing::warn!(error = %e, "config relay: the configured nodes could not be read");
                continue;
            }
        };
        open.retain(|port, (ep, tasks)| {
            let keep = want.get(port) == Some(ep);
            if !keep {
                for t in tasks.iter() {
                    t.abort();
                }
            }
            keep
        });
        for (port, ep) in want {
            if open.contains_key(&port) {
                continue;
            }
            let pair = [(port, DEVICE_SSH), (port + 1, DEVICE_TELNET)].map(|(local, device)| {
                tokio::spawn(listen(
                    relay.clone(),
                    bus.clone(),
                    oxidized_host.clone(),
                    local,
                    ep.clone(),
                    device,
                ))
            });
            open.insert(port, (ep, pair));
        }
        metrics::gauge!("yagra_config_relay_listeners").set(open.len() as f64 * 2.0);
    }
}

async fn listen(
    relay: Arc<Relay>,
    bus: Arc<dyn RelayBus>,
    oxidized_host: String,
    local: u16,
    ep: Endpoint,
    device: u16,
) {
    let listener = match TcpListener::bind(SocketAddr::new(
        IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED),
        local,
    ))
    .await
    {
        Ok(l) => l,
        Err(e) => {
            tracing::warn!(port = local, error = %e, "config relay: a port could not be opened");
            return;
        }
    };
    loop {
        let Ok((sock, peer)) = listener.accept().await else {
            // Out of file descriptors, most likely: wait rather than spin.
            tokio::time::sleep(Duration::from_secs(1)).await;
            continue;
        };
        let relay = relay.clone();
        let bus = bus.clone();
        let host = oxidized_host.clone();
        let ep = ep.clone();
        tokio::spawn(async move {
            let outcome = if oxidized_addresses(&host).await.contains(&peer.ip()) {
                serve(&relay, &bus, sock, &ep, device).await
            } else {
                drop(sock);
                Outcome::Stranger
            };
            if outcome.is_relay_failure() {
                relay.record_failure(ep.node);
            }
            if outcome == Outcome::Stranger {
                tracing::warn!(%peer, "config relay: a connection not from oxidized was refused");
            }
            metrics::counter!("yagra_config_relay_sessions_total", "outcome" => outcome.as_str())
                .increment(1);
        });
    }
}

fn frame(poller: &str, session: Uuid, seq: u64) -> RelayFrame {
    RelayFrame {
        poller_id: poller.to_owned(),
        session,
        seq,
        open: None,
        bytes: String::new(),
        close: false,
        refused: None,
    }
}

/// One connection from oxidized: open a session with the node's poller and carry the bytes.
async fn serve(
    relay: &Arc<Relay>,
    bus: &Arc<dyn RelayBus>,
    sock: TcpStream,
    ep: &Endpoint,
    device: u16,
) -> Outcome {
    if relay.is_cancelled(ep.node) {
        return Outcome::Cancelled;
    }
    let poller = match relay.route(ep.node) {
        Route::Via(p) => p,
        Route::NoPoller => return Outcome::NoPoller,
        Route::PollerTooOld => return Outcome::PollerTooOld,
        Route::RelayRefused => return Outcome::RelayRefused,
    };
    let session = Uuid::new_v4();
    let mut rx = relay.open_session(session, ep.node);
    let outcome = run_session(&poller, session, sock, ep, device, &mut rx, bus).await;
    relay.end_session(session);
    outcome
}

async fn run_session(
    poller: &str,
    session: Uuid,
    sock: TcpStream,
    ep: &Endpoint,
    device: u16,
    rx: &mut mpsc::Receiver<RelayFrame>,
    bus: &Arc<dyn RelayBus>,
) -> Outcome {
    let open = RelayFrame {
        open: Some(RelayTarget {
            node_id: NodeId::from(ep.node),
            target: ep.address,
            port: device,
        }),
        ..frame(poller, session, 0)
    };
    if bus.publish_relay_to_poller(open).await.is_err() {
        return Outcome::NotOpened;
    }
    // The poller waits RELAY_CONNECT_SECS for the device; a little more here, so its refusal
    // arrives before this side gives up.
    match timeout(Duration::from_secs(RELAY_CONNECT_SECS + 5), rx.recv()).await {
        Ok(Some(f)) if f.seq == 0 && !f.close => {}
        _ => return Outcome::NotOpened,
    }
    carry(poller, session, sock, rx, bus).await
}

async fn carry(
    poller: &str,
    session: Uuid,
    sock: TcpStream,
    rx: &mut mpsc::Receiver<RelayFrame>,
    bus: &Arc<dyn RelayBus>,
) -> Outcome {
    let (mut read, mut write) = sock.into_split();
    let mut buf = vec![0u8; RELAY_CHUNK_BYTES];
    // Frame 0 was the open.
    let mut sent: u64 = 1;
    // The poller's frame 0 was its answer to the open.
    let mut expect: u64 = 1;
    let mut moved: u64 = 0;
    let deadline = Instant::now() + Duration::from_secs(RELAY_MAX_SECS);
    let idle = Duration::from_secs(RELAY_IDLE_SECS);
    let mut last = Instant::now();
    let close = |seq| RelayFrame {
        close: true,
        ..frame(poller, session, seq)
    };
    loop {
        let wake = (last + idle).min(deadline);
        tokio::select! {
            got = rx.recv() => {
                let Some(f) = got else {
                    let _ = bus.publish_relay_to_poller(close(sent)).await;
                    return Outcome::Cut;
                };
                if f.seq != expect {
                    let _ = bus.publish_relay_to_poller(close(sent)).await;
                    return Outcome::Cut;
                }
                expect += 1;
                let Some(bytes) = f.slice() else {
                    let _ = bus.publish_relay_to_poller(close(sent)).await;
                    return Outcome::Cut;
                };
                moved += bytes.len() as u64;
                if moved > RELAY_MAX_BYTES {
                    let _ = bus.publish_relay_to_poller(close(sent)).await;
                    return Outcome::Cut;
                }
                if !bytes.is_empty() && write.write_all(&bytes).await.is_err() {
                    let _ = bus.publish_relay_to_poller(close(sent)).await;
                    return Outcome::Closed;
                }
                if f.close {
                    let _ = write.shutdown().await;
                    // A poller that ended the session early says why; the fetch it carried failed
                    // because of the relay, not the device.
                    return if f.refused.is_some() { Outcome::Cut } else { Outcome::Closed };
                }
                last = Instant::now();
            }
            n = read.read(&mut buf) => {
                let n = match n {
                    Ok(0) | Err(_) => {
                        let _ = bus.publish_relay_to_poller(close(sent)).await;
                        return Outcome::Closed;
                    }
                    Ok(n) => n,
                };
                moved += n as u64;
                if moved > RELAY_MAX_BYTES {
                    let _ = bus.publish_relay_to_poller(close(sent)).await;
                    return Outcome::Cut;
                }
                let out = RelayFrame {
                    bytes: encode_raw(&buf[..n]),
                    ..frame(poller, session, sent)
                };
                sent += 1;
                if bus.publish_relay_to_poller(out).await.is_err() {
                    return Outcome::Cut;
                }
                last = Instant::now();
            }
            () = tokio::time::sleep_until(wake) => {
                let _ = bus.publish_relay_to_poller(close(sent)).await;
                return Outcome::Cut;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::StreamExt;
    use std::net::Ipv4Addr;
    use tokio_stream::wrappers::BroadcastStream;
    use yagra_bus::InMemoryBus;

    struct Fixed(Route);
    impl Routes for Fixed {
        fn route(&self, _node: Uuid) -> Route {
            self.0.clone()
        }
    }

    #[tokio::test]
    async fn the_relay_address_is_the_one_the_route_to_oxidized_leaves_from() {
        assert_eq!(
            relay_address("127.0.0.1").await,
            Some(IpAddr::V4(Ipv4Addr::LOCALHOST))
        );
        assert_eq!(relay_address("name.invalid").await, None);
    }

    #[test]
    fn the_oxidized_host_is_read_from_its_url() {
        assert_eq!(host_of("http://oxidized:8888").as_deref(), Some("oxidized"));
        assert_eq!(host_of("http://oxidized").as_deref(), Some("oxidized"));
        assert_eq!(host_of("http://[::1]:8888/x").as_deref(), Some("::1"));
        assert_eq!(host_of("http://:8888"), None);
    }

    #[test]
    fn only_the_relay_ending_a_fetch_counts_as_its_failure() {
        for o in [
            Outcome::Closed,
            Outcome::NoPoller,
            Outcome::PollerTooOld,
            Outcome::RelayRefused,
            Outcome::Cancelled,
            Outcome::NotOpened,
            Outcome::Cut,
            Outcome::Stranger,
        ] {
            assert_eq!(
                o.is_relay_failure(),
                !matches!(o, Outcome::Closed | Outcome::Stranger),
                "{o:?}"
            );
        }
    }

    /// A cancel cuts the node's sessions, refuses new ones, and swallows the one late report.
    #[test]
    fn a_cancelled_fetch_is_cut_refused_and_reported_once() {
        let relay = Relay::new(Arc::new(Fixed(Route::Via("p1".to_owned()))));
        let (a, b) = (Uuid::from_u128(1), Uuid::from_u128(2));
        let mut ra = relay.open_session(Uuid::from_u128(10), a);
        let _rb = relay.open_session(Uuid::from_u128(11), b);
        relay.cancel(a);
        assert!(ra.try_recv().is_err());
        assert!(matches!(
            ra.try_recv(),
            Err(tokio::sync::mpsc::error::TryRecvError::Disconnected)
        ));
        assert_eq!(
            relay.sessions.lock().expect("lock").len(),
            1,
            "b keeps its session"
        );
        assert!(relay.is_cancelled(a) && !relay.is_cancelled(b));
        assert!(relay.take_cancelled(a));
        assert!(!relay.take_cancelled(a), "one report only");
    }

    #[test]
    fn a_relay_failure_explains_one_failed_fetch() {
        let relay = Relay::new(Arc::new(Fixed(Route::NoPoller)));
        let node = Uuid::from_u128(1);
        assert!(!relay.take_recent_failure(node));
        relay.record_failure(node);
        assert!(relay.take_recent_failure(node));
        assert!(
            !relay.take_recent_failure(node),
            "forgotten once it answered"
        );
    }

    /// Stand in for the poller: answer the open, echo one slice back upper-cased, and close.
    async fn fake_poller(bus: Arc<InMemoryBus>) {
        let mut inbox = bus.subscribe_relay_to_poller();
        let mut seq = 0u64;
        while let Ok((_, f)) = inbox.recv().await {
            if f.open.is_some() {
                RelayBus::publish_relay_to_core(&*bus, frame("p1", f.session, 0))
                    .await
                    .expect("publish");
                seq = 1;
                continue;
            }
            let bytes = f.slice().expect("bytes");
            if bytes.is_empty() {
                continue;
            }
            let reply = RelayFrame {
                bytes: encode_raw(&bytes.to_ascii_uppercase()),
                ..frame("p1", f.session, seq)
            };
            RelayBus::publish_relay_to_core(&*bus, reply)
                .await
                .expect("publish");
            let done = RelayFrame {
                close: true,
                ..frame("p1", f.session, seq + 1)
            };
            RelayBus::publish_relay_to_core(&*bus, done)
                .await
                .expect("publish");
            return;
        }
    }

    /// What oxidized writes into core's port reaches the poller, and the poller's bytes come back.
    #[tokio::test]
    async fn a_connection_is_carried_to_the_nodes_poller_and_back() {
        let bus = Arc::new(InMemoryBus::new(64));
        let relay = Relay::new(Arc::new(Fixed(Route::Via("p1".to_owned()))));
        let replies = BroadcastStream::new(bus.subscribe_relay_to_core())
            .filter_map(|r| async move { r.ok() });
        tokio::spawn(run_reply_loop(relay.clone(), Box::pin(replies)));
        tokio::spawn(fake_poller(bus.clone()));

        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("addr");
        let client = tokio::spawn(async move {
            let mut c = TcpStream::connect(addr).await.expect("connect");
            c.write_all(b"show run").await.expect("write");
            let mut got = Vec::new();
            c.read_to_end(&mut got).await.expect("read");
            got
        });
        let (sock, _) = listener.accept().await.expect("accept");
        let ep = Endpoint {
            node: Uuid::from_u128(1),
            address: IpAddr::V4(Ipv4Addr::new(192, 0, 2, 10)),
        };
        let relay_bus: Arc<dyn RelayBus> = bus.clone();
        let outcome = serve(&relay, &relay_bus, sock, &ep, DEVICE_SSH).await;
        assert_eq!(outcome, Outcome::Closed);
        assert_eq!(client.await.expect("client"), b"SHOW RUN");
    }

    /// With no poller to go through, nothing is published and the fetch is the relay's failure.
    #[tokio::test]
    async fn a_node_with_no_poller_is_not_opened() {
        let bus = Arc::new(InMemoryBus::new(64));
        let mut inbox = bus.subscribe_relay_to_poller();
        let relay = Relay::new(Arc::new(Fixed(Route::NoPoller)));
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("addr");
        let _client = tokio::spawn(async move { TcpStream::connect(addr).await });
        let (sock, _) = listener.accept().await.expect("accept");
        let ep = Endpoint {
            node: Uuid::from_u128(1),
            address: IpAddr::V4(Ipv4Addr::new(192, 0, 2, 10)),
        };
        let relay_bus: Arc<dyn RelayBus> = bus.clone();
        let outcome = serve(&relay, &relay_bus, sock, &ep, DEVICE_SSH).await;
        assert_eq!(outcome, Outcome::NoPoller);
        assert!(outcome.is_relay_failure());
        assert!(inbox.try_recv().is_err(), "nothing was sent to any poller");
    }
}
