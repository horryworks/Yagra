// SPDX-License-Identifier: AGPL-3.0-only
//! The helpers this crate holds for the whole workspace have no second copy anywhere in it
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
}
