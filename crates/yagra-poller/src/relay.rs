// SPDX-License-Identifier: AGPL-3.0-only
//! The config-backup relay (ADR-210 stage 5): core's bundled oxidized logs in to a device through
//! this poller.
//!
//! oxidized connects to a port on core; core carries the byte stream over the bus to the poller
//! assigned to the device, and this module connects to the device and passes the bytes both ways.
//! That is what lets one oxidized reach the devices behind every site's NAT — and every device, by
//! the user's decision, goes this way, so there is one path to keep working.
//!
//! **It carries bytes and speaks no protocol.** oxidized speaks SSH or telnet end to end with the
//! device; this module never parses, logs or keeps what it carries. That is why it opens a plain
//! TCP connection rather than going through `yagra-transport`, whose job is to speak protocols.
//!
//! What it does not take on trust (decision 46): it connects only when the node is in this
//! poller's working set **at the address named**, and only to the SSH or telnet port. Core already
//! decides who may be fetched; this is the second lock, so a frame that core did not mean — or
//! that another site published — cannot point this poller at anything else on its network.
//!
//! ⚠️ A telnet session carries the device password in the clear, across the bus (TLS on a remote
//! site) and through this process. Nothing here prints a frame, and neither may anything added.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tokio::time::{timeout, Instant};
use uuid::Uuid;
use yagra_bus::nats::NatsBus;
use yagra_bus::{
    encode_raw, RelayBus, RelayFrame, RelayTarget, RELAY_CHUNK_BYTES, RELAY_CONNECT_SECS,
    RELAY_IDLE_SECS, RELAY_MAX_BYTES, RELAY_MAX_SECS, RELAY_PORTS,
};
use yagra_telemetry::{spawn_cancellable, CancellationToken};

use crate::working_set::WorkingSet;

/// How many sessions this poller carries at once. oxidized runs a few fetches at a time, so this
/// is far above what it asks for, and bounds what a flood of opens could cost a small site.
const MAX_SESSIONS: usize = 64;

/// Frames from core queued for one session before the session is ended as too slow.
const SESSION_QUEUE: usize = 256;

/// Why a session ended, as the label on `yagra_config_relay_sessions_total`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Outcome {
    /// Either side finished the conversation.
    Closed,
    /// Not opened: the target is not this poller's to reach.
    Refused,
    /// Not opened: the device did not accept the connection in time.
    Unreachable,
    /// Ended by a limit (idle, length, size, a slow queue) or a frame out of order.
    Cut,
}

impl Outcome {
    fn as_str(self) -> &'static str {
        match self {
            Self::Closed => "closed",
            Self::Refused => "refused",
            Self::Unreachable => "unreachable",
            Self::Cut => "cut",
        }
    }
}

fn count(outcome: Outcome) {
    metrics::counter!("yagra_config_relay_sessions_total", "outcome" => outcome.as_str())
        .increment(1);
}

/// Why an open is refused, or `None` when this poller may connect. Pure, so the rule is tested
/// without a socket.
fn refusal(ws: &WorkingSet, open: &RelayTarget) -> Option<&'static str> {
    if !RELAY_PORTS.contains(&open.port) {
        return Some("the port is not one a relay connects to");
    }
    if !ws.polls_at(open.node_id, open.target) {
        return Some("the node is not polled by this poller at that address");
    }
    None
}

/// Send a frame back to core. A failure ends the session: core is not listening any more.
async fn reply(bus: &Arc<dyn RelayBus>, frame: RelayFrame) -> bool {
    bus.publish_relay_to_core(frame).await.is_ok()
}

fn frame(poller_id: &str, session: Uuid, seq: u64) -> RelayFrame {
    RelayFrame {
        poller_id: poller_id.to_owned(),
        session,
        seq,
        open: None,
        bytes: String::new(),
        close: false,
        refused: None,
    }
}

