// SPDX-License-Identifier: AGPL-3.0-only
//! The `token_enum!` macro: one token list per fieldless enum that is both a DB column value and a
//! JSON tag.
//!
//! Here rather than inside `reports/`, where it was written, because it is no longer a reports
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

/// Parse an operator-supplied **filter** token against a stored enum's list, refusing the `Unknown`
/// fallback.
///
/// `Unknown` is the one variant nothing ever writes — it is what a token this build cannot read
/// degrades to on the way *in*. Accepting it on the way out would build `WHERE col = 'unknown'`,
/// which matches no row, so the operator would get a confident empty answer where they should have
/// got a 400. Every stored enum that reaches a query parameter has this same rule, which is why it
/// is here and not copied into each of them.
pub(crate) fn parse_filter_token<T: Copy + PartialEq>(
    all: &[T],
    unknown: T,
    token: impl Fn(T) -> &'static str,
    s: &str,
) -> Option<T> {
    all.iter()
        .copied()
        .find(|v| *v != unknown && token(*v) == s)
}

/// The tokens [`parse_filter_token`] accepts, for the 400 that names them.
pub(crate) fn filter_token_list<T: Copy + PartialEq>(
    all: &[T],
    unknown: T,
    token: impl Fn(T) -> &'static str,
) -> String {
    all.iter()
        .copied()
        .filter(|v| *v != unknown)
        .map(token)
        .collect::<Vec<_>>()
        .join(", ")
}
