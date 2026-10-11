// SPDX-License-Identifier: AGPL-3.0-only
//! **Yagra-auth — who is asking** (ADR-202 Inc.6).
//!
//! Local accounts and in-memory sessions (`auth`), the signed session tokens a core pair shares
//! (`token`), long-lived API tokens (`apitokens`), the two external sign-ins (`oidc`, `ldap`), and
//! the audit log every mutating request writes (`audit`).
//!
//! It stands on `yagra-base` and names nothing in `yagra-core`; the compiler holds that. The
//! guard extractors that turn a session into a decision about one request stay in core's `api/`,
//! because they are about the HTTP surface rather than about identity.
//!
//! ⚠️ Checks about the whole program are `yagra-core/src/program_guards.rs`, which reads this
//! crate's tree too. A check about one module here reads through this crate's `module_source`.

pub mod apitokens;
pub mod audit;
pub mod auth;
pub mod ldap;
#[cfg(test)]
mod module_source;
pub mod oidc;
pub mod token;
