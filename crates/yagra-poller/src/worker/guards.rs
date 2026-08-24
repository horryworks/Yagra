// SPDX-License-Identifier: AGPL-3.0-only
//! The checks that read this module **as source text** (ADR-099).
//!
//! Two things the split established have no type, so reading the source is the only technique
//! available: that the dispatch delegates instead of talking to a device itself, and that a file
//! names only the check kinds it is the home of. Both are about *placement*, and placement is
//! exactly the kind of rule that decays quietly — the offending edit compiles, runs, and looks like
//! every other line around it.
//!
//! Declared `#[cfg(test)] mod guards;` in `mod.rs`, which is how [`crate::module_source`]'s
//! exclusion derives it: a guard file that sits inside the directory it greps otherwise matches its
//! own needles, and ADR-086 hit that within minutes of splitting.
//!
//! 🚨 **Neither check may cut the source for itself, and this module met the reason within an hour
//! of being written.** A first measurement of "which files name a `CheckSpec` variant" cut each file
//! at its first `#[cfg(test)]` and reported that `mod.rs` names **none** — because `mod.rs` declares
//! `#[cfg(test)] mod testkit;` above the dispatch, so everything below it vanished. The dispatch
//! names all twenty. That is ADR-091's defect met for the ninth time in this repository, and it is
//! why the text comes from `module_source` and never from a hand-rolled `.split()`.

use crate::module_source;

/// Which check kinds each file is allowed to name, and why it is the one that names them.
///
/// The shape ADR-094 and ADR-095 use for table ownership, which is the closest thing this repository
/// has to a placement rule that is *checked* rather than described. A hand-written table is
/// tolerable here for the reason `retention.rs::PRUNE_SITES` gives about itself: it falls the safe
/// way. Forgetting to declare a kind fails the build; declaring one nothing uses costs nobody
/// anything.
///
/// ⚠️ **The two entries below `mod.rs` are the interesting ones, and neither is incidental.**
/// `stream.rs` names `MerakiCollect` because that job fans out to many results and cannot go through
/// the one-job-one-result dispatch, and `Dns` because a DNS check against the system resolver
/// carries the display address `0.0.0.0` — per-device single-flight would let one such check starve
/// every other. A third kind that needs either treatment has to add itself here, which is the whole
/// point.
///
/// The four SNMP conversation files declare **nothing**, and that is a statement rather than an
/// omission: they work from arguments the dispatch already destructured, so a `CheckSpec` variant
/// appearing in one of them means a check is being re-decided somewhere it cannot be seen.
const SPEC_OWNERSHIP: &[(&str, &[&str])] = &[
    // The dispatch. It is the one place that turns a spec into a conversation, so it names them all.
    (
        "mod.rs",
        &[
            "Icmp",
            "Snmp",
            "SnmpV3",
            "SnmpTable",
            "SnmpV3Table",
            "SnmpOptical",
            "SnmpV3Optical",
            "SnmpMau",
            "SnmpV3Mau",
            "Http",
            "MerakiCollect",
            "Dns",
            "SnmpNeighbors",
            "SnmpV3Neighbors",
            "SnmpL3",
            "SnmpV3L3",
            "SnmpArp",
            "SnmpV3Arp",
            "SnmpRouting",
            "SnmpV3Routing",
        ],
    ),
    // The loop, and the two kinds whose *scheduling* differs — see the doc above.
    ("stream.rs", &["MerakiCollect", "Dns"]),
    // Destructures the job it was handed; one job, many results.
    ("meraki.rs", &["MerakiCollect"]),
    // Handed an already-destructured payload. Naming a kind here would mean deciding twice.
    ("probes.rs", &[]),
    ("snmp.rs", &[]),
    ("interfaces.rs", &[]),
    ("physical.rs", &[]),
    ("adjacency.rs", &[]),
];

/// Every variant of `CheckSpec`, read from the bus type that declares them.
///
/// Derived rather than listed: a second copy of this list is a second place to forget a kind, which
/// is the failure the table above exists to prevent. The enum lives in another crate, which
/// `module_source` handles the same way `yagra-core`'s `api/metrics.rs` reads this crate's worker.
fn check_kinds() -> Vec<String> {
    let roots = module_source::roots("../yagra-bus/src", "messages");
    let text = module_source::files_no_comments(&roots)
        .into_iter()
        .map(|(_, code)| code)
        .collect::<Vec<_>>()
        .join("\n");
    let head = format!("pub enum {} {{", "CheckSpec");
    let start = text
        .find(&head)
        .unwrap_or_else(|| panic!("`{head}` is not in yagra-bus's messages module any more"));
    let body = &text[start + head.len()..];
    let end = body
        .find("\n}")
        .expect("the enum's closing brace at column zero");
    body[..end]
        .lines()
        .filter_map(|l| {
            let t = l.trim();
            let name = t.split('(').next()?.trim();
            (!name.is_empty()
                && !t.starts_with('#')
                && name.chars().next().is_some_and(char::is_uppercase)
                && name.chars().all(|c| c.is_alphanumeric()))
            .then(|| name.to_owned())
        })
        .collect()
}

