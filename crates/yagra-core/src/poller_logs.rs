// SPDX-License-Identifier: AGPL-3.0-only
//! Collecting a remote-site poller's log over the bus, for the support bundle (ADR-045 Increment 4).
//!
//! # The half the disk cannot reach
//!
//! ADR-045 決定 2 put core's own log on a named volume so a bundle taken *after* a crash still
//! carries the run that died. Increment 3 extended that to a poller sharing the volume. A poller at
//! a monitored site shares nothing — its disk is three networks away — so its log reaches a bundle
//! only by being asked for.
//!
//! The two paths are **complements, not alternatives**, and the table is worth keeping in mind when
//! reading an omission in a bundle:
//!
//! | | reaches | cannot reach |
//! |---|---|---|
//! | disk (Inc.3) | co-located, **including a run that already died** | another host |
//! | bus (here)   | **live, at another site** | one that is not running |
//!
//! # Shape: one long-lived subscriber, many short-lived collections
//!
//! Chunks arrive on a single fan-in subject and are routed by `request_id`. A subscription per
//! bundle would mean a subscribe/unsubscribe round trip on the bus for something a human presses a
//! button for a few times a year, and — worse — a race where the first chunk arrives before the
//! subscription is established. So [`PollerLogCollector::run_reply_loop`] runs for the process
//! lifetime and [`PollerLogCollector::collect`] registers an inbox for the duration of one bundle.
//!
//! # What this is not allowed to do
//!
//! **It cannot fail a bundle.** Every outcome — a site that says nothing, a site that refuses, a
//! chunk sequence with a hole, a bus that is down — resolves to an omission naming the poller. A
//! support bundle is requested *because* something is broken, and refusing to produce one because a
//! remote site is unreachable would withhold the evidence at exactly the wrong moment. That is the
//! module-level rule `api/support.rs` states, applied one seam further out.
//!
//! **It does not scan for secrets, and that is deliberate rather than an omission.** Core's literal
//! set is built from core's own environment and structurally cannot contain a monitored device's
//! community string. The scan therefore runs on the poller, which holds the plaintext (see
//! `yagra-poller`'s `support_logs`). What core *does* still do is run its own
//! `support_bundle::SecretScan` over the assembled archive, so a remote poller's bytes are checked
//! against core's rules as well — two scans with different knowledge, neither sufficient alone.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::mpsc;
use uuid::Uuid;
use yagra_bus::{LogBus, PollerLogChunk, PollerLogRequest};

/// How long to wait for every asked poller to finish answering.
///
/// Sized against a WAN round trip plus a few megabytes of chunks, not against a poll timeout: the
/// sites this exists for are the ones on a slow link. Bounded because a bundle is a human waiting at
/// a browser, and a site that is simply gone must not be able to hold that open.
pub const REPLY_DEADLINE: Duration = Duration::from_secs(20);

/// How many raw log bytes core asks each poller for.
///
/// Per poller rather than shared, so a chatty site cannot crowd out a quiet one — and matched to the
/// per-poller share the disk path already uses, so a bundle does not carry wildly more from a remote
/// site than from a co-located one.
pub const PER_POLLER_MAX_BYTES: u64 = 2 * 1024 * 1024;

/// Ceiling on what all remote pollers contribute to one bundle.
///
/// The same value as the disk path's poller budget and separate from it, so the two cannot combine
/// into a bundle nobody can download. Core's own log budget is untouched by either.
pub const TOTAL_MAX_BYTES: usize = 8 * 1024 * 1024;

/// One remote poller's log file, reassembled.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteLogFile {
    /// The poller that sent it.
    pub poller_id: String,
    /// The filename as written at that site.
    pub name: String,
    pub bytes: Vec<u8>,
}

/// Why one poller contributed nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteLogGap {
    pub poller_id: String,
    /// Written for the manifest, so it reads as an explanation rather than a status code.
    pub why: String,
}

