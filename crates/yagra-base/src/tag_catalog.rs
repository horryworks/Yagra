// SPDX-License-Identifier: AGPL-3.0-only
//! The deployment's tags as one list, and the two writes that act on every place a tag is named
//! (ADR-263).
//!
//! A tag (ADR-135) is free text stored where it is used. It lives in six columns: a node's and a
//! folder's own labels, the labels each refuses to inherit, the tags a notification routing rule
//! names (ADR-259) and the tags an alert-grouping exception names
//! (ADR-262). This module is the one place that knows all six, so a rename or a delete cannot fix
//! the nodes and forget the rule that was waiting for them.
//!
//! Since ADR-263 Inc.2 a tag may also be **registered** by name (`registered_tags`, migration
//! 0189) before anything carries it. Registration is a suggestion, not a restriction: a tag no one
//! registered is still carried, listed and renamed like any other. The rename and the delete keep
//! the registry in step inside the same transaction as the six columns.
//!
//! 🚨 **Every write here touches all six columns in one transaction.** Renaming the nodes' copy
//! alone is the accident this module exists to prevent: the rule still names the old spelling,
//! matches nobody, and nothing says so.
//!
//! It sits at the crate root rather than in `repo/` because it is about no one table — the
//! ownership check in `repo/guards.rs` assigns each file there the tables it may name, and a file
//! whose subject is "every table a tag lives in" belongs to none of them.

use std::collections::BTreeMap;

use sqlx::{PgPool, Row};

/// Where one tag is named, counted per place. Effective counts (a node's own labels plus its
/// folders', minus its refusals) are not here: they need the folder tree, which core resolves.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TagUses {
    /// Nodes carrying it among their own labels.
    pub nodes: u64,
    /// Folders carrying it, which hand it to everything beneath them.
    pub folders: u64,
    /// Nodes and folders refusing to inherit it.
    pub refusals: u64,
    /// Notification routing rules naming it.
    pub rules: u64,
    /// Alert-grouping exceptions naming it.
    pub exceptions: u64,
    /// An Admin registered it by name (ADR-263 Inc.2).
    pub registered: bool,
}

/// How many rows each place changed in one rename or delete. `0` everywhere means the tag was
/// named nowhere — not an error, the answer is simply that nothing needed changing.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TagRewrite {
    pub nodes: u64,
    pub node_refusals: u64,
    pub folders: u64,
    pub folder_refusals: u64,
    pub rules: u64,
    pub exceptions: u64,
    /// Registered names changed (0 or 1).
    pub registered: u64,
}

impl TagRewrite {
    /// Rows changed across all six places.
    #[must_use]
    pub fn total(&self) -> u64 {
        self.nodes
            + self.node_refusals
            + self.folders
            + self.folder_refusals
            + self.rules
            + self.exceptions
            + self.registered
    }
}

/// Why a delete was refused, naming what would change meaning. Nothing was written.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DeleteBlocked {
    /// Routing rules that name only this tag and no folder: without it they reach every node.
    pub rules: Vec<String>,
    /// Exceptions that name only this tag and no folder: without it they name nothing, which the
    /// table's CHECK (migration 0185) refuses.
    pub exceptions: u64,
}

/// The outcome of a delete.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeleteOutcome {
    Deleted(TagRewrite),
    Blocked(DeleteBlocked),
}

/// The six places, as `(table, column, kind)`. Read by both writes and by [`uses`], so a seventh
/// column is added once. `kind` names the [`TagUses`] field the place counts towards.
///
/// ⚠️ `nodes` is first and is the only table with an `updated_at` the writes keep current; the
/// statement builder below special-cases it by name.
const PLACES: [(&str, &str, &str); 6] = [
    ("nodes", "tags", "node"),
    ("nodes", "tags_excluded", "refusal"),
    ("node_groups", "tags", "folder"),
    ("node_groups", "tags_excluded", "refusal"),
    ("routing_rules", "tags", "rule"),
    ("alert_grouping_exceptions", "tags", "exception"),
];

fn into_rewrite(counts: [u64; 6], registered: u64) -> TagRewrite {
    TagRewrite {
        registered,
        nodes: counts[0],
        node_refusals: counts[1],
        folders: counts[2],
        folder_refusals: counts[3],
        rules: counts[4],
        exceptions: counts[5],
    }
}

fn touch(table: &str) -> &'static str {
    if table == "nodes" {
        ", updated_at = now()"
    } else {
        ""
    }
}

