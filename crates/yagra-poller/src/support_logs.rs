// SPDX-License-Identifier: AGPL-3.0-only
//! Answering core's request for this poller's own log (ADR-045 Increment 4).
//!
//! # What this is for, and what it is not
//!
//! A support bundle is assembled by core (ADR-045 決定 1). Increment 3 gave it a co-located
//! poller's log for free — same host, one shared volume — but a poller at a monitored site has its
//! own disk, and nothing core can read reaches it. This module is that crossing.
//!
//! **It is not a substitute for the disk path, and the disk path is not a substitute for it.** They
//! cover disjoint failures, which is the whole reason both exist:
//!
//! | | reaches | cannot reach |
//! |---|---|---|
//! | disk (Inc.3) | a co-located poller, **including a run that has already died** | another host |
//! | bus (this)   | **a live poller at another site** | one that is not running |
//!
//! A poller killed by the OOM killer cannot answer a request; its last hour is on the volume beside
//! core, if it shares one. A poller three networks away is running fine and has nothing on any disk
//! core can see. Same argument ADR-045 決定 2 made for core, one component over.
//!
//! # The secret scan runs **here**, and that is the security decision of the increment
//!
//! Core's redaction scan (`support_bundle::SecretScan`) is built from the literal secret values core
//! can see — its own environment. That set **structurally cannot** contain a monitored device's SNMP
//! community or SNMPv3 passphrase: core decrypts those from the credential store and inlines them
//! into a job, and from that moment the plaintext lives in the poller. So a scan of a poller's log
//! run on core would be checking for the wrong secrets.
//!
//! The scan therefore runs on the side that holds the knowledge, and it is **fail-closed exactly as
//! core's is**: a match refuses this poller's whole contribution rather than redacting a line, and
//! the refusal names the rule and never the value. Redact-and-continue would assume the pattern set
//! is complete, and the cost of being wrong is a device credential crossing an air gap.
//!
//! Two limits, both stated rather than hidden:
//!
//! * The literal set comes from the **current** working set. A credential removed since a log line
//!   was written is not in it. Same class of gap as core's, one level down.
//! * [`MIN_SECRET_LEN`] applies for the same reason it does in core: a lab community of `public` is
//!   six characters and would forbid an ordinary English word, refusing every reply forever — and a
//!   check that always refuses gets switched off rather than fixed.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use yagra_bus::{encode_raw, LogBus, PollerLogChunk, PollerLogRequest};

use crate::working_set::WorkingSet;

/// The service name this binary passed to `yagra_telemetry::init_instance`, and therefore the first
/// half of its log filenames. Named here so the reader and the writer cannot drift apart.
pub(crate) const SERVICE_NAME: &str = "yagra-poller";

/// Raw bytes per chunk.
///
/// NATS's default maximum payload is 1 MB and base64 costs 4/3, so this leaves the encoded body
/// around 340 KB — room for the JSON envelope and for a deployment that has lowered `max_payload`
/// somewhat, without paying a message per kilobyte.
const CHUNK_BYTES: usize = 256 * 1024;

/// A value shorter than this is never enforced as a secret literal.
///
/// Deliberately the same floor as `support_bundle::MIN_SECRET_LEN` in core, and for the identical
/// reason: a short value occurs by chance in ordinary text, so enforcing one would refuse every
/// reply forever. The lab community `public` is exactly the case — six characters, and the word
/// appears in prose.
const MIN_SECRET_LEN: usize = 8;

/// What a scan of one poller's own log found. `Some` names the rule that fired; the matched text is
/// deliberately not carried, so a caller cannot log it by accident.
type ScanHit = Option<&'static str>;

/// The literal credentials to look for, plus the patterns that apply regardless.
///
/// A struct rather than a bare `Vec` so the pattern half cannot be forgotten by a caller that has no
/// literals to enforce — which is the common case on a poller whose working set is all ICMP.
pub(crate) struct LogScan {
    literals: Vec<String>,
}