/// How many times `code` names `CheckSpec::<kind>` — as a whole variant, not as a prefix.
///
/// 🚨 **A plain substring search is wrong here and looked right.** `CheckSpec::Snmp` is a prefix of
/// `CheckSpec::SnmpTable`, `SnmpV3` of `SnmpV3Table`, and `SnmpMau` of nothing but only by luck —
/// eight of the twenty variants extend another. So a file permitted the short name could name the
/// long one for free, and the count that guards this check's own floor was inflated by every long
/// variant in the file. Found by breaking the check and reading the message it produced, which
/// named the wrong variant.
fn mentions(code: &str, kind: &str) -> usize {
    let needle = format!("{}::{kind}", "CheckSpec");
    code.match_indices(&needle)
        .filter(|(at, _)| {
            code[at + needle.len()..]
                .chars()
                .next()
                .is_none_or(|c| !c.is_alphanumeric() && c != '_')
        })
        .count()
}

/// **The dispatch delegates: no arm reaches for the device itself.**
///
/// This is what makes `mod.rs`'s table of conversations true rather than aspirational. It is not
/// tidiness — before ADR-099 the HTTP arm was 101 lines and the DNS arm 53, together 47% of
/// `execute`, and between them they owned nineteen of this module's tests while being reachable by
/// no function name. There was nowhere else to put the next line, so the next line went there.
///
/// 🚨 **The floor comes first and is the load-bearing half.** A body that came back empty — a
/// renamed function, a changed signature, a reader pointed at the wrong file — satisfies "no arm
/// touches the transport" perfectly, and this repository has shipped that shape eight times
/// (`rejection-only-tests-pass-when-everything-rejects`, read backwards).
#[test]
fn the_dispatch_delegates_and_touches_no_device() {
    let code = module_source::files_no_comments(&module_source::roots("src/worker", "mod"))
        .into_iter()
        .find(|(name, _)| name == "mod.rs")
        .map(|(_, code)| code)
        .expect("worker/mod.rs");
    let head = "pub async fn execute(";
    let start = code
        .find(head)
        .expect("`execute` is still the dispatch in worker/mod.rs");
    let body = &code[start..];
    let end = body
        .find("\n}")
        .expect("the dispatch's closing brace at column zero");
    let body = &body[..end];

    // The floor: the arms are really in the text this test read.
    let arms = body.matches("CheckSpec::").count();
    assert!(
        arms >= 20,
        "only {arms} arms were found in `execute`; the assertions below would pass over a body \
         this test never actually read"
    );

    // Assembled at runtime so this file's own prose cannot satisfy the search, the same discipline
    // every other source-reading check here uses.
    for reach in [format!("{}.", "transport"), format!("{}.", "walker")] {
        assert!(
            !body.contains(&reach),
            "`execute` calls `{reach}..` itself. Every arm delegates to the file named for how that \
             check talks to the device — that is what keeps the dispatch a dispatch, and it is how \
             the HTTP arm came to be 101 lines the last time nothing said so (ADR-099)"
        );
    }
}

/// **A file names only the check kinds it declares — and every file is in the table.**
///
/// Both directions, pinned to the directory rather than to a list, so a ninth conversation file
/// cannot ship without someone deciding what it is the home of (the shape
/// `filterSpecRegistry.test.ts` and `repo/guards.rs` both use).
///
/// 🚨 **Two floors, and they count what was checked rather than what was collected.** ADR-091's
/// guard was written the other way round and stayed green over zero files; that is the mistake this
/// assertion order exists to avoid.
#[test]
fn every_file_names_only_the_check_kinds_it_declares() {
    let kinds = check_kinds();
    assert!(
        kinds.len() >= 18,
        "only {} `CheckSpec` variants were derived from the bus; the ownership check below would \
         then be filtering against almost nothing",
        kinds.len()
    );

    let files = module_source::files_no_comments(&module_source::roots("src", "worker"));
    let declared: Vec<&str> = SPEC_OWNERSHIP.iter().map(|(f, _)| *f).collect();
    let on_disk: Vec<String> = files
        .iter()
        .map(|(name, _)| name.clone())
        .filter(|n| n != "testkit.rs")
        .collect();
    for name in &on_disk {
        assert!(
            declared.contains(&name.as_str()),
            "worker/{name} is not in SPEC_OWNERSHIP. Say which check kinds it is the home of — an \
             empty list is a real answer, and the four SNMP files give it"
        );
    }
    for name in &declared {
        assert!(
            on_disk.iter().any(|f| f == name) || *name == "guards.rs",
            "SPEC_OWNERSHIP names worker/{name}, which is not there any more"
        );
    }

    let mut checked = 0usize;
    for (name, code) in &files {
        if name == "testkit.rs" {
            continue;
        }
        let allowed = SPEC_OWNERSHIP
            .iter()
            .find(|(f, _)| f == name)
            .map(|(_, k)| *k)
            .unwrap_or(&[]);
        for kind in &kinds {
            let hits = mentions(code, kind);
            if hits == 0 {
                continue;
            }
            checked += hits;
            assert!(
                allowed.contains(&kind.as_str()),
                "worker/{name} names the check kind `{kind}` but does not declare it. A check kind belongs to \
                 the file that holds its conversation; naming one anywhere else means the same \
                 decision is being made in two places (ADR-099)"
            );
        }
    }
    assert!(
        checked >= 20,
        "only {checked} check-kind mentions were examined across worker/; the ownership assertion \
         above ran over almost nothing"
    );
}
