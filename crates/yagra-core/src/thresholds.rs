// SPDX-License-Identifier: AGPL-3.0-only
//! Threshold persistence (Workstream #1).
//!
//! Stores scope-based threshold rules; the alert engine resolves them per (node, metric)
//! via [`yagra_common::resolve_effective`] (most-specific-wins, most-restrictive tie-break,
//! ADR-013). This is the I/O adapter; the resolution + evaluation logic is in `yagra-common`
//! (tested there) and the firing logic in [`crate::alerts`].

use serde::Serialize;
use sqlx::{PgPool, Row};
use uuid::Uuid;
use yagra_common::{Direction, ScopeLevel, ThresholdBounds, ThresholdRule};

/// A stored threshold rule with its scope and id (id is for the API; the engine ignores it).
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct StoredThreshold {
    pub id: Uuid,
    // Serialized as `scope_level` so the GET response matches the POST body field name.
    #[serde(rename = "scope_level")]
    pub level: ScopeLevel,
    /// Every profile, folder group, node or port this rule applies to — `scope_level` says which
    /// of those they are. Empty for a `global` rule, which applies to every node.
    //
    // ⚠️ Replaces the single `scope_id` this used to carry, in the API response as well (ADR-078).
    // The database COLUMN of that name is still written (see `ThresholdWrite::primary`) because a
    // core predating migration 0096 resolves by it — but it is deliberately not a second field
    // here. Two spellings of "which target" is the mirror `extensibility.md` §2 forbids, and the
    // copy that would rot is the one a future resolver reaches for by habit: on a rule naming
    // four profiles it answers about one of them, quietly covering a quarter of the fleet.
    //
    // (The two notes above are `//` on purpose: this type derives `ToSchema`, so a `///` line is
    // published verbatim to every API client. The line that IS `///` is written for them.)
    pub scope_ids: Vec<String>,
    /// Which way this rule’s `warning`/`critical` face. Superseded by the four bounds on the rule
    /// itself, which describe both sides; on a rule bounding both, this names the **primary side
    /// only** and describes half of what the rule does.
    //
    // ⚠️ These three are on `StoredThreshold` rather than on `ThresholdRule`, where they used to
    // be fields, because ADR-081 made them *derived*: the four bounds are the truth and these are
    // a reading of them. Keeping them as fields would let a literal set the two to different
    // things — the divergence `extensibility.md` §2 is about, and the one that made a rule get
    // stored facing one way while its bounds faced the other. They are written out here so the
    // published JSON keeps every key it had; dropping them would have been a breaking change to
    // every client, which is the opposite of what this ADR's N-1 story promises.
    pub direction: yagra_common::Direction,
    /// The primary side’s warning bound. See `direction`.
    pub warning: Option<f64>,
    /// The primary side’s critical bound. See `direction`.
    pub critical: Option<f64>,
    #[serde(flatten)]
    pub rule: ThresholdRule,
}

impl StoredThreshold {
    /// Build a row, deriving the legacy triple from the rule's bounds.
    ///
    /// The only way to make one, so `direction`/`warning`/`critical` cannot be set to something the
    /// bounds do not say. A struct literal could, and a row describing a rule the engine does not
    /// run is precisely the failure ADR-081 is about.
    #[must_use]
    pub fn new(id: Uuid, level: ScopeLevel, scope_ids: Vec<String>, rule: ThresholdRule) -> Self {
        let bounds = rule.bounds();
        Self {
            id,
            level,
            scope_ids,
            direction: bounds.direction(),
            warning: bounds.warning(),
            critical: bounds.critical(),
            rule,
        }
    }
}

/// The fields one save writes — the body of both [`ThresholdStore::create`] and
/// [`ThresholdStore::update`].
///
/// A struct rather than the eight positional arguments those two used to take. `scope_ids` would
/// have been the ninth, and both already carried `#[allow(clippy::too_many_arguments)]` — which is
/// the design signal `coding-conventions.md` describes rather than a lint to silence. Two writers
/// of one row is also how this repo has shipped a rule enforced on only one of them
/// (`extensibility.md` §3), so they now share a shape as well as a validator.
#[derive(Debug, Clone, Copy)]
pub struct ThresholdWrite<'a> {
    pub scope_level: &'a str,
    /// Every target, already validated by the API edge. Empty for a `global` rule.
    pub scope_ids: &'a [String],
    pub metric: &'a str,
    /// The four bounds — the truth this row is written from (ADR-081).
    ///
    /// ⚠️ `direction`, `warning` and `critical` are **not** fields here. They are still written to
    /// their columns, but derived from these bounds at the moment of the write rather than passed
    /// in beside them: two carriers of one fact is how a row gets stored saying `above` while its
    /// bounds face down, and nothing downstream could tell which half to believe.
    pub bounds: ThresholdBounds,
    pub dwell_samples: i32,
}

