// SPDX-License-Identifier: AGPL-3.0-only
//! The `token_enum!` macro: one token list per fieldless enum that is both a DB column value and a
//! JSON tag.
//!
//! Here rather than inside `reports.rs`, where it was written, because it is no longer a reports
//! concern: [`crate::cadence::Cadence`] and the analysis-schedule status use it too, and a macro
//! shared across modules that lives inside one of them is the migration tripwire
//! `api-conventions.md` describes for helpers.
//!
//! Named for what it holds — enums whose variants are *stored* tokens — and deliberately not
//! `tokens`, which would sit one line from `token.rs` (signed session tokens) and mean something
//! entirely different.

/// Give a fieldless enum its token list once: `ALL`, `as_str`, and a lenient `from_stored`.
///
/// The tokens listed must match what `#[serde(rename_all = …)]` produces — the column and the JSON
/// tag are the same string, written by two different mechanisms, and nothing else makes them agree.
/// Each user pins that with a `token_and_serde_agree`-style test (`testing.md`).
macro_rules! token_enum {
    ($t:ty, $unknown:ident, $col:literal, [$($v:ident => $s:literal),+ $(,)?]) => {
        impl $t {
            /// Every variant, so anything that must present all of them reads one list.
            pub const ALL: &'static [$t] = &[$(Self::$v),+];

            /// Stable token — the DB column value and the JSON tag.
            #[must_use]
            pub const fn as_str(self) -> &'static str {
                match self { $(Self::$v => $s),+ }
            }

            /// Parse a stored token, degrading to `Unknown` rather than failing the read: a value
            /// this build does not recognise came from a newer core, and a row that cannot be
            /// listed at all is a worse answer than one whose state reads "unknown".
            #[must_use]
            pub fn from_stored(s: &str) -> Self {
                match Self::ALL.iter().copied().find(|v| v.as_str() == s) {
                    Some(v) => v,
                    None => {
                        tracing::warn!(
                            token = %s, column = $col,
                            "unrecognised token; a newer core wrote this row"
                        );
                        Self::$unknown
                    }
                }
            }
        }
    };
}

pub(crate) use token_enum;