/// The outcome of one fan-out: what arrived, and — equally load-bearing — what did not and why.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct RemoteLogs {
    pub files: Vec<RemoteLogFile>,
    /// One entry per poller that was asked and produced nothing usable. **Never left empty by
    /// accident**: a site that says nothing has to be named, because "no file from Tokyo" and "no
    /// poller in Tokyo" look identical in an archive (ADR-045 決定 3).
    pub gaps: Vec<RemoteLogGap>,
    /// How many pollers were asked at all.
    ///
    /// Carried so the bundle can say that the fan-out **ran**, which nothing else in the artefact
    /// reveals. Found by taking a real bundle from a single-node deployment: every poller there is
    /// co-located, so every reply is deduplicated against its disk copy and the archive comes out
    /// byte-identical whether the bus path worked perfectly or was dead. That is precisely the
    /// ambiguity ADR-045 名指しした — **an aggregate of zero means "safe" and "the check never ran"
    /// at the same time** — and the fix is the same one: publish the reason, not just the number.
    pub asked: usize,
    /// How many of them sent at least one chunk back.
    ///
    /// ⚠️ **`asked` alone is not enough, and the first draft of this got it wrong.** A poller whose
    /// reply is deduplicated against its disk copy and a poller that never answered at all are both
    /// invisible in the file list, so a message built from `asked` claimed "and answered" about a
    /// site that may have said nothing. Two numbers, or the sentence is a guess.
    pub answered: usize,
}

/// Per-poller reassembly state while a fan-out is in flight.
#[derive(Default)]
struct Pending {
    /// `name` → bytes so far. A poller may send several files.
    files: Vec<(String, Vec<u8>)>,
    /// Next sequence number expected, so a hole is detected rather than silently concatenated.
    next_seq: u32,
    refused: Option<String>,
    gap: Option<String>,
    done: bool,
    /// Whether this poller sent anything at all. Distinct from `done`, which is also set by a
    /// publish failure on our side and by a deadline — neither of which is the site answering.
    answered: bool,
}

/// Asks pollers for their logs and reassembles the answers (ADR-045 Inc.4).
pub struct PollerLogCollector {
    bus: Arc<dyn LogBus>,
    /// request_id → the inbox of the collection waiting on it. Entries live for one bundle.
    inflight: Mutex<HashMap<Uuid, mpsc::UnboundedSender<PollerLogChunk>>>,
}

impl PollerLogCollector {
    #[must_use]
    pub fn new(bus: Arc<dyn LogBus>) -> Self {
        Self {
            bus,
            inflight: Mutex::new(HashMap::new()),
        }
    }

    /// Route incoming chunks to whichever collection is waiting for them. Runs for the process
    /// lifetime.
    ///
    /// A chunk for an unknown `request_id` is dropped without a warning at `warn`: it is the normal
    /// consequence of a slow site answering after its bundle's deadline expired, and logging it as a
    /// problem would train an operator to ignore the log during exactly the incident this feature is
    /// for.
    pub async fn run_reply_loop<S>(self: Arc<Self>, mut stream: S)
    where
        S: futures::Stream<Item = PollerLogChunk> + Unpin,
    {
        use futures::StreamExt;
        while let Some(chunk) = stream.next().await {
            let tx = self
                .inflight
                .lock()
                .ok()
                .and_then(|m| m.get(&chunk.request_id).cloned());
            match tx {
                Some(tx) => {
                    let _ = tx.send(chunk);
                }
                None => tracing::debug!(
                    request = %chunk.request_id,
                    poller = %chunk.poller_id,
                    "dropping a support-log chunk for a request that is no longer waiting"
                ),
            }
        }
    }