impl ThresholdWrite<'_> {
    /// What goes in the legacy `scope_id` column: the first target, or the empty string.
    ///
    /// ⚠️ Written on every save even though nothing in this build reads it back for resolution.
    /// It is what a core predating migration 0096 resolves the rule by, so leaving it empty would
    /// turn a rollback into "the rule matches nothing" instead of "the rule matches its first
    /// target" — inert versus narrowed, and only one of those is safe to discover late.
    fn primary(&self) -> &str {
        self.scope_ids.first().map_or("", String::as_str)
    }

    /// What goes in the legacy `direction`/`warning`/`critical` columns: the rule's primary side.
    ///
    /// ⚠️ Written on every save even though this build resolves from the four bounds. It is what a
    /// core predating migration 0098 evaluates the rule by, so leaving them null would turn a
    /// rollback into "the rule never fires" instead of "the rule watches one side of the band" —
    /// inert versus narrowed, and only one of those is safe to discover late.
    const fn legacy(&self) -> (&'static str, Option<f64>, Option<f64>) {
        (
            self.bounds.direction().as_str(),
            self.bounds.warning(),
            self.bounds.critical(),
        )
    }
}

/// Read the stored token back into the enum.
///
/// ⚠️ Goes through [`ScopeLevel::from_token`] rather than a hand-written `match` with a `_` arm.
/// The wildcard this replaces mapped anything unrecognised to `Profile`, which was harmless
/// while the vocabulary was closed and became a trap the moment ADR-075 added `global`: a global
/// rule read as a profile rule with an empty `scope_id` matches no node and goes silently inert.
/// An unknown token still falls back to `Profile` — a column value nothing writes — but the fall
/// back is now a single named default instead of a match arm that swallows new variants.
fn parse_level(s: &str) -> ScopeLevel {
    ScopeLevel::from_token(s).unwrap_or(ScopeLevel::Profile)
}

/// The four bounds of one row, falling back to the pre-ADR-081 triple when they are all null.
///
/// ⚠️ The fallback is not belt-and-braces, it is the **rollback path**. Migration 0098 backfilled
/// every row that existed when it ran, so four nulls beside a populated `warning`/`critical` can
/// only be a row an OLDER core wrote afterwards — during a rollback window, when that core knew
/// nothing about these columns. Reading it as "no bounds" would make the rule stored, listed and
/// silently inert, which is exactly the failure ADR-081 exists to remove; reading it through
/// `from_legacy` is what the older core itself meant by the row. Same reasoning as `scope_ids`
/// above, and the same shape.
fn read_bounds(row: &sqlx::postgres::PgRow) -> anyhow::Result<ThresholdBounds> {
    let bounds = ThresholdBounds {
        warning_below: row.try_get("warning_below")?,
        critical_below: row.try_get("critical_below")?,
        warning_above: row.try_get("warning_above")?,
        critical_above: row.try_get("critical_above")?,
    };
    if !bounds.is_empty() {
        return Ok(bounds);
    }
    Ok(ThresholdBounds::from_legacy(
        parse_direction(&row.try_get::<String, _>("direction")?),
        row.try_get("warning")?,
        row.try_get("critical")?,
    ))
}

/// Read the stored direction token back into the enum.
///
/// ⚠️ Goes through [`Direction::from_token`] for the same reason [`parse_level`] does, and it took
/// until ADR-081 to notice that the lesson written five lines above had not been applied five lines
/// below. The `_ => Direction::Above` this replaces mapped **every** unrecognised token to `above`,
/// including a mis-cased `Below` — and the config-bundle import path binds this column as raw text
/// with no CHECK constraint behind it, so `snmp_up below 0.5` arriving as `"Below"` became
/// `above 0.5` and paged for every healthy device. `above` is still the fallback (it is what the
/// column's writers have always produced), but it is now one named default rather than a match arm
/// that swallows whatever it is handed.
fn parse_direction(s: &str) -> Direction {
    Direction::from_token(s).unwrap_or(Direction::Above)
}

