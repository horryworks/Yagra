// SPDX-License-Identifier: AGPL-3.0-only
//! **Yagra-base — the database layer under `yagra-core`** (ADR-202 Inc.5).
//!
//! What every part of core stands on: the PostgreSQL repositories (`repo`), the folder tree
//! (`groups`), sealed monitoring credentials (`secrets`, `sealed_row`), how an enum is stored
//! (`stored_enum`), the reserved seed ids (`seed_ids`), and the two process-wide signals a write
//! raises (`config_gen`, `change_feed`).
//!
//! **It names nothing above it, and the compiler holds that.** It used to be a folder in core,
//! where `repo/guards.rs` searched the text for an upward import; as its own crate it cannot
//! depend on `yagra-core`, so the import does not compile. A type a repository needs lives here
//! and its domain module imports it from here (`repo::adjacency_settings`, `repo::topology_mode`,
//! `groups::org_group_id`).
//!
//! ⚠️ **The checks that speak for the whole program live in core**, in
//! `yagra-core/src/program_guards.rs`, and read this crate's sources beside core's. A check that
//! reads only one crate quietly stops seeing the other — which is exactly what splitting a crate
//! does to a check written before the split. A check about one module of this crate stays here
//! and reads through this crate's own `module_source`.
//!
//! Behind the `test-util` feature: the real-PostgreSQL convention (`pgtest`), the table
//! vocabulary (`sql_tables`) and the test-only reads callers' tests use (`repo::MIGRATIONS` and
//! the ordered readers). Core enables it from dev-dependencies only.

pub mod change_feed;
pub mod config_gen;
pub mod groups;
#[cfg(test)]
mod module_source;
#[cfg(any(test, feature = "test-util"))]
pub mod pgtest;
pub mod repo;
pub mod sealed_row;
pub mod secrets;
pub mod seed_ids;
#[cfg(any(test, feature = "test-util"))]
pub mod sql_tables;
pub mod stored_enum;

/// What `token_enum!` expands to needs, reachable from a crate that does not name it.
#[doc(hidden)]
pub mod __private {
    pub use tracing;
}