fn closing(poller_id: &str, session: Uuid, seq: u64, why: Option<&str>) -> RelayFrame {
    RelayFrame {
        close: true,
        refused: why.map(str::to_owned),
        ..frame(poller_id, session, seq)
    }
}

/// Serve relay frames for the lifetime of the process.
pub(crate) async fn run_relay_loop<S>(
    mut stream: S,
    poller_id: String,
    working_set: Arc<Mutex<WorkingSet>>,
    bus: Arc<dyn RelayBus>,
) where
    S: futures::Stream<Item = RelayFrame> + Unpin,
{
    use futures::StreamExt;
    let mut sessions: HashMap<Uuid, mpsc::Sender<RelayFrame>> = HashMap::new();
    while let Some(f) = stream.next().await {
        if f.poller_id != poller_id {
            continue;
        }
        if let Some(open) = f.open.clone() {
            sessions.retain(|_, tx| !tx.is_closed());
            if sessions.contains_key(&f.session) {
                continue;
            }
            // The lock is held for this statement only, never across an `.await`.
            let why = match working_set.lock() {
                Ok(ws) => refusal(&ws, &open),
                Err(_) => Some("the working set could not be read"),
            };
            let why = why.or((sessions.len() >= MAX_SESSIONS).then_some("too many relay sessions"));
            if let Some(why) = why {
                tracing::warn!(node = %open.node_id, reason = why, "config relay refused");
                count(Outcome::Refused);
                reply(&bus, closing(&poller_id, f.session, 0, Some(why))).await;
                continue;
            }
            let (tx, rx) = mpsc::channel(SESSION_QUEUE);
            sessions.insert(f.session, tx);
            tokio::spawn(run_session(
                poller_id.clone(),
                f.session,
                open,
                rx,
                bus.clone(),
            ));
            continue;
        }
        let Some(tx) = sessions.get(&f.session) else {
            continue;
        };
        let id = f.session;
        if tx.try_send(f).is_err() {
            // Full or gone. Dropping the sender ends the session, which then says so itself.
            sessions.remove(&id);
        }
    }
}

/// One session: connect, then pass bytes until either side closes or a limit is reached.
async fn run_session(
    poller_id: String,
    session: Uuid,
    open: RelayTarget,
    mut rx: mpsc::Receiver<RelayFrame>,
    bus: Arc<dyn RelayBus>,
) {
    let addr = SocketAddr::new(open.target, open.port);
    let stream = match timeout(
        Duration::from_secs(RELAY_CONNECT_SECS),
        TcpStream::connect(addr),
    )
    .await
    {
        Ok(Ok(s)) => s,
        Ok(Err(_)) | Err(_) => {
            count(Outcome::Unreachable);
            reply(
                &bus,
                closing(
                    &poller_id,
                    session,
                    0,
                    Some("the device did not accept the connection"),
                ),
            )
            .await;
            return;
        }
    };
    if !reply(&bus, frame(&poller_id, session, 0)).await {
        count(Outcome::Cut);
        return;
    }
    let outcome = carry(&poller_id, session, stream, &mut rx, &bus).await;
    count(outcome);
}