/// What narrows a page of the ruleset. Every field is optional; all of them are ANDed.
///
/// A struct rather than three parameters because the order of three `Option`s is exactly the kind
/// of call-site mistake that compiles, runs, and answers a different question.
#[derive(Debug, Default, Clone, Copy)]
pub struct ThresholdFilter<'a> {
    /// Case-insensitive substring of the metric name.
    pub metric: Option<&'a str>,
    /// Any of these scope levels. Empty means unfiltered — never an empty array in SQL, which
    /// `= ANY(…)` would match nothing against.
    pub level: &'a [ScopeLevel],
    /// Any of these directions. Empty means unfiltered.
    pub direction: &'a [Direction],
}

/// PostgreSQL-backed threshold store.
pub struct ThresholdStore {
    pool: PgPool,
}

impl ThresholdStore {
    #[must_use]
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Columns every read below selects, in the order [`Self::row_to_threshold`] expects.
    const COLUMNS: &'static str = "id, scope_level, scope_id, scope_ids, metric, direction, \
         warning, critical, warning_below, critical_below, warning_above, critical_above, \
         dwell_samples";

    /// **Every** threshold rule — the alert engine snapshots these to evaluate against.
    ///
    /// Deliberately uncapped, and must stay so: a truncated snapshot is not a shorter list, it is
    /// an alert engine that silently stops evaluating some rules. The *API* read is capped
    /// separately ([`Self::list_page`]) because a browser rendering the whole fleet's node-level
    /// overrides is a different problem from the engine needing all of them.
    pub async fn list_all(&self) -> anyhow::Result<Vec<StoredThreshold>> {
        let rows = sqlx::query(&format!("SELECT {} FROM thresholds", Self::COLUMNS))
            .fetch_all(&self.pool)
            .await?;
        rows.into_iter().map(Self::row_to_threshold).collect()
    }

    /// The filter's predicate: one const, every clause always present, every value a nullable bind.
    ///
    /// Not assembled conditionally, and that is the point — a `WHERE` built by pushing clauses has
    /// a branch per filter that can be forgotten, and a forgotten clause on *this* table fails
    /// open: the operator sees more rules than they asked for, which reads as a wrong answer about
    /// what pages the fleet. One const also keeps the count query and the page query asking the
    /// same question, so "500 of 3,200" cannot count a different set from the one it is showing.
    /// ⚠️ The direction clause asks about the **bounds**, not only the legacy `direction` column
    /// (ADR-081). A rule bounding both sides stores `above` there — it is the primary side — so
    /// `direction=below` used to miss it, and the caller who asked "show me the rules that fire
    /// when a value drops" would be shown a shorter list than the truth. That is the worst answer
    /// available for an alerting filter: complete-looking and short.
    ///
    /// The legacy column is still ORed in, which makes this strictly a **widening**: every row
    /// that matched before still matches. It has to be — a row an N-1 core wrote during a rollback
    /// window has all four bounds null, and so does a liveness rule, whose direction is a fact
    /// about the check rather than about a number.
    const FILTER_WHERE: &'static str = "($1::text IS NULL OR metric ILIKE '%' || $1 || '%') \
         AND ($2::text[] IS NULL OR scope_level = ANY($2)) \
         AND ($3::text[] IS NULL OR direction = ANY($3) \
              OR ('above' = ANY($3) \
                  AND (warning_above IS NOT NULL OR critical_above IS NOT NULL)) \
              OR ('below' = ANY($3) \
                  AND (warning_below IS NOT NULL OR critical_below IS NOT NULL)))";

    /// `CASE scope_level WHEN … END` ranking the levels broadest-first, **built from
    /// [`ScopeLevel::ALL`]** rather than written out.
    ///
    /// That order is also the precedence `resolve_effective` applies, so a hand-written copy is a
    /// second place the precedence lives — and the copy that rots is this one, because getting it
    /// wrong only changes the order of a table nobody diffs. Interpolating into SQL is safe here
    /// and only here: every fragment comes from `as_str()`, which is a `const fn` over the enum.
    fn scope_level_rank() -> String {
        let arms: String = ScopeLevel::ALL
            .iter()
            .enumerate()
            .map(|(rank, level)| format!("WHEN '{}' THEN {rank} ", level.as_str()))
            .collect();
        // The `ELSE` catches a level written by a *newer* core after a rollback: sort it last
        // rather than dropping the row out of the page.
        format!("CASE scope_level {arms}ELSE {} END", ScopeLevel::ALL.len())
    }

