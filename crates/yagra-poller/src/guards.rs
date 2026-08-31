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

/// Every gauge about the polling schedule is named in exactly one file.
///
/// `yagra_working_set_specs` was set from **two** — `assignment.rs` on every sync and
/// `heartbeat.rs` on every beat — and ADR-108 Inc.2 put `yagra_poll_demand_per_second` beside it.
/// Two sites for one fact is precisely how the second one gets forgotten, and the failure is silent
/// in the worst way: a gauge nobody sets reads **0**, which is a legitimate value for all three of
/// these. So the rule is `assignment.rs` and nowhere else, and this is the rule.
///
/// ⚠️ **They are not all published from the same function, and that is deliberate.** The first two
/// are one fact — what core handed this poller, and what that costs per second — so
/// `publish_working_set_gauges` sets them together on every sync. `yagra_poll_deferred_specs`
/// (ADR-109 Inc.4) is a different fact on a different clock: how much of what was due did not fit
/// in the last batch, which changes several times a tick. Setting it from the sync path would leave
/// it stale for a whole beat; setting the other two from the scheduler loop would recompute a
/// full-set sum for nothing. What the check enforces is the property that matters — one file, so a
/// reader can find every one of them by opening it.
///
/// 🚨 **The floor is the load-bearing half.** A walk that stopped finding this crate's sources
/// reports "each name is published once" over nothing at all, in the same words as a healthy crate
/// (`floor-must-count-what-was-checked`).
#[test]
fn the_polling_gauges_are_published_from_one_place() {
    let files = module_source::crate_files();
    assert!(
        files.len() >= 24,
        "only {} production files were walked; the walk has stopped finding this crate and the \
         assertions below are vacuous",
        files.len()
    );

    for metric in [
        "yagra_working_set_specs",
        "yagra_working_set_timers",
        "yagra_poll_demand_per_second",
        "yagra_poll_deferred_specs",
    ] {
        let needle = format!("\"{metric}\"");
        let sites: Vec<&str> = files
            .iter()
            .filter(|(_, code)| code.contains(&needle))
            .map(|(name, _)| name.as_str())
            .collect();
        assert_eq!(
            sites,
            ["assignment.rs"],
            "{metric} is named in {sites:?}. Every gauge about the polling schedule belongs to \
             `assignment.rs`, so that a reader can never see one of them refreshed and another \
             stale — see ADR-108 Increment 2 and ADR-109 Increment 4"
        );
    }
}

/// Every shipped file that quotes the concurrency default quotes the constant.
///
/// `YAGRA_MAX_CONCURRENT_POLLS`' default was a bare literal in `main.rs` and was written out again
/// in five shipped files, with nothing holding the six together — so the number an operator reads
/// and the number the binary uses were free to disagree, silently, forever. It was raised
/// 64 → 256 by ADR-109, which is exactly the moment a set like that drifts.
///
/// 🚨 **The floor counts files whose anchor was actually found, not files opened.** A prefix that
/// stops matching yields no number to compare, which is indistinguishable from agreement — so each
/// file must produce a reading, and the count of readings is asserted. That is the failure this
/// repo has already paid for twice (`floor-must-count-what-was-checked`).
///
/// ⚠️ **This covers every in-repo restatement and none of the four outside it.** The website says
/// the number too — `docs/features/monitoring.md` and `docs/reference/configuration.md`, EN and JA —
/// and lives in the `Yagra-Website` repository, which is not checked out here and cannot be reached
/// from a test. `/docs`' drift sweep is the only net over those, and this comment is the record
/// that it is a net with a hole.
#[test]
fn every_shipped_file_states_the_concurrency_default_the_binary_uses() {
    // (path relative to this crate, the text that anchors the declaration)
    const SITES: [(&str, &str); 5] = [
        ("../../DEPLOYMENT.md", "| `YAGRA_MAX_CONCURRENT_POLLS` | `"),
        (
            "../../DEPLOYMENT.ja.md",
            "| `YAGRA_MAX_CONCURRENT_POLLS` | `",
        ),
        (
            "../../docker-compose.poller.yml",
            "YAGRA_MAX_CONCURRENT_POLLS:-",
        ),
        (
            "../../docker-compose.deploy.yml",
            "YAGRA_MAX_CONCURRENT_POLLS:-",
        ),
        (
            "../../docker-compose.yml",
            "# YAGRA_MAX_CONCURRENT_POLLS: \"",
        ),
    ];

    let expected = crate::limiter::DEFAULT_MAX_CONCURRENT_POLLS;
    let mut read = 0usize;
    for (path, anchor) in SITES {
        let text = std::fs::read_to_string(path).unwrap_or_else(|e| panic!("read {path}: {e}"));
        let after = text.split_once(anchor).unwrap_or_else(|| {
            panic!(
                "{path} no longer contains {anchor:?}. Either the default stopped being stated \
                 there, or the anchor changed — and an anchor that matches nothing makes this \
                 check pass over that file without reading it"
            )
        });
        let digits: String = after.1.chars().take_while(char::is_ascii_digit).collect();
        let found: usize = digits
            .parse()
            .unwrap_or_else(|_| panic!("{path}: no number follows {anchor:?}, got {digits:?}"));
        assert_eq!(
            found, expected,
            "{path} says the concurrency default is {found}; the binary uses {expected} \
             (limiter::DEFAULT_MAX_CONCURRENT_POLLS). One of the two is a lie to an operator"
        );
        read += 1;
    }
    assert_eq!(
        read,
        SITES.len(),
        "only {read} of {} shipped files produced a reading",
        SITES.len()
    );
}