/// The two directions of one opened session. Returns how it ended.
async fn carry(
    poller_id: &str,
    session: Uuid,
    stream: TcpStream,
    rx: &mut mpsc::Receiver<RelayFrame>,
    bus: &Arc<dyn RelayBus>,
) -> Outcome {
    let (mut read, mut write) = stream.into_split();
    let mut buf = vec![0u8; RELAY_CHUNK_BYTES];
    let mut sent: u64 = 1;
    // Core's frame 0 was the open.
    let mut expect: u64 = 1;
    let mut moved: u64 = 0;
    let deadline = Instant::now() + Duration::from_secs(RELAY_MAX_SECS);
    let idle = Duration::from_secs(RELAY_IDLE_SECS);
    let mut last = Instant::now();
    loop {
        let cut = |why: &'static str| closing(poller_id, session, sent, Some(why));
        let wake = (last + idle).min(deadline);
        tokio::select! {
            got = rx.recv() => {
                let Some(f) = got else {
                    reply(bus, cut("the relay fell behind")).await;
                    return Outcome::Cut;
                };
                if f.seq != expect {
                    reply(bus, cut("a relay frame was lost")).await;
                    return Outcome::Cut;
                }
                expect += 1;
                let Some(bytes) = f.slice() else {
                    reply(bus, cut("a relay frame could not be read")).await;
                    return Outcome::Cut;
                };
                moved += bytes.len() as u64;
                if moved > RELAY_MAX_BYTES {
                    reply(bus, cut("the session carried too much")).await;
                    return Outcome::Cut;
                }
                if !bytes.is_empty() {
                    // Bounded by the same limits as the wait: a device that stops reading would
                    // otherwise hold this arm, and the idle and length limits with it, for ever.
                    match tokio::time::timeout_at(wake, write.write_all(&bytes)).await {
                        Ok(Ok(())) => {}
                        Ok(Err(_)) => {
                            reply(bus, closing(poller_id, session, sent, None)).await;
                            return Outcome::Closed;
                        }
                        Err(_) => {
                            reply(bus, cut("nothing moved for too long")).await;
                            return Outcome::Cut;
                        }
                    }
                }
                if f.close {
                    let _ = write.shutdown().await;
                    return Outcome::Closed;
                }
                last = Instant::now();
            }
            n = read.read(&mut buf) => {
                let n = match n {
                    Ok(0) | Err(_) => {
                        reply(bus, closing(poller_id, session, sent, None)).await;
                        return Outcome::Closed;
                    }
                    Ok(n) => n,
                };
                moved += n as u64;
                if moved > RELAY_MAX_BYTES {
                    reply(bus, cut("the session carried too much")).await;
                    return Outcome::Cut;
                }
                metrics::counter!("yagra_config_relay_bytes_total").increment(n as u64);
                let out = RelayFrame {
                    bytes: encode_raw(&buf[..n]),
                    ..frame(poller_id, session, sent)
                };
                sent += 1;
                if !reply(bus, out).await {
                    return Outcome::Cut;
                }
                last = Instant::now();
            }
            () = tokio::time::sleep_until(wake) => {
                let why = if Instant::now() >= deadline {
                    "the session ran too long"
                } else {
                    "nothing moved for too long"
                };
                reply(bus, cut(why)).await;
                return Outcome::Cut;
            }
        }
    }
}

