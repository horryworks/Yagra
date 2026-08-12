// SPDX-License-Identifier: AGPL-3.0-only
//! Audit log persistence (security.md: who changed what, when).
//!
//! Rows are written by the API middleware in [`crate::api`] — one per mutating request
//! (method + path + status) plus login events from the auth handler. This is the I/O
//! adapter only; what gets recorded is decided at the API layer. Append-only: there is no
//! update or delete path, and reads are admin-gated (`ViewAudit`).

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::{PgPool, Row};
use uuid::Uuid;

/// One audit row (API shape; `at` is RFC 3339 text at the edge).
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct AuditRow {
    pub id: Uuid,
    pub at: String,
    pub username: String,
    pub action: String,
    pub status: i32,
}

/// Default / maximum page sizes for the listing endpoint.
pub const DEFAULT_LIMIT: i64 = 100;
pub const MAX_LIMIT: i64 = 500;

// ⚠️ The two enums below derive `ToSchema` **and** `JsonSchema`, so their doc comments are published
// verbatim to REST clients (the OpenAPI document, and from it `web/src/api/schema.d.ts`) and to MCP
// clients (the tool's JSON schema). Keep them outward-facing; rationale goes in plain `//` comments
// like this one, which neither generator reads.
//
// Why they are enums rather than `Option<String>`: the same vocabulary has to exist in TypeScript,
// and a hand-written copy there would be a mirror with nothing pinning it (extensibility.md §2).
// Declared once here, the generated union reaches the WebUI and `types/api.ts::schemaEnumPins` holds
// the runtime `as const` array against it in both directions — the mirror is deleted, not tested.
//
// It is also what makes the `LIKE` safe. The value bound into the query is always one of the six
// compile-time prefixes below, never the caller's text, so a `%` or `_` in the request cannot reach
// the pattern. Taking a raw prefix would have been an injection path that `bind` does *not* close.

/// What an audit entry was, as the log's filterable vocabulary.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Deserialize, utoipa::ToSchema, schemars::JsonSchema,
)]
#[serde(rename_all = "lowercase")]
pub enum AuditAction {
    /// A resource was created.
    Post,
    /// A resource was replaced.
    Put,
    /// A resource was partially updated.
    Patch,
    /// A resource was deleted.
    Delete,
    /// A sign-in attempt, by any configured method — local, LDAP or OIDC.
    Login,
    /// An action taken through the MCP tool surface rather than the WebUI.
    Mcp,
}

// Note there is no `from_token`/`as_str` pair here, unlike most enums in this workspace. Parsing at
// the edge is **serde's**: the query field is typed, so an unknown token is rejected during
// extraction and the accepted values reach clients through the generated OpenAPI union rather than
// through a hand-written message. The iteration helpers the tests need live in `mod tests`, where
// nothing production reads them.
//
// ⚠️ Nothing above `mod tests` may spell the test-gate attribute, in code **or in a comment**.
// `production_source()` slices this file at the first occurrence of that literal, and it does so
// before it strips comments — so writing the attribute in prose up here silently truncates the
// haystack and the three source-scanning tests below start passing on nothing. That is the same
// self-matching-needle trap `reports.rs` documents from the other direction.
//
// The trade-off taken here, stated once: a typed query field means an invalid `action=` is refused
// by axum's `Query` rejection (plain text, naming the valid variants) rather than by the ADR-019
// error envelope. `?limit=abc` already behaved that way. The closed union in `schema.d.ts` is worth
// more than the envelope on a request the WebUI's own `<select>` cannot produce.
impl AuditAction {
    /// The literal prefix of the `action` column this variant selects.
    ///
    /// `audit_mw` writes `"{method} {path}"`, so a method is its name plus a space — the space is
    /// load-bearing, or `POST` would also match a hypothetical `POSTAL`.
    ///
    /// ⚠️ **`login` is a prefix, not an equality.** `api/session.rs` writes `auth.login` for a local
    /// sign-in but `auth.login.ldap`, `auth.login.ldap_unavailable` and `auth.login.ldap_conflict`
    /// for the directory paths, and says so: *"`auth.login` prefix, so an existing
    /// `LIKE 'auth.login%'` query still finds everything."* An equality here would silently exclude
    /// every LDAP and OIDC sign-in from a filter whose whole purpose is to find sign-ins.
    ///
    /// ⚠️ **`mcp` exists because `/mcp` does not pass through `audit_mw`.** A tool that acts records
    /// its own row as `mcp.<tool> key=value` (`mcp/tools.rs::record_audit`), so without this variant
    /// "what did the assistant do" is a question the log answers and the filter cannot ask.
    #[must_use]
    pub fn sql_prefix(self) -> &'static str {
        match self {
            Self::Post => "POST ",
            Self::Put => "PUT ",
            Self::Patch => "PATCH ",
            Self::Delete => "DELETE ",
            Self::Login => "auth.login",
            Self::Mcp => "mcp.",
        }
    }
}