impl LogScan {
    /// Build from the working set's current credentials, applying the length floor.
    pub(crate) fn from_working_set(ws: &WorkingSet) -> Self {
        Self::from_literals(ws.secret_literals())
    }

    /// Split from [`Self::from_working_set`] so the floor is testable without a working set.
    pub(crate) fn from_literals(candidates: Vec<String>) -> Self {
        Self {
            literals: candidates
                .into_iter()
                .filter(|v| v.len() >= MIN_SECRET_LEN)
                .collect(),
        }
    }

    /// Scan one file's bytes. Mirrors core's rule order — literals first, because they are the
    /// strongest rule and the one a pattern list cannot express.
    pub(crate) fn check(&self, bytes: &[u8]) -> ScanHit {
        let text = String::from_utf8_lossy(bytes);
        if self.literals.iter().any(|lit| text.contains(lit.as_str())) {
            return Some("a device credential this poller currently holds");
        }
        if text.contains("PRIVATE KEY-----") {
            return Some("a PEM private key block");
        }
        None
    }
}

/// Serve support-log requests for the lifetime of the process (ADR-045 Inc.4).
///
/// One request at a time, deliberately: a bundle is taken by a human pressing a button, so there is
/// no concurrency to win, and serialising means the file read and the base64 of a multi-megabyte log
/// cannot overlap with a second copy of themselves on a small remote host.
pub(crate) async fn run_log_request_loop<S>(
    mut stream: S,
    poller_id: String,
    dir: PathBuf,
    working_set: Arc<Mutex<WorkingSet>>,
    bus: Arc<dyn LogBus>,
) where
    S: futures::Stream<Item = PollerLogRequest> + Unpin,
{
    use futures::StreamExt;
    while let Some(req) = stream.next().await {
        // Addressed to someone else: the subject already routed it, so this can only be a mistake or
        // a probe. Answering would put this site's log body on the wire for a request it was not the
        // subject of — the same reasoning `run_upgrade_loop` applies, with a disclosure at stake
        // rather than a wrong install.
        if req.poller_id != poller_id {
            tracing::warn!(
                intended = %req.poller_id,
                "ignoring a support-log request addressed elsewhere"
            );
            continue;
        }
        // The scan is built per request, from the credentials held right now. The lock is taken and
        // released in this statement — a `MutexGuard` alive across the `.await` below would make
        // the whole loop `!Send` and so unspawnable, which is the compiler enforcing the rule that
        // the poll loop must never wait behind a support request.
        let scan = working_set
            .lock()
            .ok()
            .map(|ws| LogScan::from_working_set(&ws));
        let reply = match scan {
            Some(scan) => build_reply(&req, &poller_id, &dir, &scan),
            // A poisoned lock means another task panicked holding it. Refuse rather than ship with
            // an empty literal set: "we could not build the check" must never read as "the check
            // passed" (ADR-045's lesson about a `0` that means its own opposite).
            None => {
                tracing::error!("support-log request refused: the working set is unreadable");
                refusal(
                    &req,
                    &poller_id,
                    "the working set could not be read, so the credential scan could not be built. \
                     Refusing rather than sending an unchecked log",
                )
            }
        };
        send_all(&*bus, reply).await;
    }
}