/// Every tag named anywhere, with where, and every registered one. A tag named only by a rule, an
/// exception or a refusal is listed too: it is exactly the kind of leftover the list is for. A
/// registered tag nothing carries is listed with every count at zero.
pub async fn uses(pool: &PgPool) -> anyhow::Result<BTreeMap<String, TagUses>> {
    let places = PLACES
        .iter()
        .map(|(table, col, kind)| {
            format!("SELECT unnest({col}) AS t, '{kind}' AS kind FROM {table}")
        })
        .collect::<Vec<_>>()
        .join(" UNION ALL ");
    let rows = sqlx::query(&format!(
        "SELECT t, kind, count(*) AS n FROM ({places}) s GROUP BY t, kind"
    ))
    .fetch_all(pool)
    .await?;
    let mut out: BTreeMap<String, TagUses> = BTreeMap::new();
    for row in rows {
        let tag: String = row.try_get("t")?;
        let kind: String = row.try_get("kind")?;
        let n = u64::try_from(row.try_get::<i64, _>("n")?).unwrap_or(0);
        let entry = out.entry(tag).or_default();
        match kind.as_str() {
            "node" => entry.nodes += n,
            "folder" => entry.folders += n,
            "refusal" => entry.refusals += n,
            "rule" => entry.rules += n,
            "exception" => entry.exceptions += n,
            other => anyhow::bail!("unexpected tag place {other:?}"),
        }
    }
    for tag in registered(pool).await? {
        out.entry(tag).or_default().registered = true;
    }
    Ok(out)
}

/// The registered tags, by name (ADR-263 Inc.2). What the tag fields suggest.
pub async fn registered(pool: &PgPool) -> anyhow::Result<Vec<String>> {
    Ok(
        sqlx::query_scalar("SELECT tag FROM registered_tags ORDER BY tag")
            .fetch_all(pool)
            .await?,
    )
}

/// Register `tag` by name. `true` when it was not registered before; registering one twice is not
/// an error. The caller validates it (`api::nodes::validated_labels`).
pub async fn register(pool: &PgPool, tag: &str) -> anyhow::Result<bool> {
    let res = sqlx::query("INSERT INTO registered_tags (tag) VALUES ($1) ON CONFLICT DO NOTHING")
        .bind(tag)
        .execute(pool)
        .await?;
    Ok(res.rows_affected() > 0)
}

/// Rename `from` to `to` everywhere. When `to` is already named in a row, `from` is dropped from
/// that row instead, so a merge never leaves a duplicate. Order inside each array is kept.
///
/// The caller validates `to` (`api::nodes::validated_labels`) and refuses `from == to`; `from` is
/// deliberately not validated, so an old label the current rules would refuse can still be renamed
/// away.
pub async fn rename(pool: &PgPool, from: &str, to: &str) -> anyhow::Result<TagRewrite> {
    let mut tx = pool.begin().await?;
    let mut counts = [0u64; 6];
    for (i, (table, col, _)) in PLACES.iter().enumerate() {
        let res = sqlx::query(&format!(
            "UPDATE {table} SET {col} = CASE WHEN $2 = ANY({col}) \
                 THEN array_remove({col}, $1) ELSE array_replace({col}, $1, $2) END{touch} \
             WHERE $1 = ANY({col})",
            touch = touch(table),
        ))
        .bind(from)
        .bind(to)
        .execute(&mut *tx)
        .await?;
        counts[i] = res.rows_affected();
    }
    // A registered name follows the rename; onto a name already registered, the old one just goes.
    sqlx::query(
        "INSERT INTO registered_tags (tag) \
         SELECT $2 WHERE EXISTS (SELECT 1 FROM registered_tags WHERE tag = $1) \
         ON CONFLICT DO NOTHING",
    )
    .bind(from)
    .bind(to)
    .execute(&mut *tx)
    .await?;
    let registered = sqlx::query("DELETE FROM registered_tags WHERE tag = $1")
        .bind(from)
        .execute(&mut *tx)
        .await?
        .rows_affected();
    tx.commit().await?;
    Ok(into_rewrite(counts, registered))
}