/// How an audit entry's HTTP status turned out.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Deserialize, utoipa::ToSchema, schemars::JsonSchema,
)]
#[serde(rename_all = "lowercase")]
pub enum AuditStatusClass {
    /// The action succeeded (below 300).
    Ok,
    /// The action was refused — bad request, unauthorized, forbidden, not found (300–499).
    Client,
    /// The action failed inside Yagra (500 and above).
    Server,
}

impl AuditStatusClass {
    /// The inclusive `[min, max]` status bounds this class selects, `None` meaning unbounded.
    ///
    /// Bounds rather than a `CASE`, so the predicate is two ordinary comparisons a planner
    /// understands. The boundaries match the WebUI's badge colouring (`lib/format.ts::httpStatusTone`)
    /// — a 3xx reads as "refused" on both sides — but this is now the only place that decides it:
    /// the TypeScript bucketing was deleted when the filter moved into SQL, leaving `httpStatusTone`
    /// responsible for the colour alone.
    #[must_use]
    pub fn bounds(self) -> (Option<i32>, Option<i32>) {
        match self {
            Self::Ok => (None, Some(299)),
            Self::Client => (Some(300), Some(499)),
            Self::Server => (Some(500), None),
        }
    }
}

/// A validated audit-log query. Built at the API edge by `api::audit::audit_page`.
#[derive(Debug, Default, Clone)]
pub struct AuditFilter {
    /// Keyset cursor — rows strictly older than this. Distinct from `since`/`until`, which bound
    /// the window being searched rather than where this page starts.
    pub before: Option<DateTime<Utc>>,
    pub since: Option<DateTime<Utc>>,
    pub until: Option<DateTime<Utc>>,
    /// Free text over the username and the action, already trimmed and length-capped at the edge.
    pub q: Option<String>,
    pub action: Option<AuditAction>,
    pub status: Option<AuditStatusClass>,
    /// Rows per page; clamped to `1..=MAX_LIMIT` by [`AuditRepo::list`].
    pub limit: i64,
}

impl AuditFilter {
    /// The newest `limit` rows, unfiltered.
    ///
    /// For the readers that want a recent slice rather than an operator's query — the support
    /// bundle and the RCA evidence collector. Named rather than left to `..Default::default()` at
    /// each site so "no filter" has one spelling, and so adding a field later cannot quietly change
    /// what those two collect.
    #[must_use]
    pub fn newest(limit: i64) -> Self {
        Self {
            limit,
            ..Self::default()
        }
    }
}