    /// Ask every poller in `targets` for its log since `since_unix_s`, and wait up to
    /// [`REPLY_DEADLINE`] for the answers.
    ///
    /// Never returns an error: a failure to *ask* is a gap for that poller like any other, because
    /// the caller is assembling a support bundle and has nothing better to do with an error.
    pub async fn collect(&self, targets: &[String], since_unix_s: i64) -> RemoteLogs {
        if targets.is_empty() {
            return RemoteLogs::default();
        }
        let request_id = Uuid::new_v4();
        let (tx, rx) = mpsc::unbounded_channel();
        if let Ok(mut m) = self.inflight.lock() {
            m.insert(request_id, tx);
        }

        let mut pending: HashMap<String, Pending> = HashMap::new();
        for id in targets {
            let mut p = Pending::default();
            if let Err(e) = self
                .bus
                .publish_poller_log_request(PollerLogRequest {
                    poller_id: id.clone(),
                    request_id,
                    since_unix_s,
                    max_bytes: PER_POLLER_MAX_BYTES,
                })
                .await
            {
                tracing::warn!(poller = %id, error = %e, "failed to ask a poller for its log");
                p.gap = Some(
                    "the request could not be published to the bus; see health/dependencies.json"
                        .to_owned(),
                );
                p.done = true;
            }
            pending.insert(id.clone(), p);
        }

        let outcome = tokio::time::timeout(REPLY_DEADLINE, gather(rx, &mut pending)).await;
        if outcome.is_err() {
            tracing::warn!(
                request = %request_id,
                "support-log collection hit its deadline; unanswered pollers are recorded as gaps"
            );
        }
        // Unregister before assembling, so a late chunk lands in the debug branch above rather than
        // in a channel nobody will ever read.
        if let Ok(mut m) = self.inflight.lock() {
            m.remove(&request_id);
        }
        let mut out = assemble(pending);
        out.asked = targets.len();
        out
    }
}

/// Drain chunks until every poller has sent its terminal message. Returns when all are `done`; the
/// caller bounds it with a timeout, which is what makes a silent site cost the deadline once rather
/// than forever.
async fn gather(
    mut rx: mpsc::UnboundedReceiver<PollerLogChunk>,
    pending: &mut HashMap<String, Pending>,
) {
    while pending.values().any(|p| !p.done) {
        let Some(chunk) = rx.recv().await else {
            return;
        };
        // A chunk from a poller we did not ask. It cannot be attributed to a site whose log we
        // requested, so carrying it would put unrequested bytes in the archive.
        let Some(p) = pending.get_mut(&chunk.poller_id) else {
            tracing::warn!(
                poller = %chunk.poller_id,
                "ignoring a support-log chunk from a poller this bundle did not ask"
            );
            continue;
        };
        if p.done {
            continue;
        }
        // Recorded before any other decision: this site responded, whatever the reply turns out to
        // say. It is the only number that separates "answered and was deduplicated" from "never
        // answered", and both look identical in the file list.
        p.answered = true;
        if let Some(why) = chunk.refused {
            p.refused = Some(why);
            p.done = true;
            continue;
        }
        // Ordering is the one thing a fan-in subject does not guarantee across a reconnect. A hole
        // truncates this poller's contribution rather than splicing two halves of a log file
        // together, because a silently spliced log is a wrong answer rather than a missing one.
        if chunk.seq != p.next_seq {
            p.gap = Some(format!(
                "its reply arrived out of order (expected chunk {}, got {}), so nothing from this \
                 poller was kept — a spliced log would read as a continuous one",
                p.next_seq, chunk.seq
            ));
            p.files.clear();
            p.done = true;
            continue;
        }
        p.next_seq += 1;
        if chunk.last {
            p.done = true;
        }
        let Some(slice) = chunk.slice() else {
            p.gap = Some(
                "part of its reply was not decodable, so nothing from this poller was kept"
                    .to_owned(),
            );
            p.files.clear();
            p.done = true;
            continue;
        };
        if slice.is_empty() || chunk.name.is_empty() {
            continue;
        }
        match p.files.last_mut() {
            Some((name, bytes)) if *name == chunk.name => bytes.extend_from_slice(&slice),
            _ => p.files.push((chunk.name, slice)),
        }
    }
}