    /// One page of threshold rules for the API, plus how many matched, so the caller can tell the
    /// operator how many were withheld rather than silently showing a prefix.
    ///
    /// Ordered so the page is stable and readable: broadest scope first (global → profile →
    /// group → node),
    /// then by metric. Without an `ORDER BY`, PostgreSQL is free to return a different arbitrary
    /// subset each time the same page is fetched.
    ///
    /// ⚠️ The total counts the rows **matching the filter**, not every row in the table. That is
    /// what makes `truncated` mean something once a filter exists: "showing 500 of 3,200" has to be
    /// 3,200 matches, or the operator cannot tell whether narrowing further would help.
    pub async fn list_page(
        &self,
        limit: i64,
        filter: &ThresholdFilter<'_>,
    ) -> anyhow::Result<(Vec<StoredThreshold>, i64)> {
        // `query` rather than `query_scalar` so both statements bind through the same helper —
        // two binder shapes would be two places to get the positional order wrong.
        let total: i64 = Self::bind_filter(
            sqlx::query(&format!(
                "SELECT count(*) FROM thresholds WHERE {}",
                Self::FILTER_WHERE
            )),
            filter,
        )
        .fetch_one(&self.pool)
        .await?
        .try_get(0)?;
        let rows = Self::bind_filter(
            sqlx::query(&format!(
                "SELECT {} FROM thresholds WHERE {} ORDER BY {}, metric, scope_id LIMIT $4",
                Self::COLUMNS,
                Self::FILTER_WHERE,
                Self::scope_level_rank()
            )),
            filter,
        )
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        let items = rows
            .into_iter()
            .map(Self::row_to_threshold)
            .collect::<anyhow::Result<Vec<_>>>()?;
        Ok((items, total))
    }