/// The `WHERE` of a page of the audit log. Binds `$1..=$7` in the order [`bind_audit_filter`]
/// writes them; `$8` is the page size.
///
/// Every clause is **always present** with a `NULL` bind meaning "no filter", rather than being
/// appended when set. A conditionally-built predicate has a branch that can be forgotten, and here
/// forgetting one fails **open** — the same rule `NodeRepo::SCOPE_PREDICATE` and
/// [`crate::events::EVENT_FILTER_WHERE`] are written to.
///
/// ⚠️ The `$4` free-text alternation is parenthesised as a whole. `WHERE a AND b OR c AND d` parses
/// as `(a AND b) OR (c AND d)`, so an unwrapped `OR` here would let every row matching the username
/// escape the time range and the action filter.
///
/// ⚠️ `username` and `action` are **attacker-influenceable** — a failed sign-in records the submitted
/// username verbatim (`api/session.rs`) — so nothing in this string is ever formatted from request
/// input. The only interpolation any caller does is of this constant itself.
const AUDIT_FILTER_WHERE: &str = "($1::timestamptz IS NULL OR at < $1) \
     AND ($2::timestamptz IS NULL OR at >= $2) \
     AND ($3::timestamptz IS NULL OR at <= $3) \
     AND ($4::text IS NULL \
          OR (username ILIKE '%' || $4 || '%' OR action ILIKE '%' || $4 || '%')) \
     AND ($5::text IS NULL OR action LIKE $5 || '%') \
     AND ($6::int IS NULL OR status >= $6) \
     AND ($7::int IS NULL OR status <= $7)";

/// The one statement that reads the log. `at DESC` matches the `$1` cursor column for column;
/// `audit_log_at_idx` (migration 0012) serves both.
fn list_audit_sql() -> String {
    format!(
        "SELECT id, at, username, action, status FROM audit_log \
         WHERE {AUDIT_FILTER_WHERE} ORDER BY at DESC LIMIT $8"
    )
}

/// Bind `$1..=$7` of [`AUDIT_FILTER_WHERE`], in the one order that matches it.
///
/// One helper because the sequence is positional and silent when wrong: swapping the two status
/// bounds still compiles, still runs, and just answers a different question.
fn bind_audit_filter<'q>(
    q: sqlx::query::Query<'q, sqlx::Postgres, sqlx::postgres::PgArguments>,
    f: &'q AuditFilter,
) -> sqlx::query::Query<'q, sqlx::Postgres, sqlx::postgres::PgArguments> {
    let (min, max) = f.status.map_or((None, None), AuditStatusClass::bounds);
    q.bind(f.before)
        .bind(f.since)
        .bind(f.until)
        .bind(f.q.as_deref())
        .bind(f.action.map(AuditAction::sql_prefix))
        .bind(min)
        .bind(max)
}

/// PostgreSQL-backed append-only audit log.
pub struct AuditRepo {
    pool: PgPool,
}

