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
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;
use tokio::time::{timeout, Instant};
use uuid::Uuid;
use yagra_base::config_backup::ConfigBackupRepo;
use yagra_bus::{
    encode_raw, RelayBus, RelayFrame, RelayTarget, CAP_CONFIG_RELAY, CAP_CONFIG_RELAY_REFUSED,
    RELAY_CHUNK_BYTES, RELAY_CONNECT_SECS, RELAY_IDLE_SECS, RELAY_MAX_BYTES, RELAY_MAX_SECS,
    RELAY_PORTS,
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
/// one after it. Taken from the list the poller checks an open against, so the two cannot drift.
const DEVICE_SSH: u16 = RELAY_PORTS[0];
const DEVICE_TELNET: u16 = RELAY_PORTS[1];

/// How long a relay failure turns the next failed fetch of that node into "unreachable"
/// (decision 47). Longer than one fetch takes, shorter than the gap to the next one.
const FAILURE_MEMORY: Duration = Duration::from_secs(5 * 60);

/// How often the listeners are matched against the configured nodes when nothing asks sooner.
const RECONCILE: Duration = Duration::from_secs(30);

/// How long the node list waits for the listeners to be matched before it answers anyway.
const LISTENING_WAIT: Duration = Duration::from_secs(5);

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

/// One session in flight.
struct Session {
    node: Uuid,
    poller: String,
    tx: mpsc::Sender<RelayFrame>,
}

/// The relay's shared state.
pub(crate) struct Relay {
    routes: Arc<dyn Routes>,
    /// Each session's channel, with the node it carries a login to and the poller it was opened
    /// with — every poller may publish on the reply subject, so a frame is taken only from that one.
    sessions: Mutex<HashMap<Uuid, Session>>,
    failures: Mutex<HashMap<Uuid, Instant>>,
    /// Nodes whose fetch a person cancelled (decision 54): new sessions are refused and the fetch's
    /// late report is not recorded, for [`FAILURE_MEMORY`].
    cancelled: Mutex<HashMap<Uuid, Instant>>,
    /// Requests to match the listeners now, each answered once a pass that began after it is done
    /// ([`Relay::listening_now`]). The receiver is taken by [`run_listeners`].
    asks: mpsc::UnboundedSender<oneshot::Sender<()>>,
    asked: Mutex<Option<mpsc::UnboundedReceiver<oneshot::Sender<()>>>>,
    /// Whether [`run_listeners`] is running here, so a core without it does not wait for nothing.
    listening: AtomicBool,
}

impl Relay {
    pub(crate) fn new(routes: Arc<dyn Routes>) -> Arc<Self> {
        let (asks, asked) = mpsc::unbounded_channel();
        Arc::new(Self {
            routes,
            sessions: Mutex::new(HashMap::new()),
            failures: Mutex::new(HashMap::new()),
            cancelled: Mutex::new(HashMap::new()),
            asks,
            asked: Mutex::new(Some(asked)),
            listening: AtomicBool::new(false),
        })
    }

    /// Wait until the listeners match the configured nodes as they are now, for at most
    /// [`LISTENING_WAIT`]. Called before oxidized is handed its node list: oxidized fetches a node
    /// it has just been handed at once, and a port still closed fails that fetch as "did not
    /// answer". Every node switched on failed its first fetch that way until the next 30-second
    /// pass (measured on a PoC deployment, 2026-10-24).
    pub(crate) async fn listening_now(&self) {
        if !self.listening.load(Ordering::Acquire) {
            return;
        }
        let (done, wait) = oneshot::channel();
        if self.asks.send(done).is_err() {
            return;
        }
        if timeout(LISTENING_WAIT, wait).await.is_err() {
            tracing::warn!(
                "config relay: the listeners were not matched in time; answering anyway"
            );
        }
    }

    pub(crate) fn route(&self, node: Uuid) -> Route {
        self.routes.route(node)
    }

    pub(crate) fn record_failure(&self, node: Uuid) {
        if let Ok(mut f) = self.failures.lock() {
            f.insert(node, Instant::now());
        }
    }

    /// A session reached `node`'s device, so whatever this fetch reports next is the device's
    /// answer. An earlier failed connection of the same fetch — the SSH relay of an `ssh, telnet`
    /// node with no SSH — must not turn a refused telnet login into "did not answer", which the
    /// lockout limits do not count.
    fn clear_failure(&self, node: Uuid) {
        if let Ok(mut f) = self.failures.lock() {
            f.remove(&node);
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
            s.retain(|_, session| session.node != node);
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

    fn open_session(&self, id: Uuid, node: Uuid, poller: &str) -> mpsc::Receiver<RelayFrame> {
        let (tx, rx) = mpsc::channel(SESSION_QUEUE);
        if let Ok(mut s) = self.sessions.lock() {
            s.insert(
                id,
                Session {
                    node,
                    poller: poller.to_owned(),
                    tx,
                },
            );
        }
        rx
    }

    fn end_session(&self, id: Uuid) {
        if let Ok(mut s) = self.sessions.lock() {
            s.remove(&id);
        }
    }

    /// Hand a poller's frame to its session. A frame for a session that has ended, or from a poller
    /// the session was not opened with, is dropped, and a session that cannot keep up is ended (its
    /// receiver then sees the channel close).
    fn deliver(&self, frame: RelayFrame) {
        let Ok(mut s) = self.sessions.lock() else {
            return;
        };
        let id = frame.session;
        let Some(session) = s.get(&id).filter(|s| s.poller == frame.poller_id) else {
            return;
        };
        if session.tx.try_send(frame).is_err() {
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

/// Whether a pair of listeners is kept as it is this tick: it still serves the endpoint wanted on
/// its port, and neither of them has stopped. A listener's task ends only if it panicked; the pair
/// is then opened again on the next pass, rather than leaving the node with no relay until core
/// restarts. A port that could not be bound is never entered, so it too is tried again.
fn still_listening(wanted: Option<&Endpoint>, serving: &Endpoint, stopped: bool) -> bool {
    wanted == Some(serving) && !stopped
}

/// Keep one pair of listeners per node that may be fetched, until cancelled: every
/// [`RECONCILE`], and at once whenever [`Relay::listening_now`] asks. The ports are opened on
/// `listen_on`: every address in production, since oxidized reaches core on its own network.
pub(crate) async fn run_listeners(
    relay: Arc<Relay>,
    repo: Arc<ConfigBackupRepo>,
    bus: Arc<dyn RelayBus>,
    oxidized_host: String,
    listen_on: IpAddr,
) {
    let Some(mut asked) = relay.asked.lock().ok().and_then(|mut a| a.take()) else {
        tracing::warn!("config relay: the listeners are already running");
        return;
    };
    relay.listening.store(true, Ordering::Release);
    let mut open: HashMap<u16, (Endpoint, [JoinHandle<()>; 2])> = HashMap::new();
    let mut every = tokio::time::interval(RECONCILE);
    every.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        let mut waiting = Vec::new();
        tokio::select! {
            _ = every.tick() => {}
            ask = asked.recv() => match ask {
                Some(done) => waiting.push(done),
                None => return,
            },
        }
        // Every request queued before this pass begins is answered by it: the pass reads the
        // configured nodes after the request was made.
        while let Ok(done) = asked.try_recv() {
            waiting.push(done);
        }
        reconcile(&relay, &repo, &bus, &oxidized_host, listen_on, &mut open).await;
        for done in waiting {
            let _ = done.send(());
        }
    }
}

/// Match the open listeners against the configured nodes once. A port is bound before this
/// returns, so a node list answered after it names only ports that accept.
async fn reconcile(
    relay: &Arc<Relay>,
    repo: &ConfigBackupRepo,
    bus: &Arc<dyn RelayBus>,
    oxidized_host: &str,
    listen_on: IpAddr,
    open: &mut HashMap<u16, (Endpoint, [JoinHandle<()>; 2])>,
) {
    let want = match desired(repo).await {
        Ok(w) => w,
        Err(e) => {
            // A failed read changes nothing: closing every relay on a database hiccup would
            // turn one slow query into a round of failed fetches.
            tracing::warn!(error = %e, "config relay: the configured nodes could not be read");
            return;
        }
    };
    open.retain(|port, (ep, tasks)| {
        let keep = still_listening(
            want.get(port),
            ep,
            tasks.iter().any(JoinHandle::is_finished),
        );
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
        // Both ports or neither: a pair left half open is tried again on the next pass.
        let (Some(ssh), Some(telnet)) =
            (bind(listen_on, port).await, bind(listen_on, port + 1).await)
        else {
            continue;
        };
        let pair = [(ssh, DEVICE_SSH), (telnet, DEVICE_TELNET)].map(|(listener, device)| {
            tokio::spawn(listen(
                relay.clone(),
                bus.clone(),
                oxidized_host.to_owned(),
                listener,
                ep.clone(),
                device,
            ))
        });
        open.insert(port, (ep, pair));
    }
    metrics::gauge!("yagra_config_relay_listeners").set(open.len() as f64 * 2.0);
}

/// Open one relay port. It may fail while the listener it replaces has not let go of it yet.
async fn bind(listen_on: IpAddr, local: u16) -> Option<TcpListener> {
    match TcpListener::bind(SocketAddr::new(listen_on, local)).await {
        Ok(l) => Some(l),
        Err(e) => {
            tracing::warn!(port = local, error = %e, "config relay: a port could not be opened");
            None
        }
    }
}

/// Carry each of oxidized's connections on one bound relay port, until aborted.
async fn listen(
    relay: Arc<Relay>,
    bus: Arc<dyn RelayBus>,
    oxidized_host: String,
    listener: TcpListener,
    ep: Endpoint,
    device: u16,
) {
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
    let mut rx = relay.open_session(session, ep.node, &poller);
    let outcome = if open_with_poller(&poller, session, ep, device, &mut rx, bus).await {
        // The device took the connection: what this fetch reports next is its answer.
        relay.clear_failure(ep.node);
        carry(&poller, session, sock, &mut rx, bus).await
    } else {
        Outcome::NotOpened
    };
    relay.end_session(session);
    outcome
}

/// Ask the poller to connect to the device, and wait for its answer. `true` when it connected.
async fn open_with_poller(
    poller: &str,
    session: Uuid,
    ep: &Endpoint,
    device: u16,
    rx: &mut mpsc::Receiver<RelayFrame>,
    bus: &Arc<dyn RelayBus>,
) -> bool {
    let open = RelayFrame {
        open: Some(RelayTarget {
            node_id: NodeId::from(ep.node),
            target: ep.address,
            port: device,
        }),
        ..frame(poller, session, 0)
    };
    if bus.publish_relay_to_poller(open).await.is_err() {
        return false;
    }
    // The poller waits RELAY_CONNECT_SECS for the device; a little more here, so its refusal
    // arrives before this side gives up.
    matches!(
        timeout(Duration::from_secs(RELAY_CONNECT_SECS + 5), rx.recv()).await,
        Ok(Some(f)) if f.seq == 0 && !f.close
    )
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
                if !bytes.is_empty() {
                    // Bounded by the same limits as the wait: a peer that stops reading would
                    // otherwise hold this arm, and the idle and length limits with it, for ever.
                    match tokio::time::timeout_at(wake, write.write_all(&bytes)).await {
                        Ok(Ok(())) => {}
                        Ok(Err(_)) => {
                            let _ = bus.publish_relay_to_poller(close(sent)).await;
                            return Outcome::Closed;
                        }
                        Err(_) => {
                            let _ = bus.publish_relay_to_poller(close(sent)).await;
                            return Outcome::Cut;
                        }
                    }
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
        let mut ra = relay.open_session(Uuid::from_u128(10), a, "p1");
        let _rb = relay.open_session(Uuid::from_u128(11), b, "p1");
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

    /// A listener that could not open its port is opened again on the next tick.
    #[test]
    fn a_stopped_listener_is_replaced() {
        let ep = Endpoint {
            node: Uuid::from_u128(1),
            address: IpAddr::V4(Ipv4Addr::new(192, 0, 2, 10)),
        };
        let moved = Endpoint {
            address: IpAddr::V4(Ipv4Addr::new(192, 0, 2, 11)),
            ..ep.clone()
        };
        assert!(still_listening(Some(&ep), &ep, false));
        assert!(!still_listening(Some(&ep), &ep, true), "its bind failed");
        assert!(!still_listening(Some(&moved), &ep, false));
        assert!(!still_listening(None, &ep, false));
    }

    /// A frame is taken only from the poller its session was opened with: every poller may publish
    /// on the reply subject.
    #[test]
    fn a_frame_from_another_poller_is_dropped() {
        let relay = Relay::new(Arc::new(Fixed(Route::Via("p1".to_owned()))));
        let id = Uuid::from_u128(10);
        let mut rx = relay.open_session(id, Uuid::from_u128(1), "p1");
        relay.deliver(frame("p2", id, 0));
        assert!(rx.try_recv().is_err(), "another site's frame");
        relay.deliver(frame("p1", id, 0));
        assert_eq!(
            rx.try_recv().expect("its own poller's frame").poller_id,
            "p1"
        );
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
        // The SSH relay of this fetch failed a moment ago; this connection reaches the device.
        relay.record_failure(ep.node);
        let outcome = serve(&relay, &relay_bus, sock, &ep, DEVICE_SSH).await;
        assert_eq!(outcome, Outcome::Closed);
        assert_eq!(client.await.expect("client"), b"SHOW RUN");
        assert!(
            !relay.take_recent_failure(ep.node),
            "a session that reached the device: the fetch's answer is the device's"
        );
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

    /// With no listener loop on this core there is nothing to wait for, and nothing is waited for.
    #[tokio::test]
    async fn the_node_list_does_not_wait_for_listeners_that_are_not_running() {
        let relay = Relay::new(Arc::new(Fixed(Route::NoPoller)));
        timeout(Duration::from_millis(200), relay.listening_now())
            .await
            .expect("answered at once");
    }

    /// A node switched on between two passes has its ports open once `listening_now` returns,
    /// which is what lets oxidized's first fetch of it reach the relay rather than a closed port.
    #[sqlx::test(migrator = "yagra_base::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn a_node_switched_on_is_listened_for_before_the_list_is_answered(pool: sqlx::PgPool) {
        use yagra_base::config_backup::{BackupInput, NodeConfigBackup, ScanMode};
        let repo = Arc::new(ConfigBackupRepo::new(pool.clone()));
        let relay = Relay::new(Arc::new(Fixed(Route::NoPoller)));
        let bus: Arc<dyn RelayBus> = Arc::new(InMemoryBus::new(8));
        let loop_task = tokio::spawn(run_listeners(
            relay.clone(),
            repo.clone(),
            bus,
            "localhost".to_owned(),
            // Loopback, so the test opens no port to the network (and asks no firewall).
            IpAddr::V4(Ipv4Addr::LOCALHOST),
        ));
        // The loop's first pass, with nothing configured yet.
        relay.listening_now().await;

        let node = yagra_base::pgtest::node(&pool, "sw-21", 21, None).await;
        repo.put(
            node,
            &NodeConfigBackup {
                enabled: true,
                login_order: None,
                input: BackupInput::Ssh,
                model_override: None,
                scan_mode: ScanMode::Normal,
            },
            true,
        )
        .await
        .expect("put");
        let port = repo.relay_ports(&[node]).await.expect("ports")[&node];
        relay.listening_now().await;
        for p in [port, port + 1] {
            timeout(
                Duration::from_secs(2),
                TcpStream::connect((Ipv4Addr::LOCALHOST, p)),
            )
            .await
            .expect("in time")
            .expect("the relay port accepts");
        }
        loop_task.abort();
    }
}
