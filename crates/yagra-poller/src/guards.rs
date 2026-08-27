// SPDX-License-Identifier: AGPL-3.0-only
//! What ADR-103 bought, written as checks so the next person cannot spend it.
//!
//! Both read production text through [`crate::module_source`], never `include_str!` — the raw file
//! carries this module with it, and then a needle spelled here would match the line it is written on
//! (ADR-091/102). Both also carry floors, because the healthy answer to each is "found nothing",
//! which a reader that has stopped matching gives in exactly the same words.

use crate::module_source;

/// `main` starts loops. It must not *be* one.
///
/// The idiom for a loop that runs as long as the process does is one of two things — a stream
/// awaited, or an interval ticked — and before ADR-103 `main.rs` held four of them: the working-set
/// sync, the local scheduler, the heartbeat and the upgrade hand-off. What stays behind is allowed
/// to loop, and does: `drain_inflight` and `connect_bus` both spin, and both end on a budget. That
/// is why the needles are these two and not `loop {`.
///
/// 🚨 **The acceptance side is not decoration.** Without it, a reader that returned nothing would
/// satisfy the ban and this test would report a clean crate
/// (`rejection-only-tests-pass-when-everything-rejects`).
#[test]
fn main_runs_no_loop_of_its_own() {
    let stream_await = format!(".next{}.await", "()");
    let interval = format!("tokio::time::{}(", "interval");

    // Acceptance first: the three modules that took the loops must each still hold one.
    for module in ["heartbeat", "assignment", "upgrade"] {
        let code = module_source::code("src", module);
        assert!(
            code.lines().count() >= 40,
            "{module}.rs came back as {} lines; the reader has stopped finding it",
            code.lines().count()
        );
        assert!(
            code.contains(&stream_await) || code.contains(&interval),
            "{module}.rs holds none of the loops it was given — either it lost one, or the needles \
             below no longer match the idiom and the ban is vacuous"
        );
    }

    let main = module_source::code("src", "main");
    assert!(
        main.lines().count() >= 150,
        "main.rs came back as {} lines; nothing below is being checked",
        main.lines().count()
    );
    for needle in [&stream_await, &interval] {
        assert!(
            !main.contains(needle),
            "main.rs contains {needle:?}. A loop that runs for the life of the process belongs to \
             the subsystem it serves, and main starts subsystems — see ADR-103"
        );
    }
}

/// Every capability `yagra-bus` defines is one this poller answers for.
///
/// A capability it does not claim is work core withholds: an authenticated URL check is never sent,
/// forwarding reports the poller as unable to carry the original datagram, a support bundle names
/// the site as unrepresented. None of that is an error anywhere — the beat is well-formed, the logs
/// are quiet, and every test is green — which is why the vocabulary is **derived** from the one file
/// that defines it rather than listed here, exactly as `sql_tables` derives table names from
/// `migrations/`.
///
/// ⚠️ Claiming is not the same as claiming *unconditionally*: two of the eight are earned at
/// runtime, and `heartbeat.rs` naming them is all this check asks. Whether the condition is right is
/// a question no source scan can answer.
#[test]
fn every_capability_constant_is_claimed() {
    let defined = module_source::code("../yagra-bus/src", "messages");
    let vocabulary: Vec<String> = defined
        .lines()
        .filter_map(|l| l.trim().strip_prefix("pub const CAP_"))
        .filter_map(|rest| rest.split(':').next())
        .map(|name| format!("CAP_{name}"))
        .collect();
    assert!(
        vocabulary.len() >= 6,
        "only {} capability constants were derived from yagra-bus; the vocabulary has stopped \
         matching and the assertion below is vacuous: {vocabulary:?}",
        vocabulary.len()
    );

    let beat = module_source::code("src", "heartbeat");
    let unclaimed: Vec<&String> = vocabulary.iter().filter(|c| !beat.contains(*c)).collect();
    assert!(
        unclaimed.is_empty(),
        "{unclaimed:?} are defined by yagra-bus and named nowhere in heartbeat.rs. A capability \
         this poller does not claim is work core silently withholds from it — see ADR-103"
    );
}