impl AuditRepo {
    #[must_use]
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Append one entry. Failures are the caller's to log — auditing must never take the
    /// API down, so callers treat this as best-effort.
    pub async fn record(&self, username: &str, action: &str, status: u16) -> anyhow::Result<()> {
        sqlx::query("INSERT INTO audit_log (id, username, action, status) VALUES ($1, $2, $3, $4)")
            .bind(Uuid::new_v4())
            .bind(username)
            .bind(action)
            .bind(i32::from(status))
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// One newest-first page matching `filter`. `filter.limit` is clamped to `[1, MAX_LIMIT]`.
    ///
    /// The filter is applied **here**, not by the caller over a fetched page. That distinction is
    /// the whole point of this function: the WebUI used to narrow the loaded rows in TypeScript, so
    /// "last 30 days, DELETE only" examined the newest 100 entries and silently hid every older
    /// match — in a log whose purpose is completeness.
    pub async fn list(&self, filter: &AuditFilter) -> anyhow::Result<Vec<AuditRow>> {
        let limit = filter.limit.clamp(1, MAX_LIMIT);
        let sql = list_audit_sql();
        let rows = bind_audit_filter(sqlx::query(&sql), filter)
            .bind(limit)
            .fetch_all(&self.pool)
            .await?;
        rows.into_iter()
            .map(|row| {
                let at: DateTime<Utc> = row.try_get("at")?;
                Ok(AuditRow {
                    id: row.try_get("id")?,
                    at: at.to_rfc3339(),
                    username: row.try_get("username")?,
                    action: row.try_get("action")?,
                    status: row.try_get("status")?,
                })
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Iteration + token helpers, here rather than beside the enums because nothing production reads
    // them: serde owns the parsing at the edge. Keeping them in this module also keeps this the
    // file's first test gate, which is the split point `production_source()` slices on — see the
    // warning above the `AuditAction` impl.
    impl AuditAction {
        const ALL: [Self; 6] = [
            Self::Post,
            Self::Put,
            Self::Patch,
            Self::Delete,
            Self::Login,
            Self::Mcp,
        ];

        /// The token a client sends, matching the `#[serde(rename_all)]` on the enum.
        fn as_str(self) -> &'static str {
            match self {
                Self::Post => "post",
                Self::Put => "put",
                Self::Patch => "patch",
                Self::Delete => "delete",
                Self::Login => "login",
                Self::Mcp => "mcp",
            }
        }

        fn from_token(s: &str) -> Option<Self> {
            Self::ALL.into_iter().find(|a| a.as_str() == s)
        }
    }

    impl AuditStatusClass {
        const ALL: [Self; 3] = [Self::Ok, Self::Client, Self::Server];

        fn as_str(self) -> &'static str {
            match self {
                Self::Ok => "ok",
                Self::Client => "client",
                Self::Server => "server",
            }
        }

        fn from_token(s: &str) -> Option<Self> {
            Self::ALL.into_iter().find(|c| c.as_str() == s)
        }
    }

    const SRC: &str = include_str!("audit.rs");

    /// Executable code above the tests, comments stripped — see `dns_check.rs` for why both.
    fn production_source() -> String {
        SRC.split("#[cfg(test)]")
            .next()
            .expect("split always yields a first element")
            .lines()
            .filter(|l| !l.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn the_page_limit_is_clamped_in_both_directions() {
        // `?limit=` is operator-supplied on an admin-only screen, but an unbounded or zero/negative
        // value must still never reach the query.
        let clamp = |n: i64| n.clamp(1, MAX_LIMIT);
        assert_eq!(clamp(0), 1);
        assert_eq!(clamp(-5), 1);
        assert_eq!(clamp(50), 50);
        assert_eq!(clamp(i64::MAX), MAX_LIMIT);
        const {
            assert!(
                DEFAULT_LIMIT <= MAX_LIMIT,
                "the default must fit inside the cap"
            );
        };
    }

    #[test]
    fn audit_paging_is_keyset_and_never_offset() {
        // The audit log is append-only and read newest-first, so OFFSET would shift rows under a
        // reader the moment anything is audited mid-page — in a log whose purpose is completeness.
        let sql = list_audit_sql();
        assert!(sql.contains("($1::timestamptz IS NULL OR at < $1)"));
        assert!(sql.contains("ORDER BY at DESC LIMIT $8"));
        assert!(
            !sql.contains("OFFSET"),
            "OFFSET paging reintroduced — entries shift under the reader as actions are audited"
        );
    }

    // `both_cursor_shapes_select_the_same_columns_in_the_same_order` was deleted here, deliberately.
    // It existed because a cursor page and a first page were two separate prepared statements
    // feeding one positional row reader, so their SELECT lists could drift. There is now one
    // statement with the cursor as a nullable bind, so the thing it guarded cannot happen. Deleting
    // it beat updating it: a test that can no longer fail is worse than no test, because it reads
    // like coverage.

    #[test]
    fn the_filter_is_always_present_rather_than_appended_when_set() {
        // The fail-open shape: a conditionally-built WHERE has a branch someone can forget, and the
        // branch that gets forgotten returns *more* rows, not fewer. Every clause is a nullable bind.
        let src = production_source();
        for builder in ["push_str(", "if let Some", "unwrap_or_default()"] {
            assert!(
                !AUDIT_FILTER_WHERE.contains(builder),
                "the predicate looks conditionally built ({builder})"
            );
        }
        assert_eq!(
            AUDIT_FILTER_WHERE.matches(" IS NULL").count(),
            7,
            "every one of the seven binds must carry its own 'no filter' arm"
        );
        assert!(src.contains("const AUDIT_FILTER_WHERE"));
        // The slice must reach the *end* of the production half, not just its start. Without this,
        // anything that moves the split point earlier turns the three source-scanning tests in this
        // module into tests of an empty string — which is worse than not having them.
        assert!(
            src.contains("pub async fn list("),
            "production_source() stopped before the end of the file — something above `mod tests` \
             spells the test-gate attribute"
        );
    }

    #[test]
    fn the_audit_filter_binds_every_placeholder_it_names() {
        // Positional and silent when wrong. Count the distinct `$n` the predicate names and the
        // binds the helper writes; a mismatch is a query that runs and answers a different question.
        let named: std::collections::BTreeSet<&str> = (1..=9)
            .map(|n| match n {
                1 => "$1",
                2 => "$2",
                3 => "$3",
                4 => "$4",
                5 => "$5",
                6 => "$6",
                7 => "$7",
                8 => "$8",
                _ => "$9",
            })
            .filter(|p| AUDIT_FILTER_WHERE.contains(p))
            .collect();
        assert_eq!(named.len(), 7, "the predicate should name exactly $1..=$7");
        // Slice between two lines of *code*: `production_source()` strips comments, so a doc-comment
        // marker would not survive and the window would run to the end of the file — silently
        // counting `record`'s binds and `list`'s page size too.
        let binds = production_source()
            .split("fn bind_audit_filter")
            .nth(1)
            .expect("the bind helper")
            .split("pub struct AuditRepo")
            .next()
            .expect("split always yields a first element")
            .matches(".bind(")
            .count();
        assert_eq!(
            binds,
            named.len(),
            "bind_audit_filter must write one value per placeholder"
        );
        // …and the page size is bound separately, by `list`, as $8.
        assert!(list_audit_sql().contains("LIMIT $8"));
    }

    #[test]
    fn the_free_text_alternation_cannot_swallow_the_clauses_after_it() {
        // `a AND b OR c AND d` parses as `(a AND b) OR (c AND d)`. An unwrapped OR here would let
        // every row matching the username escape the time range and the action filter — the same
        // parenthesisation trap `AlertHistoryRepo::SCOPE_PREDICATE` documents.
        let clause = AUDIT_FILTER_WHERE
            .split("AND ($4::text IS NULL")
            .nth(1)
            .expect("the free-text clause");
        let alternation = clause.split("AND ($5").next().expect("first element");
        assert!(
            alternation.contains("OR (username ILIKE") && alternation.contains("action ILIKE"),
            "the two ILIKE arms must sit inside their own parentheses: {alternation}"
        );
    }

    #[test]
    fn every_statement_binds_its_values_instead_of_interpolating_them() {
        // `username` and `action` are attacker-influenceable — a failed login records the submitted
        // username verbatim — and they are now searchable, which is exactly when this stops being
        // theoretical. The predicate is a `const` with no `{}` in it, the only `format!` interpolates
        // that const, and every request value reaches SQL through `.bind(`.
        let src = production_source();
        assert!(
            !AUDIT_FILTER_WHERE.contains('{'),
            "the predicate must contain no format placeholder — it is not a template"
        );
        assert_eq!(
            src.matches("format!(").count(),
            1,
            "exactly one format!, and it interpolates the constant predicate"
        );
        assert!(src.contains("WHERE {AUDIT_FILTER_WHERE} ORDER BY"));
        for builder in ["push_str(", "QueryBuilder"] {
            assert!(
                !src.contains(builder),
                "SQL may be being built by string concatenation ({builder}); bind the value instead"
            );
        }
    }

    #[test]
    fn every_status_class_partitions_the_status_space() {
        // Disjoint and covering, so "ok + client + server" is the same set as "no status filter".
        // This is what catches someone turning `< 500` into `<= 500` while tidying.
        let holds = |c: AuditStatusClass, s: i32| {
            let (min, max) = c.bounds();
            min.is_none_or(|m| s >= m) && max.is_none_or(|m| s <= m)
        };
        for status in 100..=599 {
            let matched: Vec<_> = AuditStatusClass::ALL
                .into_iter()
                .filter(|c| holds(*c, status))
                .collect();
            assert_eq!(
                matched.len(),
                1,
                "status {status} matched {} classes, want exactly 1",
                matched.len()
            );
        }
        assert!(holds(AuditStatusClass::Ok, 200));
        assert!(holds(AuditStatusClass::Client, 403));
        assert!(holds(AuditStatusClass::Server, 500));
    }

    #[test]
    fn every_action_filter_names_a_shape_the_writers_produce() {
        // `LIKE prefix || '%'` is `starts_with` — so this table is the query, run in Rust. The rows
        // are real strings the writers emit: `api/mod.rs::audit_mw` formats "{method} {path}",
        // `api/session.rs` writes the `auth.login*` family, and `mcp/tools.rs::record_audit` writes
        // "mcp.<tool> key=value".
        let cases: &[(&str, Option<AuditAction>)] = &[
            ("POST /api/v1/nodes", Some(AuditAction::Post)),
            ("PUT /api/v1/users/1", Some(AuditAction::Put)),
            ("PATCH /api/v1/nodes/1", Some(AuditAction::Patch)),
            ("DELETE /api/v1/mutes/1", Some(AuditAction::Delete)),
            ("auth.login", Some(AuditAction::Login)),
            // The three the WebUI's `=== 'auth.login'` equality used to miss entirely.
            ("auth.login.ldap", Some(AuditAction::Login)),
            ("auth.login.ldap_unavailable", Some(AuditAction::Login)),
            ("auth.login.ldap_conflict", Some(AuditAction::Login)),
            ("mcp.ack_alert node=abc", Some(AuditAction::Mcp)),
            ("mcp.run_analysis tool=flap", Some(AuditAction::Mcp)),
            // Nothing else claims these.
            ("auth.logout", None),
            ("GET /api/v1/nodes", None),
        ];
        for (action, want) in cases {
            let got: Vec<_> = AuditAction::ALL
                .into_iter()
                .filter(|a| action.starts_with(a.sql_prefix()))
                .collect();
            match want {
                Some(w) => assert_eq!(got, vec![*w], "{action}"),
                None => assert!(got.is_empty(), "{action} matched {got:?}, want nothing"),
            }
        }
    }

    #[test]
    fn a_method_prefix_keeps_its_separating_space() {
        // Without the space, `POST` would also select a path-less action beginning with those
        // letters. Cheap to assert, and the kind of thing a "tidy up the strings" pass removes.
        for action in [
            AuditAction::Post,
            AuditAction::Put,
            AuditAction::Patch,
            AuditAction::Delete,
        ] {
            assert!(
                action.sql_prefix().ends_with(' '),
                "{:?} must end with a space",
                action
            );
        }
    }

    #[test]
    fn every_variant_round_trips_through_its_token_and_through_serde() {
        // The token and the serde tag are produced by two different mechanisms — `as_str()` and
        // `#[serde(rename_all)]` — and nothing makes them agree, so a disagreement means a value the
        // WebUI sends is a value this parser rejects.
        for a in AuditAction::ALL {
            assert_eq!(AuditAction::from_token(a.as_str()), Some(a));
            let json = format!("\"{}\"", a.as_str());
            assert_eq!(
                serde_json::from_str::<AuditAction>(&json).expect("serde accepts its own token"),
                a
            );
        }
        for c in AuditStatusClass::ALL {
            assert_eq!(AuditStatusClass::from_token(c.as_str()), Some(c));
            let json = format!("\"{}\"", c.as_str());
            assert_eq!(
                serde_json::from_str::<AuditStatusClass>(&json)
                    .expect("serde accepts its own token"),
                c
            );
        }
        assert_eq!(
            AuditAction::from_token("POST"),
            None,
            "tokens are lowercase"
        );
        assert_eq!(AuditAction::from_token(""), None);
        assert_eq!(AuditStatusClass::from_token("2xx"), None);
    }
}
