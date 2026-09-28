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

/// Give a fieldless enum its token list once.
///
/// Two forms, and the difference is what an unrecognised token means:
///
/// - `token_enum!(T, [V => "v", …])` — **strict**: `ALL`, `as_str`, and `from_token` returning
///   `None`. For a token that arrives from outside (a query parameter, a request body) or whose
///   unknown value has no honest reading.
/// - `token_enum!(T, Fallback, "table.column", [V => "v", …])` — the above plus a **lenient**
///   `from_stored` that degrades to `Fallback` and says so in a `warn!`. For a stored column an
///   older core must still be able to list after a newer one wrote to it.
///
/// Before ADR-184 thirteen enums wrote their table out twice by hand — once in `as_str`, once in a
/// parser — which is two lists that agree only as long as someone edits both.
///
/// The tokens listed must match what `#[serde(rename_all = …)]` produces — the column and the JSON
/// tag are the same string, written by two different mechanisms, and nothing else makes them agree.
/// Each user pins that with a `token_and_serde_agree`-style test (`testing.md`).
macro_rules! token_enum {
    ($t:ty, [$($v:ident => $s:literal),+ $(,)?]) => {
        #[allow(
            dead_code,
            reason = "every enum gets the whole vocabulary; not every enum reads every part of it"
        )]
        impl $t {
            /// Every variant, so anything that must present all of them reads one list.
            pub const ALL: &'static [$t] = &[$(Self::$v),+];

            /// Every token, in the same order — for a message or a description that lists them.
            pub const TOKENS: &'static [&'static str] = &[$($s),+];

            /// Stable token — the DB column value and the JSON tag.
            #[must_use]
            pub const fn as_str(self) -> &'static str {
                match self { $(Self::$v => $s),+ }
            }

            /// The variant a token names, or `None` for any other text.
            #[must_use]
            pub fn from_token(s: &str) -> Option<Self> {
                Self::TOKENS
                    .iter()
                    .position(|t| *t == s)
                    .map(|i| Self::ALL[i])
            }
        }
    };
    ($t:ty, $unknown:ident, $col:literal, [$($v:ident => $s:literal),+ $(,)?]) => {
        $crate::stored_enum::token_enum!($t, [$($v => $s),+]);

        impl $t {

            /// Parse a stored token, degrading to `Unknown` rather than failing the read: a value
            /// this build does not recognise came from a newer core, and a row that cannot be
            /// listed at all is a worse answer than one whose state reads "unknown".
            #[must_use]
            pub fn from_stored(s: &str) -> Self {
                match Self::from_token(s) {
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

#[cfg(test)]
mod tests {
    /// Parsers that match literal tokens by hand, with why each is not a `token_enum!`.
    const HAND_WRITTEN: &[(&str, &str)] = &[
        (
            "events/mod.rs",
            "`EventStatGroup::parse` reads a query parameter one way; it has no stored token and no `as_str`",
        ),
        (
            "netbox.rs",
            "`NetboxField` has a `Custom(String)` variant, so it is not a fieldless enum",
        ),
        (
            "api/flow.rs",
            "`FlowAgg::parse` accepts two spellings of three kinds (`talkers` and `top-talkers`)",
        ),
        (
            "secrets.rs",
            "`parse` validates a stored JSON document; the matched strings are a field's values",
        ),
    ];

    /// ADR-184: a parser whose body is a `"token" =>` table is the second copy of an `as_str` —
    /// the macro writes both halves from one list. Needles are built at run time; the floor counts
    /// the macro's users, which only grows.
    #[test]
    fn no_enum_hand_writes_its_token_table() {
        let files = crate::module_source::crate_code();
        assert!(files.len() >= 150, "only {} files were read", files.len());
        let parser = regex::Regex::new(&format!(
            r"fn (?:{}|from_str|from_token|from_stored)\([^)]*\)[^{{]*\{{",
            "parse"
        ))
        .unwrap();
        let arm = regex::Regex::new(r#""[a-z0-9_]+" => "#).unwrap();
        let mut offenders = Vec::new();
        let mut uses = 0;
        for (name, code) in &files {
            uses += code.matches(&format!("{}!(", "token_enum")).count();
            if HAND_WRITTEN.iter().any(|(f, _)| f == name) || name == "stored_enum.rs" {
                continue;
            }
            for m in parser.find_iter(code) {
                // The body: from the opening brace to its match.
                let mut depth = 0usize;
                let mut end = code.len();
                for (i, ch) in code[m.end() - 1..].char_indices() {
                    match ch {
                        '{' => depth += 1,
                        '}' => {
                            depth -= 1;
                            if depth == 0 {
                                end = m.end() - 1 + i;
                                break;
                            }
                        }
                        _ => {}
                    }
                }
                if arm.is_match(&code[m.end()..end]) {
                    offenders.push(format!("{name}: {}", m.as_str()));
                }
            }
        }
        assert!(
            offenders.is_empty(),
            "{offenders:?} spell a token table by hand beside an `as_str` — declare the enum with \
             `token_enum!` so the two directions come from one list"
        );
        assert!(uses >= 22, "only {uses} `token_enum!` users found");
    }
}