/// Subscribe to this poller's relay frames and start the loop. Unconditional: whether the broker
/// lets the subscription through is what the heartbeat's claim reports
/// ([`yagra_bus::CAP_CONFIG_RELAY`] or its `-refused` twin).
pub(crate) async fn start(
    bus: &Arc<NatsBus>,
    poller_id: &str,
    working_set: &Arc<Mutex<WorkingSet>>,
    shutdown: &CancellationToken,
) -> anyhow::Result<()> {
    let sub = Box::pin(bus.subscribe_relay_to_poller(poller_id).await?);
    let relay_bus: Arc<dyn RelayBus> = bus.clone();
    spawn_cancellable(
        shutdown,
        run_relay_loop(sub, poller_id.to_owned(), working_set.clone(), relay_bus),
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{IpAddr, Ipv4Addr};
    use std::time::Instant as StdInstant;
    use tokio::net::TcpListener;
    use yagra_bus::{IcmpCheck, InMemoryBus, JobSpec, NodeJobs, PollJob, SyncMsg};
    use yagra_bus::{WorkingSetSnapshot, CAP_CONFIG_RELAY};
    use yagra_common::NodeId;

    fn node() -> NodeId {
        NodeId::from(Uuid::from_u128(5))
    }

    fn working_set_at(ip: IpAddr) -> Arc<Mutex<WorkingSet>> {
        let mut ws = WorkingSet::new();
        let job = PollJob::icmp(Uuid::nil(), node(), ip, IcmpCheck::default(), 30);
        ws.apply(
            SyncMsg::SnapshotChunk(WorkingSetSnapshot {
                poller_id: "p1".to_owned(),
                epoch: Uuid::from_u128(1),
                seq: 1,
                chunk_index: 0,
                chunk_total: 1,
                nodes: vec![NodeJobs {
                    node_id: node(),
                    specs: vec![JobSpec::from_job(&job)],
                }],
                total_nodes: 0,
                pool: None,
            }),
            StdInstant::now(),
            &mut |_| 0,
        );
        Arc::new(Mutex::new(ws))
    }

    #[test]
    fn only_a_node_polled_here_at_that_address_on_ssh_or_telnet_is_opened() {
        let at = IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1));
        let ws = working_set_at(at);
        let ws = ws.lock().expect("lock");
        let open = |target, port| RelayTarget {
            node_id: node(),
            target,
            port,
        };
        assert_eq!(refusal(&ws, &open(at, 22)), None);
        assert_eq!(refusal(&ws, &open(at, 23)), None);
        assert!(refusal(&ws, &open(at, 80)).is_some(), "any other port");
        assert!(
            refusal(&ws, &open(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 9)), 22)).is_some(),
            "another address"
        );
        let other = RelayTarget {
            node_id: NodeId::from(Uuid::from_u128(6)),
            ..open(at, 22)
        };
        assert!(refusal(&ws, &other).is_some(), "a node polled elsewhere");
        assert_eq!(CAP_CONFIG_RELAY, "config-relay");
    }

    fn opening(open: RelayTarget, session: u128) -> RelayFrame {
        RelayFrame {
            open: Some(open),
            ..frame("p1", Uuid::from_u128(session), 0)
        }
    }

    /// The loop refuses over the bus, answering frame 0 with the reason and a close.
    #[tokio::test]
    async fn an_open_for_another_port_is_answered_with_a_refusal() {
        use futures::StreamExt;
        use tokio_stream::wrappers::BroadcastStream;
        let ip = IpAddr::V4(Ipv4Addr::LOCALHOST);
        let bus = Arc::new(InMemoryBus::new(64));
        let mut back = bus.subscribe_relay_to_core();
        let to_poller = BroadcastStream::new(bus.subscribe_relay_to_poller())
            .filter_map(|r| async move { r.ok().map(|(_, f)| f) });
        let relay_bus: Arc<dyn RelayBus> = bus.clone();
        tokio::spawn(run_relay_loop(
            Box::pin(to_poller),
            "p1".to_owned(),
            working_set_at(ip),
            relay_bus,
        ));
        let open = RelayTarget {
            node_id: node(),
            target: ip,
            port: 8080,
        };
        // Addressed to another poller: ignored, so the next answer is the refusal below.
        let mut elsewhere = opening(open.clone(), 1);
        elsewhere.poller_id = "p2".to_owned();
        RelayBus::publish_relay_to_poller(&*bus, elsewhere)
            .await
            .expect("publish");
        RelayBus::publish_relay_to_poller(&*bus, opening(open, 2))
            .await
            .expect("publish");
        let answer = timeout(Duration::from_secs(5), back.recv())
            .await
            .expect("an answer")
            .expect("frame");
        assert_eq!(answer.session, Uuid::from_u128(2));
        assert_eq!(answer.seq, 0);
        assert!(answer.close);
        assert!(answer.refused.is_some());
    }

    /// A session carries bytes both ways in order, and ends when the device closes.
    #[tokio::test]
    async fn a_session_carries_bytes_both_ways_and_closes_with_the_device() {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let open = RelayTarget {
            node_id: node(),
            target: IpAddr::V4(Ipv4Addr::LOCALHOST),
            port: listener.local_addr().expect("addr").port(),
        };
        let device = tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.expect("accept");
            sock.write_all(b"login: ").await.expect("write");
            let mut got = [0u8; 6];
            sock.read_exact(&mut got).await.expect("read");
            assert_eq!(&got, b"admin\n");
            sock.write_all(b"bye").await.expect("write");
        });
        let bus = Arc::new(InMemoryBus::new(64));
        let mut back = bus.subscribe_relay_to_core();
        let (tx, rx) = mpsc::channel(8);
        let relay_bus: Arc<dyn RelayBus> = bus.clone();
        let session = tokio::spawn(run_session(
            "p1".to_owned(),
            Uuid::from_u128(3),
            open,
            rx,
            relay_bus,
        ));
        async fn next(back: &mut tokio::sync::broadcast::Receiver<RelayFrame>) -> RelayFrame {
            timeout(Duration::from_secs(5), back.recv())
                .await
                .expect("a frame")
                .expect("frame")
        }
        let opened = next(&mut back).await;
        assert_eq!(
            (opened.seq, opened.close, opened.refused.clone()),
            (0, false, None)
        );
        let prompt = next(&mut back).await;
        assert_eq!(prompt.seq, 1);
        assert_eq!(prompt.slice().as_deref(), Some(&b"login: "[..]));
        tx.send(RelayFrame {
            bytes: encode_raw(b"admin\n"),
            ..frame("p1", Uuid::from_u128(3), 1)
        })
        .await
        .expect("send");
        let mut rest = Vec::new();
        loop {
            let f = next(&mut back).await;
            rest.extend(f.slice().expect("bytes"));
            if f.close {
                break;
            }
        }
        assert_eq!(rest, b"bye");
        device.await.expect("device");
        session.await.expect("session");
    }

    /// A frame out of order ends the session rather than writing a gap into the device's stream.
    #[tokio::test]
    async fn a_lost_frame_ends_the_session() {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let open = RelayTarget {
            node_id: node(),
            target: IpAddr::V4(Ipv4Addr::LOCALHOST),
            port: listener.local_addr().expect("addr").port(),
        };
        let _device = tokio::spawn(async move {
            let (sock, _) = listener.accept().await.expect("accept");
            tokio::time::sleep(Duration::from_secs(5)).await;
            drop(sock);
        });
        let bus = Arc::new(InMemoryBus::new(64));
        let mut back = bus.subscribe_relay_to_core();
        let (tx, rx) = mpsc::channel(8);
        let relay_bus: Arc<dyn RelayBus> = bus.clone();
        tokio::spawn(run_session(
            "p1".to_owned(),
            Uuid::from_u128(4),
            open,
            rx,
            relay_bus,
        ));
        let opened = back.recv().await.expect("opened");
        assert!(!opened.close);
        tx.send(frame("p1", Uuid::from_u128(4), 2))
            .await
            .expect("send");
        let cut = timeout(Duration::from_secs(5), back.recv())
            .await
            .expect("an answer")
            .expect("frame");
        assert!(cut.close);
        assert_eq!(cut.refused.as_deref(), Some("a relay frame was lost"));
    }

    #[tokio::test]
    async fn a_device_that_refuses_the_connection_is_reported_unreachable() {
        // Bind and drop, so the port is closed.
        let port = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind")
            .local_addr()
            .expect("addr")
            .port();
        let bus = Arc::new(InMemoryBus::new(64));
        let mut back = bus.subscribe_relay_to_core();
        let (_tx, rx) = mpsc::channel(8);
        let relay_bus: Arc<dyn RelayBus> = bus.clone();
        run_session(
            "p1".to_owned(),
            Uuid::from_u128(5),
            RelayTarget {
                node_id: node(),
                target: IpAddr::V4(Ipv4Addr::LOCALHOST),
                port,
            },
            rx,
            relay_bus,
        )
        .await;
        let answer = back.recv().await.expect("answer");
        assert!(answer.close);
        assert_eq!(answer.seq, 0);
        assert!(answer.refused.is_some());
    }
}