/// Collect, scan and chunk this poller's log into the messages that answer one request.
///
/// Pure apart from the filesystem read, so the whole decision — which files, the cap, the refusal,
/// the terminal marker — is reachable from a test without a bus.
pub(crate) fn build_reply(
    req: &PollerLogRequest,
    poller_id: &str,
    dir: &Path,
    scan: &LogScan,
) -> Vec<PollerLogChunk> {
    let since = std::time::UNIX_EPOCH
        + std::time::Duration::from_secs(u64::try_from(req.since_unix_s).unwrap_or(0));
    let max = usize::try_from(req.max_bytes).unwrap_or(usize::MAX);
    // The same collector core uses, from the crate that writes these files, with the same
    // newest-first ordering: when the cap bites, the hour being debugged is the one that travels.
    //
    // ⚠️ The trailing dot is load-bearing. `file_prefix` produces `yagra-poller-edge-1`, which is a
    // **prefix of** `yagra-poller-edge-10`, so selecting on it alone would make one site ship
    // another site's log. `tracing-appender` always puts a `.` between the prefix and the hour.
    let prefix = format!(
        "{}.",
        yagra_telemetry::file_prefix(SERVICE_NAME, Some(poller_id))
    );
    let collected = yagra_telemetry::collect_logs(dir, &prefix, since, max);

    // Scan every byte **before** a single chunk is built — the two-phase shape core's
    // `BundleBuilder` uses, and for the same reason: a detection must still be able to refuse the
    // whole answer rather than having already published half of it.
    for (name, bytes) in &collected.files {
        if let Some(rule) = scan.check(bytes) {
            tracing::error!(
                file = %name,
                rule = %rule,
                "support-log reply refused: this poller's own scan matched"
            );
            return refusal(
                req,
                poller_id,
                "this poller's own scan found a credential in its log; the rule and the file are in \
                 this poller's log, the value is nowhere",
            );
        }
    }

    let mut out = Vec::new();
    let mut seq = 0u32;
    for (name, bytes) in collected.files {
        for slice in bytes.chunks(CHUNK_BYTES) {
            out.push(PollerLogChunk {
                request_id: req.request_id,
                poller_id: poller_id.to_owned(),
                seq,
                last: false,
                name: name.clone(),
                bytes: encode_raw(slice),
                refused: None,
            });
            seq += 1;
        }
    }
    // Always a terminal message, even when there is nothing to send. Core stops waiting on `last`,
    // so an empty answer that ended silently would cost this poller the full deadline and then be
    // recorded as "did not answer" — which is a different, and wrong, statement about the site.
    out.push(PollerLogChunk {
        request_id: req.request_id,
        poller_id: poller_id.to_owned(),
        seq,
        last: true,
        name: String::new(),
        bytes: String::new(),
        refused: None,
    });
    out
}

/// The one-message answer that says "nothing, and here is the rule".
fn refusal(req: &PollerLogRequest, poller_id: &str, why: &str) -> Vec<PollerLogChunk> {
    vec![PollerLogChunk {
        request_id: req.request_id,
        poller_id: poller_id.to_owned(),
        seq: 0,
        last: true,
        name: String::new(),
        bytes: String::new(),
        refused: Some(why.to_owned()),
    }]
}

