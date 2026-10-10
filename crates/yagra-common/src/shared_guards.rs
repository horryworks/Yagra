// SPDX-License-Identifier: AGPL-3.0-only
//! The helpers this crate holds for the whole workspace (and the few shared helpers held by another
//! crate, named with their home below) have no second copy anywhere in it
//! (ADR-184). Test-only.
//!
//! One check over every crate rather than one per crate, because a copy is as likely to reappear
//! in a crate that has never had one — `yagra-transport` grew a private MAC parser beside
//! `yagra-oui`'s public one.

use std::path::Path;

use crate::srcread;

/// Each definition, and the one file (relative to `crates/`) allowed to hold it.
const ONE_HOME: &[(&str, &str)] = &[
    ("fn parse_mac(", "yagra-common/src/mac.rs"),
    ("fn render_mac(", "yagra-common/src/mac.rs"),
    ("fn tail_subids(", "yagra-common/src/oid.rs"),
    ("SNMP_TRAP_OID_0: &str =", "yagra-common/src/oid.rs"),
    ("SYS_UPTIME_0: &str =", "yagra-common/src/oid.rs"),
    ("tokens -= 1.0", "yagra-common/src/ratelimit.rs"),
    ("attempt += 1", "yagra-common/src/retry.rs"),
    ("let needed = if gap", "yagra-discovery/src/text.rs"),
];

/// Reading the wall clock as "time since 1970" — the one place allowed, and the two readings that
/// are not "now": a file's modification time, and a certificate's expiry, which is `None` rather
/// than `0` before 1970 because it is compared against the certificate's own date.
const EPOCH_READERS: &[&str] = &[
    "yagra-common/src/clock.rs",
    "yagra-core/src/relocation.rs",
    "yagra-transport/src/http.rs",
];

#[test]
fn every_shared_helper_is_defined_once_in_the_workspace() {
    let crates = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let mut files = Vec::new();
    for entry in std::fs::read_dir(&crates).expect("crates/ is readable") {
        let dir = entry.expect("a readable directory entry").path();
        let src = dir.join("src");
        if !src.is_dir() {
            continue;
        }
        let name = srcread::file_name(&dir);
        for (rel, code) in srcread::crate_files_no_comments(&src) {
            files.push((format!("{name}/src/{rel}"), code));
        }
    }
    assert!(
        files.len() >= 300,
        "only {} files read across the workspace",
        files.len()
    );
    let mut wrong = Vec::new();
    let mut homes_seen = 0;
    for (needle, home) in ONE_HOME {
        for (path, code) in &files {
            if !code.contains(needle) {
                continue;
            }
            if path == home {
                homes_seen += 1;
            } else {
                wrong.push(format!("{path}: {needle}"));
            }
        }
    }
    assert!(
        wrong.is_empty(),
        "{wrong:?} define again what `yagra_common` already holds — call it instead (ADR-184)"
    );
    assert_eq!(
        homes_seen,
        ONE_HOME.len(),
        "a definition moved out of the file named for it; the needle list is stale"
    );
    let epoch = [
        format!("duration_since({})", "UNIX_EPOCH"),
        format!("duration_since(std::time::{})", "UNIX_EPOCH"),
    ];
    let clocks: Vec<&str> = files
        .iter()
        .filter(|(path, code)| {
            !EPOCH_READERS.contains(&path.as_str())
                && epoch.iter().any(|n| code.contains(n.as_str()))
        })
        .map(|(path, _)| path.as_str())
        .collect();
    assert!(
        clocks.is_empty(),
        "{clocks:?} compute \"now\" by hand — call `yagra_common::clock` (ADR-184)"
    );
}