/// Turn the per-poller state into files and gaps, applying the fleet-wide byte ceiling.
fn assemble(pending: HashMap<String, Pending>) -> RemoteLogs {
    // Sorted so a bundle's contents do not depend on `HashMap` iteration order — two bundles taken
    // a minute apart should differ because the deployment changed, not because the map rehashed.
    let mut ids: Vec<String> = pending.keys().cloned().collect();
    ids.sort_unstable();

    let mut out = RemoteLogs {
        answered: pending.values().filter(|p| p.answered).count(),
        ..RemoteLogs::default()
    };
    let mut total = 0usize;
    for id in ids {
        let Some(p) = pending.get(&id) else { continue };
        if let Some(why) = &p.refused {
            out.gaps.push(RemoteLogGap {
                poller_id: id,
                why: format!(
                    "the poller refused: {why}. Its own scan runs there because core cannot see a \
                     device credential — see the ADR-045 note in yagra-poller's support_logs."
                ),
            });
            continue;
        }
        if let Some(why) = &p.gap {
            out.gaps.push(RemoteLogGap {
                poller_id: id,
                why: why.clone(),
            });
            continue;
        }
        if !p.done {
            out.gaps.push(RemoteLogGap {
                poller_id: id,
                why: format!(
                    "it did not finish answering within {}s. It may be unreachable, mid-restart, or \
                     on a link too slow for its log — health/pollers.json says when core last heard \
                     from it.",
                    REPLY_DEADLINE.as_secs()
                ),
            });
            continue;
        }
        if p.files.is_empty() {
            out.gaps.push(RemoteLogGap {
                poller_id: id,
                why: "it answered, and had no log file in the requested window. That is a real \
                      answer: the site is reachable and simply wrote nothing that recently."
                    .to_owned(),
            });
            continue;
        }
        for (name, bytes) in &p.files {
            if total.saturating_add(bytes.len()) > TOTAL_MAX_BYTES {
                out.gaps.push(RemoteLogGap {
                    poller_id: id.clone(),
                    why: format!(
                        "the {TOTAL_MAX_BYTES}-byte ceiling on remote poller logs was reached \
                         before this file. Request a shorter window, or take a second bundle."
                    ),
                });
                break;
            }
            total += bytes.len();
            out.files.push(RemoteLogFile {
                poller_id: id.clone(),
                name: name.clone(),
                bytes: bytes.clone(),
            });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use yagra_bus::{encode_raw, InMemoryBus};

    fn chunk(
        req: Uuid,
        poller: &str,
        seq: u32,
        last: bool,
        name: &str,
        body: &[u8],
    ) -> PollerLogChunk {
        PollerLogChunk {
            request_id: req,
            poller_id: poller.to_owned(),
            seq,
            last,
            name: name.to_owned(),
            bytes: encode_raw(body),
            refused: None,
        }
    }

    /// Drive `gather` directly with a scripted chunk sequence — the reassembly is the interesting
    /// half and forcing it through a bus would only add a scheduler to the test.
    async fn reassemble(targets: &[&str], chunks: Vec<PollerLogChunk>) -> RemoteLogs {
        let mut pending: HashMap<String, Pending> = targets
            .iter()
            .map(|t| ((*t).to_owned(), Pending::default()))
            .collect();
        let (tx, rx) = mpsc::unbounded_channel();
        for c in chunks {
            tx.send(c).unwrap();
        }
        drop(tx);
        gather(rx, &mut pending).await;
        assemble(pending)
    }

    /// The ordinary case: a file split across chunks comes back as one file, attributed to the
    /// poller that sent it.
    #[tokio::test]
    async fn chunks_of_one_file_are_reassembled_in_order() {
        let req = Uuid::from_u128(1);
        let out = reassemble(
            &["edge-1"],
            vec![
                chunk(
                    req,
                    "edge-1",
                    0,
                    false,
                    "yagra-poller-edge-1.2026-08-16-10.log",
                    b"{\"a\":1}\n",
                ),
                chunk(
                    req,
                    "edge-1",
                    1,
                    false,
                    "yagra-poller-edge-1.2026-08-16-10.log",
                    b"{\"b\":2}\n",
                ),
                chunk(req, "edge-1", 2, true, "", b""),
            ],
        )
        .await;

        assert!(out.gaps.is_empty(), "{:?}", out.gaps);
        assert_eq!(out.files.len(), 1);
        assert_eq!(out.files[0].poller_id, "edge-1");
        assert_eq!(out.files[0].bytes, b"{\"a\":1}\n{\"b\":2}\n");
    }

    /// A hole in the sequence throws that poller's contribution away rather than splicing two
    /// halves together. **A spliced log is a wrong answer, not a missing one** — it reads as a
    /// continuous record of a period it does not cover, which is the failure this whole feature
    /// exists to prevent one level up.
    #[tokio::test]
    async fn a_missing_chunk_discards_that_pollers_contribution_and_says_so() {
        let req = Uuid::from_u128(2);
        let out = reassemble(
            &["edge-1"],
            vec![
                chunk(req, "edge-1", 0, false, "a.log", b"first\n"),
                // seq 1 never arrives
                chunk(req, "edge-1", 2, true, "a.log", b"third\n"),
            ],
        )
        .await;

        assert!(out.files.is_empty());
        assert_eq!(out.gaps.len(), 1);
        assert_eq!(out.gaps[0].poller_id, "edge-1");
        assert!(
            out.gaps[0].why.contains("out of order"),
            "{}",
            out.gaps[0].why
        );
    }

    /// A refusal is carried through as the poller's own words, and never as an absence. It is the
    /// one gap an operator can act on.
    #[tokio::test]
    async fn a_refusal_becomes_a_named_gap() {
        let req = Uuid::from_u128(3);
        let mut refusal = chunk(req, "edge-1", 0, true, "", b"");
        refusal.refused = Some("its own scan found a credential".to_owned());
        let out = reassemble(&["edge-1"], vec![refusal]).await;

        assert!(out.files.is_empty());
        assert_eq!(out.gaps.len(), 1);
        assert!(out.gaps[0].why.contains("found a credential"));
    }

    /// A site that never answers is named. This is the property the whole gap list exists for:
    /// "no file from Tokyo" and "no poller in Tokyo" are indistinguishable in an archive unless
    /// somebody writes the difference down.
    #[tokio::test]
    async fn a_poller_that_never_answers_is_still_in_the_bundle_by_name() {
        let req = Uuid::from_u128(4);
        let out = reassemble(
            &["edge-1", "edge-2"],
            vec![chunk(req, "edge-1", 0, true, "", b"")],
        )
        .await;

        // edge-1 answered with nothing; edge-2 said nothing at all. Both are gaps, and the reasons
        // must be different — one is a fact about the site, the other about the link.
        assert_eq!(out.gaps.len(), 2);
        let by_id: HashMap<&str, &str> = out
            .gaps
            .iter()
            .map(|g| (g.poller_id.as_str(), g.why.as_str()))
            .collect();
        assert!(by_id["edge-1"].contains("had no log file"), "{:?}", by_id);
        assert!(
            by_id["edge-2"].contains("did not finish answering"),
            "{:?}",
            by_id
        );
    }

    /// Bytes from a poller nobody asked are dropped. The request is per-poller, so a chunk from
    /// elsewhere is either a routing mistake or something deliberate; either way it must not become
    /// part of an archive that leaves the building.
    #[tokio::test]
    async fn a_chunk_from_an_unasked_poller_never_enters_the_bundle() {
        let req = Uuid::from_u128(5);
        let out = reassemble(
            &["edge-1"],
            vec![
                chunk(req, "intruder", 0, false, "x.log", b"unrequested\n"),
                chunk(req, "edge-1", 0, true, "", b""),
            ],
        )
        .await;
        assert!(out.files.is_empty());
        assert!(out.gaps.iter().all(|g| g.poller_id != "intruder"));
    }

    /// The fan-out end to end over the in-memory bus, including the property that makes the
    /// deadline survivable: **`collect` returns, with a gap, rather than hanging**, when nothing
    /// answers. A support bundle must never be blocked by a site that is gone.
    #[tokio::test(start_paused = true)]
    async fn a_silent_fleet_still_returns_a_bundle_worth_of_answer() {
        let bus = Arc::new(InMemoryBus::new(8));
        let collector = PollerLogCollector::new(bus.clone());
        let out = collector.collect(&["edge-1".to_owned()], 0).await;
        assert!(out.files.is_empty());
        assert_eq!(out.gaps.len(), 1);
        assert_eq!(out.gaps[0].poller_id, "edge-1");
    }

    /// Asking nobody costs nothing — no request id, no deadline, no wait. The single-node
    /// deployment is the common case and it has no remote poller at all.
    #[tokio::test]
    async fn an_empty_target_list_is_free() {
        let bus = Arc::new(InMemoryBus::new(8));
        let collector = PollerLogCollector::new(bus);
        assert_eq!(collector.collect(&[], 0).await, RemoteLogs::default());
    }

    /// A request is published per target, addressed to that target, carrying the shared correlation
    /// id — the shape the poller's `run_log_request_loop` drops anything else against.
    #[tokio::test(start_paused = true)]
    async fn every_target_is_asked_on_its_own_address() {
        let bus = Arc::new(InMemoryBus::new(8));
        let mut requests = bus.subscribe_poller_log_requests();
        let collector = PollerLogCollector::new(bus.clone());

        let handle = tokio::spawn(async move {
            collector
                .collect(&["edge-1".to_owned(), "edge-2".to_owned()], 42)
                .await
        });

        let (a_id, a) = requests.recv().await.unwrap();
        let (b_id, b) = requests.recv().await.unwrap();
        assert_eq!(a_id, a.poller_id);
        assert_eq!(b_id, b.poller_id);
        let mut ids = vec![a.poller_id.clone(), b.poller_id.clone()];
        ids.sort();
        assert_eq!(ids, vec!["edge-1", "edge-2"]);
        assert_eq!(a.request_id, b.request_id, "one bundle, one correlation id");
        assert_eq!(a.since_unix_s, 42);
        assert_eq!(a.max_bytes, PER_POLLER_MAX_BYTES);

        handle.await.unwrap();
    }

    /// The fleet ceiling bites as a **named** gap rather than a shorter file list. A bundle that
    /// silently carried less would read as "that site logged nothing".
    #[tokio::test]
    async fn the_fleet_ceiling_is_reported_against_the_poller_it_stopped_at() {
        let req = Uuid::from_u128(6);
        let big = vec![b'x'; TOTAL_MAX_BYTES];
        let out = reassemble(
            &["edge-1", "edge-2"],
            vec![
                chunk(req, "edge-1", 0, false, "a.log", &big),
                chunk(req, "edge-1", 1, true, "", b""),
                chunk(req, "edge-2", 0, false, "b.log", b"small\n"),
                chunk(req, "edge-2", 1, true, "", b""),
            ],
        )
        .await;

        assert_eq!(out.files.len(), 1, "the first poller filled the budget");
        assert_eq!(out.files[0].poller_id, "edge-1");
        assert_eq!(out.gaps.len(), 1);
        assert_eq!(out.gaps[0].poller_id, "edge-2");
        assert!(out.gaps[0].why.contains("ceiling"), "{}", out.gaps[0].why);
    }
}