/// Publish every chunk in order. A failed publish abandons the rest: core's deadline covers a
/// half-delivered answer, and retrying into a bus that just refused would only delay that.
async fn send_all(bus: &dyn LogBus, chunks: Vec<PollerLogChunk>) {
    for chunk in chunks {
        if let Err(e) = bus.publish_poller_log_chunk(chunk).await {
            tracing::warn!(error = %e, "failed to publish a support-log chunk");
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "yagra-poller-support-logs-{}-{name}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn request() -> PollerLogRequest {
        PollerLogRequest {
            poller_id: "edge-1".to_owned(),
            request_id: Uuid::from_u128(9),
            since_unix_s: 0,
            max_bytes: 1024 * 1024,
        }
    }

    /// The reply carries the file under its own name, chunked, and always ends with a terminal
    /// marker. The name is what tells core which poller and which hour it is looking at, so a
    /// collector that renamed it would throw the answer away.
    #[test]
    fn a_log_file_is_chunked_and_the_reply_ends_with_a_terminal_marker() {
        let dir = scratch("present");
        std::fs::write(
            dir.join("yagra-poller-edge-1.2026-08-16-10.log"),
            vec![b'x'; CHUNK_BYTES + 10],
        )
        .unwrap();

        let out = build_reply(&request(), "edge-1", &dir, &LogScan::from_literals(vec![]));
        assert_eq!(out.len(), 3, "two data chunks plus the terminal marker");
        assert_eq!(out[0].name, "yagra-poller-edge-1.2026-08-16-10.log");
        assert_eq!(out[0].seq, 0);
        assert!(!out[0].last);
        assert_eq!(out[0].slice().unwrap().len(), CHUNK_BYTES);
        assert_eq!(out[1].seq, 1);
        assert_eq!(out[1].slice().unwrap().len(), 10);
        assert!(out[2].last);
        assert!(out[2].bytes.is_empty());
        assert!(out[2].refused.is_none());
    }

    /// An empty answer is still an answer. Ending silently would cost core the whole deadline and
    /// then be recorded as "this site did not respond" — a different and wrong statement.
    #[test]
    fn a_poller_with_no_log_still_answers() {
        let dir = scratch("empty");
        let out = build_reply(&request(), "edge-1", &dir, &LogScan::from_literals(vec![]));
        assert_eq!(out.len(), 1);
        assert!(out[0].last);
        assert!(out[0].refused.is_none(), "nothing to send is not a refusal");
    }

    /// Another poller's files are not swept up, even from the same directory — and the case that
    /// makes this more than bookkeeping: **`edge-1` is a prefix of `edge-10`**, so selecting on the
    /// bare instance prefix would make one site ship another site's log body. The separator is what
    /// keeps them apart, which is why the prefix is built from `file_prefix` plus a dot rather than
    /// by hand.
    #[test]
    fn only_this_pollers_own_files_are_collected() {
        let dir = scratch("prefix");
        for name in [
            "yagra-poller-edge-1.2026-08-16-10.log",
            "yagra-poller-edge-10.2026-08-16-10.log",
            "yagra-poller-edge-2.2026-08-16-10.log",
            "yagra-core.2026-08-16-10.log",
        ] {
            std::fs::write(dir.join(name), b"{}\n").unwrap();
        }
        let out = build_reply(&request(), "edge-1", &dir, &LogScan::from_literals(vec![]));
        let names: Vec<&str> = out
            .iter()
            .filter(|c| !c.last)
            .map(|c| c.name.as_str())
            .collect();
        assert_eq!(names, vec!["yagra-poller-edge-1.2026-08-16-10.log"]);
    }

    /// The security property of the increment: a device credential in the log refuses the **whole**
    /// reply, and the refusal names the rule and never the value.
    #[test]
    fn a_credential_in_the_log_refuses_the_whole_reply() {
        let dir = scratch("secret");
        std::fs::write(
            dir.join("yagra-poller-edge-1.2026-08-16-10.log"),
            b"{\"message\":\"walk failed\",\"community\":\"s3cret-community\"}\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("yagra-poller-edge-1.2026-08-16-09.log"),
            b"{\"message\":\"fine\"}\n",
        )
        .unwrap();

        let scan = LogScan::from_literals(vec!["s3cret-community".to_owned()]);
        let out = build_reply(&request(), "edge-1", &dir, &scan);
        assert_eq!(out.len(), 1, "not even the clean hour travels");
        assert!(out[0].last);
        let why = out[0].refused.as_deref().expect("a refusal is an answer");
        assert!(
            !why.contains("s3cret-community"),
            "the refusal is logged and then written into a bundle a human reads: {why}"
        );
        assert!(out[0].bytes.is_empty());
    }

    /// And the receiving half of the same guard: a scan whose every input is refused is
    /// indistinguishable from one that refuses nothing, so the accepting case is pinned too.
    #[test]
    fn a_log_holding_no_credential_is_accepted() {
        let dir = scratch("clean");
        std::fs::write(
            dir.join("yagra-poller-edge-1.2026-08-16-10.log"),
            b"{\"message\":\"snmp table walk failed\",\"node\":\"core-sw-1\"}\n",
        )
        .unwrap();
        let scan = LogScan::from_literals(vec!["s3cret-community".to_owned()]);
        let out = build_reply(&request(), "edge-1", &dir, &scan);
        assert_eq!(out.len(), 2, "one data chunk plus the terminal marker");
        assert!(out.iter().all(|c| c.refused.is_none()));
    }

    /// The length floor, and why it is not a nicety: a six-character lab community would forbid an
    /// ordinary English word and refuse every reply forever — at which point the feature gets
    /// switched off rather than fixed. Same floor, same reasoning, as core's `MIN_SECRET_LEN`.
    #[test]
    fn a_short_credential_is_not_enforced_as_a_literal() {
        let short = LogScan::from_literals(vec!["public".to_owned()]);
        assert!(short
            .check(b"{\"message\":\"a public interface went down\"}")
            .is_none());

        // …but a long one is, wherever it appears and in whatever shape.
        let long = LogScan::from_literals(vec!["private-community-1".to_owned()]);
        assert!(long
            .check(b"{\"detail\":\"tried private-community-1\"}")
            .is_some());
    }

    /// A PEM block is refused regardless of what this poller currently holds — the pattern half of
    /// the scan, which is the only rule left when the working set is all ICMP.
    #[test]
    fn the_pattern_rules_apply_with_no_literals_at_all() {
        let scan = LogScan::from_literals(vec![]);
        assert_eq!(
            scan.check(b"-----BEGIN PRIVATE KEY-----\nMIIE"),
            Some("a PEM private key block")
        );
        assert!(scan
            .check(b"{\"message\":\"the private key file is not mounted\"}")
            .is_none());
    }

    /// Newest first, so the hour being debugged is the one that survives the cap. The same ordering
    /// core's collector applies — a remote poller's contribution must not be selected by different
    /// rules than a co-located one's.
    #[test]
    fn files_are_newest_first_and_the_cap_keeps_the_newest() {
        let dir = scratch("order");
        for hour in ["10", "11", "12"] {
            std::fs::write(
                dir.join(format!("yagra-poller-edge-1.2026-08-16-{hour}.log")),
                vec![b'y'; 100],
            )
            .unwrap();
        }
        let mut req = request();
        req.max_bytes = 250;
        let out = build_reply(&req, "edge-1", &dir, &LogScan::from_literals(vec![]));
        let names: Vec<&str> = out
            .iter()
            .filter(|c| !c.last)
            .map(|c| c.name.as_str())
            .collect();
        assert_eq!(
            names,
            vec![
                "yagra-poller-edge-1.2026-08-16-12.log",
                "yagra-poller-edge-1.2026-08-16-11.log"
            ]
        );
    }

    /// A request addressed elsewhere is dropped rather than answered. The subject already routed it,
    /// so this can only be a mistake — and answering would put this site's log body on the wire for
    /// a request that was not its own.
    #[tokio::test]
    async fn a_request_addressed_to_another_poller_is_never_answered() {
        use yagra_bus::InMemoryBus;

        let dir = scratch("misaddressed");
        std::fs::write(
            dir.join("yagra-poller-edge-1.2026-08-16-10.log"),
            b"{\"message\":\"hi\"}\n",
        )
        .unwrap();

        let bus = Arc::new(InMemoryBus::new(8));
        let mut replies = bus.subscribe_poller_log_chunks();
        let mut wrong = request();
        wrong.poller_id = "edge-2".to_owned();

        run_log_request_loop(
            futures::stream::iter(vec![wrong]),
            "edge-1".to_owned(),
            dir,
            Arc::new(Mutex::new(WorkingSet::new())),
            bus.clone(),
        )
        .await;

        assert!(
            replies.try_recv().is_err(),
            "a misaddressed request must produce no reply at all"
        );
    }
}