/// Remove `tag` everywhere — unless that would change what a rule or an exception means, in which
/// case nothing is written and the outcome names them.
///
/// A row is in the way when this tag is its only tag and losing it would widen it: with no folder
/// named, a rule would reach every node (and an exception would name nothing, which the table's
/// CHECK refuses); joined with its folders by `and` (ADR-262 decision 10), it would reach the whole
/// of those folders instead of the tagged nodes in them.
///
/// 🚨 **The two tables are locked before the check, not only the rows it finds.** A row lock
/// covers the rows that match *now*; a rule edited to name only this tag between the check and the
/// write would match neither and be widened silently. `SHARE ROW EXCLUSIVE` holds off every
/// concurrent write to both tables for the length of this transaction and leaves their readers (the
/// 30-second routing refresh) alone. Both tables hold tens of rows.
pub async fn delete(pool: &PgPool, tag: &str) -> anyhow::Result<DeleteOutcome> {
    let mut tx = pool.begin().await?;
    sqlx::query("LOCK TABLE routing_rules, alert_grouping_exceptions IN SHARE ROW EXCLUSIVE MODE")
        .execute(&mut *tx)
        .await?;
    let rules: Vec<String> = sqlx::query_scalar(
        "SELECT name FROM routing_rules \
          WHERE tags = ARRAY[$1]::text[] AND (cardinality(group_ids) = 0 OR join_and) \
          ORDER BY position, name",
    )
    .bind(tag)
    .fetch_all(&mut *tx)
    .await?;
    let exceptions: Vec<sqlx::types::Uuid> = sqlx::query_scalar(
        "SELECT id FROM alert_grouping_exceptions \
          WHERE tags = ARRAY[$1]::text[] AND (cardinality(group_ids) = 0 OR join_and)",
    )
    .bind(tag)
    .fetch_all(&mut *tx)
    .await?;
    if !rules.is_empty() || !exceptions.is_empty() {
        tx.rollback().await?;
        return Ok(DeleteOutcome::Blocked(DeleteBlocked {
            rules,
            exceptions: exceptions.len() as u64,
        }));
    }
    let mut counts = [0u64; 6];
    for (i, (table, col, _)) in PLACES.iter().enumerate() {
        let res = sqlx::query(&format!(
            "UPDATE {table} SET {col} = array_remove({col}, $1){touch} WHERE $1 = ANY({col})",
            touch = touch(table),
        ))
        .bind(tag)
        .execute(&mut *tx)
        .await?;
        counts[i] = res.rows_affected();
    }
    let registered = sqlx::query("DELETE FROM registered_tags WHERE tag = $1")
        .bind(tag)
        .execute(&mut *tx)
        .await?
        .rows_affected();
    tx.commit().await?;
    Ok(DeleteOutcome::Deleted(into_rewrite(counts, registered)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    // The rule and the exception are written by hand: their writers live in core, which this crate
    // cannot name. Only the columns the two writes read are given; the rest take their defaults.
    async fn rule(pool: &PgPool, name: &str, tags: &[&str], groups: &[Uuid]) {
        sqlx::query(
            "INSERT INTO routing_rules (id, name, tags, group_ids) VALUES ($1, $2, $3, $4)",
        )
        .bind(Uuid::new_v4())
        .bind(name)
        .bind(tags.iter().map(|t| (*t).to_owned()).collect::<Vec<_>>())
        .bind(groups)
        .execute(pool)
        .await
        .expect("rule");
    }

    async fn exception(pool: &PgPool, tags: &[&str], groups: &[Uuid]) {
        sqlx::query(
            "INSERT INTO alert_grouping_exceptions (id, group_ids, tags, level) VALUES ($1, $2, $3, 1)",
        )
        .bind(Uuid::new_v4())
        .bind(groups)
        .bind(tags.iter().map(|t| (*t).to_owned()).collect::<Vec<_>>())
        .execute(pool)
        .await
        .expect("exception");
    }

    async fn column(pool: &PgPool, sql: &str) -> Vec<Vec<String>> {
        sqlx::query_scalar(sql).fetch_all(pool).await.expect(sql)
    }

    async fn label(pool: &PgPool, table: &str, col: &str, tags: &[&str]) {
        sqlx::query(&format!("UPDATE {table} SET {col} = $1"))
            .bind(tags.iter().map(|t| (*t).to_owned()).collect::<Vec<_>>())
            .execute(pool)
            .await
            .expect("label");
    }

    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn a_rename_reaches_all_six_places_and_a_merge_leaves_no_duplicate(pool: PgPool) {
        let folder = crate::pgtest::group(&pool, "Japan").await;
        crate::pgtest::node(&pool, "core-01", 1, Some(folder)).await;
        label(&pool, "nodes", "tags", &["TeamA", "core"]).await;
        label(&pool, "nodes", "tags_excluded", &["TeamA"]).await;
        label(&pool, "node_groups", "tags", &["TeamA"]).await;
        label(&pool, "node_groups", "tags_excluded", &["TeamA"]).await;
        rule(&pool, "team a", &["TeamA"], &[]).await;
        exception(&pool, &["TeamA", "TeamB"], &[]).await;

        let before = uses(&pool).await.expect("uses");
        assert_eq!(
            before["TeamA"],
            TagUses {
                nodes: 1,
                folders: 1,
                refusals: 2,
                rules: 1,
                exceptions: 1,
                registered: false,
            }
        );

        let done = rename(&pool, "TeamA", "TeamB").await.expect("rename");
        assert_eq!(done.total(), 6, "{done:?}");
        assert_eq!(
            column(&pool, "SELECT tags FROM nodes").await,
            vec![vec!["TeamB", "core"]]
        );
        assert_eq!(
            column(&pool, "SELECT tags_excluded FROM nodes").await,
            vec![vec!["TeamB"]]
        );
        assert_eq!(
            column(&pool, "SELECT tags FROM node_groups").await,
            vec![vec!["TeamB"]]
        );
        assert_eq!(
            column(&pool, "SELECT tags FROM routing_rules").await,
            vec![vec!["TeamB"]]
        );
        // The exception already named TeamB: the merge drops TeamA rather than naming TeamB twice.
        assert_eq!(
            column(&pool, "SELECT tags FROM alert_grouping_exceptions").await,
            vec![vec!["TeamB"]]
        );
        assert!(!uses(&pool).await.expect("uses").contains_key("TeamA"));

        // Nothing left to rename: every count is zero, and that is not an error.
        assert_eq!(
            rename(&pool, "TeamA", "TeamC")
                .await
                .expect("again")
                .total(),
            0
        );
    }

    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn a_delete_that_would_widen_a_rule_or_empty_an_exception_writes_nothing(pool: PgPool) {
        let folder = crate::pgtest::group(&pool, "Japan").await;
        crate::pgtest::node(&pool, "core-01", 1, Some(folder)).await;
        label(&pool, "nodes", "tags", &["TeamA"]).await;
        rule(&pool, "only team a", &["TeamA"], &[]).await;
        // Named beside a folder with `or`: losing the tag narrows it to the folder, not refused.
        rule(&pool, "team a or japan", &["TeamA"], &[folder]).await;
        // With `and`, losing the tag would widen it to the whole folder: refused.
        rule(&pool, "team a and japan", &["TeamA"], &[folder]).await;
        sqlx::query("UPDATE routing_rules SET join_and = true WHERE name = 'team a and japan'")
            .execute(&pool)
            .await
            .expect("join and");
        exception(&pool, &["TeamA"], &[]).await;

        match delete(&pool, "TeamA").await.expect("delete") {
            DeleteOutcome::Blocked(b) => {
                let mut names = b.rules.clone();
                names.sort();
                assert_eq!(
                    names,
                    vec!["only team a".to_owned(), "team a and japan".to_owned()]
                );
                assert_eq!(b.exceptions, 1);
            }
            other => panic!("expected a refusal, got {other:?}"),
        }
        assert_eq!(
            column(&pool, "SELECT tags FROM nodes").await,
            vec![vec!["TeamA"]]
        );

        sqlx::query("DELETE FROM routing_rules WHERE name <> 'team a or japan'")
            .execute(&pool)
            .await
            .expect("drop rule");
        sqlx::query("DELETE FROM alert_grouping_exceptions")
            .execute(&pool)
            .await
            .expect("drop exception");
        match delete(&pool, "TeamA").await.expect("delete") {
            DeleteOutcome::Deleted(d) => assert_eq!((d.nodes, d.rules), (1, 1)),
            other => panic!("expected a delete, got {other:?}"),
        }
        assert_eq!(
            column(&pool, "SELECT tags FROM nodes").await,
            vec![Vec::<String>::new()]
        );
        assert_eq!(
            column(&pool, "SELECT tags FROM routing_rules").await,
            vec![Vec::<String>::new()]
        );
    }

    /// ADR-263 Inc.2: a registered tag is listed with nothing carrying it, registering it twice
    /// keeps one row, a rename carries the registration to the new name (and a merge drops the old
    /// one), and a delete removes it.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn a_registered_tag_follows_its_rename_and_goes_with_its_delete(pool: PgPool) {
        assert!(register(&pool, "TeamC").await.expect("register"));
        assert!(
            !register(&pool, "TeamC").await.expect("again"),
            "a second register is a no-op"
        );
        assert!(register(&pool, "TeamD").await.expect("register"));
        let listed = uses(&pool).await.expect("uses");
        assert_eq!(
            listed["TeamC"],
            TagUses {
                registered: true,
                ..TagUses::default()
            }
        );

        let renamed = rename(&pool, "TeamC", "TeamE").await.expect("rename");
        assert_eq!(renamed.registered, 1);
        assert_eq!(
            registered(&pool).await.expect("list"),
            vec!["TeamD", "TeamE"]
        );

        // Onto a name already registered: the two become one.
        rename(&pool, "TeamE", "TeamD").await.expect("merge");
        assert_eq!(registered(&pool).await.expect("list"), vec!["TeamD"]);

        match delete(&pool, "TeamD").await.expect("delete") {
            DeleteOutcome::Deleted(d) => assert_eq!(d.registered, 1),
            other => panic!("expected a delete, got {other:?}"),
        }
        assert!(registered(&pool).await.expect("list").is_empty());
    }
}