    /// Bind `$1..=$3` of [`Self::FILTER_WHERE`], in the one order that matches it.
    ///
    /// One helper because the sequence is positional and silent when wrong: swapping two binds
    /// still compiles, still runs, and just answers a different question. The metric goes in as a
    /// *value*, never interpolated — `%` and `_` are wildcards to `ILIKE`, so a raw pattern would
    /// be an operator-supplied search turning into a table scan of everything.
    fn bind_filter<'q>(
        q: sqlx::query::Query<'q, sqlx::Postgres, sqlx::postgres::PgArguments>,
        f: &ThresholdFilter<'_>,
    ) -> sqlx::query::Query<'q, sqlx::Postgres, sqlx::postgres::PgArguments> {
        fn set(tokens: impl Iterator<Item = &'static str>) -> Option<Vec<String>> {
            let v: Vec<String> = tokens.map(str::to_owned).collect();
            (!v.is_empty()).then_some(v)
        }
        q.bind(f.metric.map(str::to_owned))
            .bind(set(f.level.iter().map(|l| l.as_str())))
            .bind(set(f.direction.iter().map(|d| d.as_str())))
    }

    /// Every rule that could conceivably reach one port — the input to
    /// `alerts::matching_rules`, which then decides which of them actually do.
    ///
    /// Not `list_all()`: that is the engine's uncapped read of the whole table, and a browser
    /// opening one port must not pull a fleet's node-level overrides across. Not `list_page`
    /// either: its cap would silently drop rules, and "which rules govern this port" is a question
    /// a prefix cannot answer.
    ///
    /// The predicate is the exact complement of what [`crate::alerts::threshold_applies`] can
    /// reject cheaply. The four broad levels are taken whole because whether one matches depends
    /// on the node's profile, tags and folder chain, which live in the engine's snapshot rather
    /// than in this table; the two narrow levels are matched by their `scope_id`, which is this
    /// table's own column. In practice the broad levels are per profile or per group and stay
    /// small, while the ones that grow with the fleet are exactly the two that get filtered.
    ///
    /// ⚠️ The narrow clauses are string comparisons against the stored `scope_id` because that is
    /// what the column holds — `<uuid>` for a node rule, `<uuid>:<ifindex>` for a port rule. Both
    /// spellings come from `yagra_common`, never assembled here.
    pub async fn candidates_for_interface(
        &self,
        node: uuid::Uuid,
        ifindex: u32,
    ) -> anyhow::Result<Vec<StoredThreshold>> {
        let broad: Vec<String> = ScopeLevel::ALL
            .iter()
            .filter(|l| !matches!(l, ScopeLevel::Node | ScopeLevel::Interface))
            .map(|l| l.as_str().to_owned())
            .collect();
        let rows = sqlx::query(&format!(
            "SELECT {} FROM thresholds \
             WHERE scope_level = ANY($1) \
                OR (scope_level = $2 AND $3 = ANY(scope_ids)) \
                OR (scope_level = $4 AND $5 = ANY(scope_ids)) \
             ORDER BY {}, metric, scope_id",
            Self::COLUMNS,
            Self::scope_level_rank()
        ))
        .bind(broad)
        .bind(ScopeLevel::Node.as_str())
        .bind(node.to_string())
        .bind(ScopeLevel::Interface.as_str())
        .bind(yagra_common::interface_scope_id(node, ifindex))
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter().map(Self::row_to_threshold).collect()
    }

    fn row_to_threshold(row: sqlx::postgres::PgRow) -> anyhow::Result<StoredThreshold> {
        let dwell: i32 = row.try_get("dwell_samples")?;
        // Derived from the bounds, never read from the row's own legacy columns: the row carries
        // both, and the bounds are the ones the engine evaluates. Reporting the column instead
        // would let the API describe a rule the engine does not run.
        let bounds = read_bounds(&row)?;
        Ok(StoredThreshold::new(
            row.try_get("id")?,
            parse_level(&row.try_get::<String, _>("scope_level")?),
            // Migration 0096 backfilled every row that existed when it ran, so an empty array
            // beside a populated `scope_id` can only be a row an OLDER core wrote afterwards —
            // i.e. during a rollback window. Reading that as "no targets" would make the rule
            // inert; reading it as one target is what the older core itself meant by it.
            {
                let ids: Vec<String> = row.try_get("scope_ids")?;
                let primary: String = row.try_get("scope_id")?;
                if ids.is_empty() && !primary.is_empty() {
                    vec![primary]
                } else {
                    ids
                }
            },
            ThresholdRule::new(
                row.try_get::<String, _>("metric")?,
                bounds,
                u32::try_from(dwell).unwrap_or(1),
            ),
        ))
    }

    /// Create a threshold rule; returns its id.
    pub async fn create(&self, w: ThresholdWrite<'_>) -> anyhow::Result<Uuid> {
        let id = Uuid::new_v4();
        let (direction, warning, critical) = w.legacy();
        sqlx::query(
            "INSERT INTO thresholds \
             (id, scope_level, scope_id, scope_ids, metric, direction, warning, critical, \
              warning_below, critical_below, warning_above, critical_above, dwell_samples) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13)",
        )
        .bind(id)
        .bind(w.scope_level)
        .bind(w.primary())
        .bind(w.scope_ids)
        .bind(w.metric)
        .bind(direction)
        .bind(warning)
        .bind(critical)
        .bind(w.bounds.warning_below)
        .bind(w.bounds.critical_below)
        .bind(w.bounds.warning_above)
        .bind(w.bounds.critical_above)
        .bind(w.dwell_samples.max(1))
        .execute(&self.pool)
        .await?;
        Ok(id)
    }

    /// Overwrite a threshold rule **in place, keeping its id**. Returns whether a row was updated.
    ///
    /// Keeping the id is what makes editing safe rather than merely convenient: the alert engine
    /// keys its state on `(node, metric)` — never on this id — so rewriting a rule cannot strand an
    /// open alert or change the dedup key an external on-call tool is holding. Delete-and-recreate
    /// would additionally leave a window in which the rule does not exist, during which the fleet
    /// is not paged for the very thing the operator was adjusting.
    ///
    /// The `dwell_samples.max(1)` floor is applied here **as well as** in [`Self::create`]: an edit
    /// is a second write path to the same column, and a floor that only one writer applies is a
    /// floor the other one silently removes.
    pub async fn update(&self, id: Uuid, w: ThresholdWrite<'_>) -> anyhow::Result<bool> {
        let (direction, warning, critical) = w.legacy();
        let res = sqlx::query(
            "UPDATE thresholds SET scope_level = $2, scope_id = $3, scope_ids = $4, metric = $5, \
             direction = $6, warning = $7, critical = $8, warning_below = $9, \
             critical_below = $10, warning_above = $11, critical_above = $12, \
             dwell_samples = $13 WHERE id = $1",
        )
        .bind(id)
        .bind(w.scope_level)
        .bind(w.primary())
        .bind(w.scope_ids)
        .bind(w.metric)
        .bind(direction)
        .bind(warning)
        .bind(critical)
        .bind(w.bounds.warning_below)
        .bind(w.bounds.critical_below)
        .bind(w.bounds.warning_above)
        .bind(w.bounds.critical_above)
        .bind(w.dwell_samples.max(1))
        .execute(&self.pool)
        .await?;
        Ok(res.rows_affected() > 0)
    }

    /// The id of an existing rule with the same scope and metric, if there is one (ADR-081).
    ///
    /// 🚨 What a second such rule used to do: it was stored, listed, and never evaluated.
    /// `resolve_effective` merged every winner at the most-specific level under **one** direction —
    /// taken from whichever row the index happened to yield first — so a rule facing the other way
    /// had its bounds compared the wrong way round and disappeared. Since ADR-081 a single rule can
    /// bound both sides, so the second row has no reason to exist and is refused instead.
    ///
    /// ⚠️ **The refusal lives here rather than in a `UNIQUE` constraint, deliberately.** A
    /// constraint cannot be added to a table that already holds duplicates: the migration fails,
    /// and a migration that fails is a core that will not start. Measured on the test deployment
    /// 2026-08-21: 0 duplicate groups over 33 rules — one deployment, which is not a proof about
    /// every deployment. So existing duplicates keep resolving exactly as they do today (badly, and
    /// visibly, once one of them is edited) and no new one can be created.
    ///
    /// `scope_ids` compares as an ordered array, matching how the API stores it: the edge already
    /// refuses a repeated target, but it does not sort, so two rules naming the same two profiles
    /// in opposite orders are not caught here. That is the narrow miss this accepts — both rules
    /// still resolve, and the pair is far rarer than the accidental second rule this is for.
    pub async fn find_duplicate(
        &self,
        scope_level: &str,
        scope_ids: &[String],
        metric: &str,
        exclude: Option<Uuid>,
    ) -> anyhow::Result<Option<Uuid>> {
        let row = sqlx::query(
            "SELECT id FROM thresholds \
             WHERE scope_level = $1 AND scope_ids = $2 AND metric = $3 \
               AND ($4::uuid IS NULL OR id <> $4) \
             LIMIT 1",
        )
        .bind(scope_level)
        .bind(scope_ids)
        .bind(metric)
        .bind(exclude)
        .fetch_optional(&self.pool)
        .await?;
        row.map(|r| r.try_get("id")).transpose().map_err(Into::into)
    }

    /// Delete a threshold rule. Returns whether a row was removed.
    pub async fn delete(&self, id: Uuid) -> anyhow::Result<bool> {
        let res = sqlx::query("DELETE FROM thresholds WHERE id = $1")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(res.rows_affected() > 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SRC: &str = include_str!("thresholds.rs");

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
    fn an_unreadable_scope_or_direction_degrades_to_the_broadest_safest_reading() {
        // Neither column can be trusted to hold only what this build knows: a newer core may have
        // written a scope level or direction this one has never heard of. The fallbacks are chosen,
        // not incidental — `Profile` is the *broadest* scope, so a mis-read rule still applies to
        // something rather than silently binding to one node; `Above` is the common direction.
        assert_eq!(parse_level("nonsense"), ScopeLevel::Profile);
        assert_eq!(parse_direction("nonsense"), Direction::Above);
        // Known tokens still resolve, so the fallback is not swallowing everything.
        assert_eq!(parse_level("node"), ScopeLevel::Node);
        assert_eq!(parse_level("group"), ScopeLevel::Group);
        assert_eq!(parse_level("profile"), ScopeLevel::Profile);
        assert_eq!(parse_level("global"), ScopeLevel::Global);
        assert_eq!(parse_direction("below"), Direction::Below);
        assert_eq!(parse_direction("above"), Direction::Above);
    }

    #[test]
    fn the_parsers_accept_exactly_the_vocabulary_the_api_admits() {
        // `create` stores the caller's string verbatim, and `POST /api/v1/thresholds` admits only
        // these tokens. If the writer's vocabulary and this reader ever disagreed, every stored
        // rule would silently read back as the fallback — a node override behaving as a
        // fleet-wide profile rule.
        // Driven from the enum, not from a list written out here — a hand-written list would keep
        // passing after a new level shipped, which is exactly when this check is needed.
        for level in ScopeLevel::ALL {
            assert_eq!(
                parse_level(level.as_str()),
                level,
                "{} does not round-trip",
                level.as_str()
            );
        }
        for dir in [Direction::Above, Direction::Below] {
            assert_eq!(
                parse_direction(dir.as_str()),
                dir,
                "{dir:?} does not round-trip"
            );
        }
    }

    #[test]
    fn a_dwell_of_zero_is_lifted_to_one_sample() {
        // Dwell is the anti-flap guard: zero would mean "commit on the first sample", which is the
        // hysteresis being switched off by a value the API never intended to allow. The floor is
        // applied through a binding rather than on literals so it is the *rule* being exercised
        // and not something the compiler folds away.
        let floor = |dwell: i32| dwell.max(1);
        assert_eq!(floor(0), 1);
        assert_eq!(floor(-5), 1);
        assert_eq!(floor(3), 3);
        // Reading it back converts rather than re-flooring: a stored 0 is honoured as 0 (the write
        // side is where the floor belongs), while a negative — which the column should never hold —
        // cannot convert and falls back to one sample.
        let read = |stored: i32| u32::try_from(stored).unwrap_or(1);
        assert_eq!(read(0), 0);
        assert_eq!(read(-1), 1);
        assert_eq!(read(3), 3);
        // **Twice**, not once: `create` and `update` are two writers of the same column, and a
        // floor only one of them applies is a floor the other silently removes. This counts rather
        // than merely finding one, because the failure it guards is "the second writer was added
        // without it" — which a `contains` check passes.
        assert_eq!(
            production_source().matches("dwell_samples.max(1)").count(),
            2,
            "every writer of dwell_samples applies the anti-flap floor"
        );
    }

    #[test]
    fn the_update_binds_every_placeholder_it_names_and_sets_every_column_but_the_id() {
        // The binds are positional and silent when wrong: swapping two still compiles, still runs,
        // and writes the operator's metric name into the direction column. Counting is the only
        // check there is — nothing about this is a type error.
        let src = production_source();
        let after = src
            .split_once("pub async fn update")
            .expect("update exists")
            .1;
        let body = after.split_once("\n    }").map_or(after, |(b, _)| b);
        // Derived from `COLUMNS` rather than written out: `update` sets every column but the id
        // and then names the id in the `WHERE`, so the count is exactly the column count. Writing
        // the number here instead is what made this test need editing for ADR-081 rather than
        // simply passing — a pin that has to be re-pinned by hand tells you less each time.
        let columns = ThresholdStore::COLUMNS.matches(", ").count() + 1;
        let placeholders = (1..=columns)
            .filter(|n| body.contains(&format!("${n}")))
            .count();
        assert_eq!(placeholders, columns, "{body}");
        assert_eq!(
            body.matches(".bind(").count(),
            placeholders,
            "one bind per placeholder, in the one order that matches the statement"
        );
        // Every stored column is rewritten except the id, which is the key. A column left out of
        // the SET list would keep its old value through an edit that appeared to succeed — the
        // failure mode is a form that saves and changes nothing.
        for col in ThresholdStore::COLUMNS.split(", ").filter(|c| *c != "id") {
            assert!(body.contains(&format!("{col} = $")), "{col} is not updated");
        }
        assert!(body.contains("WHERE id = $1"), "{body}");
    }

    #[test]
    fn the_engines_snapshot_is_never_capped_but_the_api_page_is() {
        let src = production_source();
        // A LIMIT on `list_all` would not shorten a list — it would stop the alert engine
        // evaluating some rules, invisibly. The comment says so; this asserts it.
        let all = src
            .split_once("pub async fn list_all")
            .expect("list_all exists")
            .1;
        let all_body = all.split_once("pub async fn").map_or(all, |(b, _)| b);
        assert!(
            !all_body.contains("LIMIT"),
            "list_all must stay uncapped — a truncated snapshot silently stops alerting"
        );
        // The API page is capped and bound, never interpolated. `$4` because the filter takes the
        // first three placeholders — see `FILTER_WHERE`.
        assert!(src.contains("metric, scope_id LIMIT $4"), "{src}");
    }

    #[test]
    fn the_filter_binds_every_placeholder_it_names_and_interpolates_none() {
        // The predicate is one const with a nullable bind per clause, and the count and the page
        // must ask the *same* question — "500 of 3,200" counting a different set from the one it
        // shows is a wrong answer about what pages the fleet. The needles are built from the
        // constants at runtime: a literal needle in a test that reads its own file matches itself
        // and passes forever.
        let src = production_source();
        let placeholders = (1..=3)
            .filter(|n| ThresholdStore::FILTER_WHERE.contains(&format!("${n}")))
            .count();
        assert_eq!(placeholders, 3, "{}", ThresholdStore::FILTER_WHERE);
        let after = src
            .split_once("fn bind_filter")
            .expect("bind_filter exists")
            .1;
        // Stop at the function's own closing brace — everything after it is other statements that
        // bind too, and counting those would make this assertion pass for the wrong reason.
        let binder = after.split_once("\n    }").map_or(after, |(b, _)| b);
        assert_eq!(
            binder.matches(".bind(").count(),
            placeholders,
            "one bind per placeholder, in the one order that matches the predicate"
        );
        // The operator's search term is a value, never part of the pattern: `%` and `_` are ILIKE
        // wildcards, so an interpolated term would let a typed `%` scan the whole table.
        assert!(
            !ThresholdStore::FILTER_WHERE.contains("{}"),
            "the predicate must not be a format string"
        );
        // Both statements filter, so the total describes the rows being shown.
        let page = src
            .split_once("pub async fn list_page")
            .expect("list_page exists")
            .1;
        let body = page.split_once("fn bind_filter").map_or(page, |(b, _)| b);
        assert_eq!(
            body.matches("Self::FILTER_WHERE").count(),
            2,
            "the count and the page must share the predicate"
        );
    }

    #[test]
    fn the_page_is_ordered_broadest_scope_first_so_it_is_stable() {
        // Without an ORDER BY, PostgreSQL may return a different arbitrary subset for the same
        // page each time, which reads as rules appearing and disappearing.
        //
        // The CASE used to be written out, and was therefore a second copy of the precedence order
        // — a new level could land outside it and sort into `ELSE` while the engine ranked it
        // correctly. It is generated from `ScopeLevel::ALL` now, so this asserts the *generator*:
        // every level present, in the enum's order, with an `ELSE` past the end for a level a
        // newer core wrote before a rollback.
        let sql = ThresholdStore::scope_level_rank();
        for (rank, level) in ScopeLevel::ALL.iter().enumerate() {
            assert!(
                sql.contains(&format!("WHEN '{}' THEN {rank} ", level.as_str())),
                "{level:?} is missing from the ORDER BY rank, or is not at rank {rank}: {sql}"
            );
        }
        assert!(
            sql.ends_with(&format!("ELSE {} END", ScopeLevel::ALL.len())),
            "{sql}"
        );
        assert!(
            production_source().contains("ORDER BY {}, metric, scope_id LIMIT $4"),
            "the page must order by the generated rank, not a hand-written CASE"
        );
    }

    #[test]
    fn the_column_list_is_named_once_and_used_by_every_read() {
        // Every read builds its SELECT from `COLUMNS`, so the positional `row_to_threshold`
        // cannot drift from what was selected. Three of them: the engine's uncapped snapshot, the
        // API's capped page, and the per-interface candidate set (ADR-076 決定 11).
        let src = production_source();
        assert_eq!(src.matches("Self::COLUMNS").count(), 3);
        // 13 since ADR-081 added the four bounds. Deliberately a written-out number: this is the
        // one assertion whose whole job is to make someone confirm the column set changed on
        // purpose, so deriving it from `COLUMNS` would make it assert nothing.
        assert_eq!(ThresholdStore::COLUMNS.matches(", ").count() + 1, 13);
    }
}
