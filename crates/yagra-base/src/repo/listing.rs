// SPDX-License-Identifier: AGPL-3.0-only
//! The read-only node inventory the API sees: [`NodeListing`] and its two implementations.
//!
//! [`NodeRepo`] is the live one and [`StaticNodeList`] is skeleton mode (no database), so the
//! router does not care which is behind it. That makes the two a **mirror** of one rule —
//! [`NodeRepo::SCOPE_PREDICATE`] in SQL, [`StaticNodeList::in_scope`] in Rust — and the tests
//! below assert the rule rather than either implementation's internal consistency
//! (`extensibility.md` §2).
//!
//! 🚨 The scope filter is why every method takes a [`super::GroupFilter`]: `Some(&[])` means "no
//! groups", i.e. match nothing, and collapsing it into `None` would turn a broken scope into
//! unrestricted access.

use std::net::{IpAddr, Ipv4Addr};

use async_trait::async_trait;
use sqlx::Row;
use uuid::Uuid;
use yagra_common::{Node, NodeId};

// Only the settings struct: `retention::Row` would collide with `sqlx::Row` above.

use super::seed::DEMO_NODE_ID;
use super::*;

/// Ceiling on one server-side node search page.
///
/// One number, deliberately: the API edge and both [`NodeListing`] implementations clamp against
/// it. It used to be written twice — the edge clamped to 500 and documented that as the maximum,
/// while the SQL re-clamped to 100 — so filtering a fleet with thousands of matches silently
/// returned 100 rows with nothing saying the list had been cut (extensibility.md §3: the same
/// fact in two places drifts, and the copy that is wrong is the one nobody reads).
pub const NODE_SEARCH_MAX: i64 = 500;

/// Ceiling on one *candidate scan* — the rows filter mode examines before answering.
///
/// Larger than [`NODE_SEARCH_MAX`] and not a contradiction of it: that one is how many rows a
/// single **answer** may contain, this one is how many the server may **look at** to build it.
/// They were one number while looking at a row and returning it were the same act; the Nodes
/// tree's state / kind / pool filters separated the two, because none of the three can be a `WHERE`
/// clause (see `api::nodes`) and each therefore rejects candidates after the query.
///
/// The invariant that matters is the direction: the scan ceiling must be **at or above** the page
/// ceiling. The bug this file's other constant was written for was the inverse — the edge promised
/// 500 while the SQL quietly cut to 100 — so a repo that returns *fewer* rows than its caller
/// documented is the failure, and a repo that returns more is simply clamped again upstream.
pub const NODE_SCAN_MAX: i64 = 5_000;

/// The direction is the whole point, and it is checked at **compile** time rather than in a test —
/// the values are constants, so a runtime assertion could only ever be a slower way of finding out.
/// A repo ceiling *below* what a caller documented is the original bug; above it is harmless,
/// because every caller clamps again to its own page.
const _: () = assert!(NODE_SEARCH_MAX <= NODE_SCAN_MAX);

/// A read-only source of the node inventory for the API. Implemented by [`NodeRepo`]
/// (live, PostgreSQL) and [`StaticNodeList`] (skeleton mode), so the router doesn't care
/// which is behind it.
///
/// Every method takes a [`GroupFilter`]. That is a signature change made on purpose: it is what
/// forces each call site to answer "what may this caller see" rather than defaulting to the whole
/// fleet, and the compiler asks the question at every one of them.
#[async_trait]
pub trait NodeListing: Send + Sync {
    /// One keyset page: nodes with `id > after` (or from the start), ordered by id,
    /// capped at `limit`. The API paginates with this so large inventories don't load
    /// everything (ui-conventions: scale-aware lists).
    async fn list_page(
        &self,
        groups: GroupFilter<'_>,
        after: Option<Uuid>,
        limit: i64,
    ) -> anyhow::Result<Vec<Node>>;
    /// Node count within the caller's scope — the denominator the fleet summary needs (the paged
    /// `list_page` only ever sees one page). Cheap `count(*)`, no row transfer.
    async fn count(&self, groups: GroupFilter<'_>) -> anyhow::Result<i64>;
    /// Every node's `(id, group_id)` — the lightweight join key for the per-group health rollup
    /// (site-matrix / region-rollup / geo-map widgets). Two columns only; the live state comes
    /// from the in-memory alert engine, so the grouping happens in-process (PG holds no live
    /// state). O(fleet) but a single indexed scan, computed server-side once per poll instead of
    /// shipping the whole inventory to every dashboard client (which under-counted every group
    /// past the first page — a correctness bug, S12/A-1).
    async fn node_group_map(
        &self,
        groups: GroupFilter<'_>,
    ) -> anyhow::Result<Vec<(Uuid, Option<Uuid>)>>;
    /// Nodes every one of whose `words` matches (ADR-258; no words ⇒ the first page ordered by
    /// name), capped at `limit`. A word matches a node when it is a case-insensitive substring of
    /// the node's name or its monitored address, when it is a substring of one of its ports'
    /// addresses (only for a word [`yagra_common::node_search::looks_like_address_part`] accepts —
    /// those sit in a JSON document the query unpacks per node), or when the node's folder is one
    /// of the word's [`SearchWord::folders`]. Backs the node-picker's typeahead, the Nodes tree and
    /// MCP `list_nodes`, so none of them loads the inventory into a browser (ui-conventions:
    /// search is server-side at fleet scale, A-2).
    ///
    /// `address`, when given, keeps only nodes **at exactly that address** (ADR-139 Inc.2) —
    /// compared as an address, not as text, so `2001:DB8::1` finds `2001:db8::1`. It is a second
    /// narrowing beside the substring rather than a spelling of it: a substring of `10.0.0.1` also
    /// matches `10.0.0.10`, and a name-ordered page capped at `limit` can hold every one of those
    /// and not the node that is actually at the address.
    ///
    /// `tags`, when given, keeps only nodes that **may** carry one of its tags (see
    /// [`TagNarrowing`]). It is a superset: the caller still decides, per node, with the full
    /// effective-tag rule.
    async fn search(
        &self,
        groups: GroupFilter<'_>,
        words: &[SearchWord],
        address: Option<IpAddr>,
        tags: Option<&TagNarrowing>,
        limit: i64,
    ) -> anyhow::Result<Vec<Node>>;
}

/// The candidates a tag filter can possibly select, in a form the query can serve (ADR-265 Inc.2).
///
/// A node's effective tags are its own plus those its folder chain supplies, minus the ones it
/// excludes — a fold the store cannot do (`inventory::tagres` in core owns it). But the store can
/// keep every node for which the answer could be yes: one whose own tags hold a wanted tag, or one
/// filed in a folder whose chain supplies one. The caller then applies the exact rule to what
/// comes back.
///
/// 🚨 **Without this the tag filter ran over the first [`NODE_SCAN_MAX`] nodes by name and judged
/// only those**, so on a fleet larger than that a tagged node sorting late was never examined —
/// measured on a 6,841-node deployment, where whole sites vanished from the filtered tree.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TagNarrowing {
    /// The wanted tags, compared exactly (tags are case-sensitive) against a node's own tags.
    pub tags: Vec<String>,
    /// Every folder whose own tags or whose ancestors' tags hold a wanted tag.
    pub folders: Vec<Uuid>,
}

impl TagNarrowing {
    /// Whether `node` may carry one of the tags: the in-memory twin of the SQL predicate.
    #[must_use]
    pub fn admits(&self, node: &Node) -> bool {
        node.tags
            .iter()
            .any(|t| self.tags.iter().any(|w| w == t.trim()))
            || node
                .group
                .is_some_and(|g| self.folders.contains(&g.as_uuid()))
    }
}

/// One word of a node search (ADR-258), as the stores receive it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SearchWord {
    /// Lower-cased, as [`yagra_common::node_search::search_words`] produced it.
    pub text: String,
    /// The folders whose nodes this word matches whatever their names: every folder the caller
    /// can see whose own name holds the word, and each one's subtree. The caller works it out,
    /// because which folders a caller can see is not this layer's question (ADR-014).
    pub folders: Vec<Uuid>,
}

impl SearchWord {
    /// A word that matches no folder.
    #[must_use]
    pub fn plain(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            folders: Vec::new(),
        }
    }
}

/// The SQL for one word, its parameters starting at `$first`: the word as a `LIKE` body, then
/// its folder ids. The text is ours; the word itself is always a bound value (security.md).
fn word_predicate(first: usize, ports: bool) -> String {
    let w = first;
    let f = first + 1;
    let like = format!(r"'%' || ${w} || '%' ESCAPE '\'");
    let port = if ports {
        format!(
            " OR EXISTS (SELECT 1 FROM node_l3 l \
               CROSS JOIN LATERAL jsonb_array_elements(coalesce(l.addresses->'addresses', '[]'::jsonb)) a \
               WHERE l.node_id = nodes.id AND (a->>'ip') ILIKE {like})"
        )
    } else {
        String::new()
    };
    format!(
        "(name ILIKE {like} OR host(address) ILIKE {like} \
          OR group_id = ANY(${f}::uuid[]){port})"
    )
}

#[async_trait]
impl NodeListing for NodeRepo {
    async fn list_page(
        &self,
        groups: GroupFilter<'_>,
        after: Option<Uuid>,
        limit: i64,
    ) -> anyhow::Result<Vec<Node>> {
        self.list_nodes_page(groups, after, limit).await
    }
    async fn count(&self, groups: GroupFilter<'_>) -> anyhow::Result<i64> {
        Ok(sqlx::query_scalar::<_, i64>(&format!(
            "SELECT count(*) FROM nodes WHERE {}",
            Self::SCOPE_PREDICATE
        ))
        .bind(Self::scope_bind(groups))
        .fetch_one(&self.pool)
        .await?)
    }
    async fn node_group_map(
        &self,
        groups: GroupFilter<'_>,
    ) -> anyhow::Result<Vec<(Uuid, Option<Uuid>)>> {
        let rows = sqlx::query(&format!(
            "SELECT id, group_id FROM nodes WHERE {}",
            Self::SCOPE_PREDICATE
        ))
        .bind(Self::scope_bind(groups))
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter()
            .map(|row| Ok((row.try_get("id")?, row.try_get("group_id")?)))
            .collect()
    }
    async fn search(
        &self,
        groups: GroupFilter<'_>,
        words: &[SearchWord],
        address: Option<IpAddr>,
        tags: Option<&TagNarrowing>,
        limit: i64,
    ) -> anyhow::Result<Vec<Node>> {
        let limit = limit.clamp(1, NODE_SCAN_MAX);
        // The exact-address narrowing is an `inet` equality, so `nodes_address_idx` serves it. Bound
        // as text and cast twice — for the NULL test and for the comparison — because one parameter
        // used as both `text` and `inet` is a type PostgreSQL refuses to infer (sqlx's `ipnetwork`
        // feature stays out of the build; see `groups::attach_prefixes`).
        let address = address.map(|a| a.to_string());
        // One parenthesised predicate per word, ANDed (ADR-258). Only placeholder numbers are
        // formatted in; every word and folder list is bound. `host(address)` strips the netmask so
        // an address substring matches the displayed address.
        let mut sql = format!(
            "SELECT {} FROM nodes WHERE {} AND ($2::text IS NULL OR address = $2::text::inet)",
            Self::NODE_COLUMNS,
            Self::SCOPE_PREDICATE
        );
        for (i, word) in words.iter().enumerate() {
            let ports = yagra_common::node_search::looks_like_address_part(&word.text);
            sql.push_str(" AND ");
            sql.push_str(&word_predicate(4 + 2 * i, ports));
        }
        if tags.is_some() {
            // `btrim` because a stored tag counts by its meaningful form (`tagres::meaningful`);
            // a row written before validation trimmed must not fall out of the superset.
            let t = 4 + 2 * words.len();
            let f = t + 1;
            sql.push_str(&format!(
                " AND (EXISTS (SELECT 1 FROM unnest(tags) AS own(tag)                    WHERE btrim(own.tag) = ANY(${t}::text[]))                    OR group_id = ANY(${f}::uuid[]))"
            ));
        }
        sql.push_str(" ORDER BY name, id LIMIT $3");
        let mut query = sqlx::query(&sql)
            .bind(Self::scope_bind(groups))
            .bind(&address)
            .bind(limit);
        for word in words {
            query = query
                .bind(yagra_common::node_search::like_escape(&word.text))
                .bind(&word.folders);
        }
        if let Some(n) = tags {
            query = query.bind(&n.tags).bind(&n.folders);
        }
        let rows = query.fetch_all(&self.pool).await?;
        rows.iter().map(node_from_row).collect()
    }
}

/// A fixed node list for skeleton mode (no database). Mirrors the live seed's demo node
/// so the WebUI shows the same nil-id loopback node.
pub struct StaticNodeList(Vec<Node>);

impl StaticNodeList {
    /// The single demo node (nil id → loopback) used by the skeleton.
    #[must_use]
    pub fn demo() -> Self {
        Self(vec![Node::new(
            NodeId::from(DEMO_NODE_ID),
            "demo-localhost",
            IpAddr::V4(Ipv4Addr::LOCALHOST),
        )])
    }
}

impl StaticNodeList {
    /// The in-memory twin of [`NodeRepo::SCOPE_PREDICATE`]. The two are a mirror
    /// (`extensibility.md` §2), so they are asserted against each other in the tests below rather
    /// than each against itself — a skeleton that scoped differently from the live store would be a
    /// silent behavioural difference exactly where nobody looks.
    fn in_scope(groups: GroupFilter<'_>, node: &Node) -> bool {
        match groups {
            None => true,
            // Mirrors `group_id = ANY($1)`: an ungrouped node matches no group, so it is invisible
            // to a scoped caller — the same rule as `Scope::allows`.
            Some(allowed) => node.group.is_some_and(|g| allowed.contains(&g.as_uuid())),
        }
    }
}

#[async_trait]
impl NodeListing for StaticNodeList {
    async fn list_page(
        &self,
        groups: GroupFilter<'_>,
        after: Option<Uuid>,
        limit: i64,
    ) -> anyhow::Result<Vec<Node>> {
        let mut nodes: Vec<Node> = self
            .0
            .iter()
            .filter(|n| Self::in_scope(groups, n))
            .cloned()
            .collect();
        nodes.sort_by_key(|n| n.id.as_uuid());
        Ok(nodes
            .into_iter()
            .filter(|n| after.is_none_or(|a| n.id.as_uuid() > a))
            .take(limit.clamp(1, 501) as usize)
            .collect())
    }
    async fn count(&self, groups: GroupFilter<'_>) -> anyhow::Result<i64> {
        Ok(self.0.iter().filter(|n| Self::in_scope(groups, n)).count() as i64)
    }
    async fn node_group_map(
        &self,
        groups: GroupFilter<'_>,
    ) -> anyhow::Result<Vec<(Uuid, Option<Uuid>)>> {
        Ok(self
            .0
            .iter()
            .filter(|n| Self::in_scope(groups, n))
            .map(|n| (n.id.as_uuid(), n.group.map(|g| g.as_uuid())))
            .collect())
    }
    async fn search(
        &self,
        groups: GroupFilter<'_>,
        words: &[SearchWord],
        address: Option<IpAddr>,
        tags: Option<&TagNarrowing>,
        limit: i64,
    ) -> anyhow::Result<Vec<Node>> {
        let mut nodes: Vec<Node> = self
            .0
            .iter()
            .filter(|n| Self::in_scope(groups, n))
            // Mirrors `address = $n::inet`: an address compared as an address, never as text.
            .filter(|n| address.is_none_or(|a| n.address == a))
            // Mirrors the tag predicate.
            .filter(|n| tags.is_none_or(|t| t.admits(n)))
            // Mirrors the per-word predicate. The skeleton has no port addresses, so that arm has
            // nothing to read here.
            .filter(|n| {
                words.iter().all(|w| {
                    n.name.to_lowercase().contains(&w.text)
                        || n.address.to_string().contains(&w.text)
                        || n.group.is_some_and(|g| w.folders.contains(&g.as_uuid()))
                })
            })
            .cloned()
            .collect();
        nodes.sort_by(|a, b| {
            a.name
                .cmp(&b.name)
                .then(a.id.as_uuid().cmp(&b.id.as_uuid()))
        });
        nodes.truncate(limit.clamp(1, NODE_SCAN_MAX) as usize);
        Ok(nodes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The words of `term`, as the API edge hands them over (no folder matches).
    fn w(term: &str) -> Vec<SearchWord> {
        yagra_common::node_search::search_words(term)
            .expect("a test term has few words")
            .into_iter()
            .map(SearchWord::plain)
            .collect()
    }

    fn node(id: u128, name: &str, addr: &str, group: Option<u128>) -> Node {
        let mut n = Node::new(
            NodeId::from(Uuid::from_u128(id)),
            name,
            addr.parse::<IpAddr>().unwrap(),
        );
        n.group = group.map(|g| GroupId::from(Uuid::from_u128(g)));
        n
    }

    #[tokio::test]
    async fn static_node_list_node_group_map_reports_group_membership() {
        let list = StaticNodeList(vec![
            node(1, "a", "10.0.0.1", Some(100)),
            node(2, "b", "10.0.0.2", None),
        ]);
        let map = list.node_group_map(None).await.unwrap();
        assert!(map.contains(&(Uuid::from_u128(1), Some(Uuid::from_u128(100)))));
        assert!(map.contains(&(Uuid::from_u128(2), None)));
    }

    /// The skeleton store is a **mirror** of `NodeRepo::SCOPE_PREDICATE` (extensibility.md §2), so
    /// this asserts the rules the SQL encodes rather than the Rust's own internal consistency:
    /// `None` filters nothing, a group set admits only its members, an ungrouped node is invisible
    /// to any scope, and an empty set matches nothing rather than everything.
    #[tokio::test]
    async fn the_skeleton_store_scopes_by_the_same_rules_as_the_sql_predicate() {
        let tokyo = Uuid::from_u128(100);
        let list = StaticNodeList(vec![
            node(1, "in-tokyo", "10.0.0.1", Some(100)),
            node(2, "ungrouped", "10.0.0.2", None),
            node(3, "in-osaka", "10.0.0.3", Some(200)),
        ]);

        // No filter ⇒ everything, including the ungrouped node.
        assert_eq!(list.count(None).await.unwrap(), 3);

        // A group set ⇒ only its members. The ungrouped node is NOT included — matching
        // `Scope::allows`, which never leaks an ungrouped node to a scoped principal.
        let only_tokyo = list.list_page(Some(&[tokyo]), None, 50).await.unwrap();
        assert_eq!(only_tokyo.len(), 1);
        assert_eq!(only_tokyo[0].name, "in-tokyo");
        assert_eq!(list.count(Some(&[tokyo])).await.unwrap(), 1);

        // The inversion that would be a privilege escalation: an empty set matches nothing.
        assert_eq!(list.count(Some(&[])).await.unwrap(), 0);
        assert!(list
            .search(Some(&[]), &w(""), None, None, 50)
            .await
            .unwrap()
            .is_empty());
        assert!(list.node_group_map(Some(&[])).await.unwrap().is_empty());

        // Scope is applied before the search term, not instead of it.
        let hits = list
            .search(Some(&[tokyo]), &w("in-"), None, None, 50)
            .await
            .unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].name, "in-tokyo");
    }

    #[tokio::test]
    async fn static_node_list_search_matches_name_or_address_ordered_and_capped() {
        let list = StaticNodeList(vec![
            node(1, "tokyo-edge", "10.0.0.1", None),
            node(2, "osaka-core", "10.0.0.2", None),
            node(3, "nagoya-edge", "192.168.5.9", None),
        ]);
        // Name substring, case-insensitive.
        assert_eq!(
            list.search(None, &w("EDGE"), None, None, 50)
                .await
                .unwrap()
                .len(),
            2
        );
        // Address substring.
        let by_addr = list
            .search(None, &w("192.168"), None, None, 50)
            .await
            .unwrap();
        assert_eq!(by_addr.len(), 1);
        assert_eq!(by_addr[0].name, "nagoya-edge");
        // Empty term returns all, ordered by name and capped.
        let capped = list.search(None, &w(""), None, None, 2).await.unwrap();
        assert_eq!(capped.len(), 2);
        assert_eq!(capped[0].name, "nagoya-edge"); // nagoya < osaka < tokyo
        assert_eq!(capped[1].name, "osaka-core");
    }

    #[tokio::test]
    async fn search_is_capped_by_the_shared_constant_not_a_local_literal() {
        let nodes: Vec<Node> = (0..600u32)
            .map(|i| node(u128::from(i) + 1, &format!("sw-{i:04}"), "10.0.0.1", None))
            .collect();
        let list = StaticNodeList(nodes);
        // A caller asking for one page gets one page — the repo does not quietly cut it shorter,
        // which was the original defect.
        let page = list
            .search(None, &w("sw-"), None, None, NODE_SEARCH_MAX)
            .await
            .unwrap();
        assert_eq!(page.len() as i64, NODE_SEARCH_MAX);
        // Asking for more than the scan ceiling yields exactly the ceiling, not some smaller
        // inner limit.
        let hits = list
            .search(None, &w("sw-"), None, None, NODE_SCAN_MAX * 2)
            .await
            .unwrap();
        assert_eq!(hits.len(), 600, "fewer nodes exist than the ceiling allows");
    }

    // ── Against a real database (ADR-114) ────────────────────────────────────────────────
    //
    // The test above asserts the rules the SQL encodes. These run the SQL.

    /// 🚨 **The two implementations answer the same question the same way.**
    ///
    /// [`StaticNodeList`] is a mirror of [`NodeRepo`]'s `SCOPE_PREDICATE` (extensibility.md §2),
    /// and until ADR-114 only the mirror could be executed. So the scope test above — which is
    /// named for the SQL — was checking the Rust's agreement with a *description* of the SQL, and
    /// the SQL half of an ADR-014 rule was enforced by reading.
    ///
    /// Both stores are given the identical fixture and asked the identical questions. The
    /// comparison is over ids, not rows: the two produce the same `Node` values by construction
    /// (one reads them, the other was built from them), so comparing whole rows would be
    /// comparing the fixture with itself.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn both_node_stores_scope_identically(pool: sqlx::PgPool) {
        let tokyo = crate::pgtest::group(&pool, "tokyo").await;
        let osaka = crate::pgtest::group(&pool, "osaka").await;
        let in_tokyo = crate::pgtest::node(&pool, "in-tokyo", 1, Some(tokyo)).await;
        let ungrouped = crate::pgtest::node(&pool, "ungrouped", 2, None).await;
        let in_osaka = crate::pgtest::node(&pool, "in-osaka", 3, Some(osaka)).await;

        let sql = crate::pgtest::repo(pool.clone());
        let memory = StaticNodeList(sql.list_nodes().await.expect("read the fixture back"));
        assert_eq!(memory.0.len(), 3, "the fixture did not land");

        for (label, scope) in [
            ("unrestricted", None),
            ("one group", Some(&[tokyo][..])),
            ("both groups", Some(&[tokyo, osaka][..])),
            // The inversion that would be a privilege escalation if the two disagreed.
            ("no groups", Some(&[][..])),
        ] {
            assert_eq!(
                sql.count(scope).await.unwrap(),
                memory.count(scope).await.unwrap(),
                "count disagrees with {label}"
            );
            assert_eq!(
                page_ids(&sql, scope).await,
                page_ids(&memory, scope).await,
                "list_page disagrees with {label}"
            );
            let mut a = sql.node_group_map(scope).await.unwrap();
            let mut b = memory.node_group_map(scope).await.unwrap();
            a.sort_unstable();
            b.sort_unstable();
            assert_eq!(a, b, "node_group_map disagrees with {label}");
            for term in ["", "in-", "10.0.0."] {
                let mut a: Vec<Uuid> = sql
                    .search(scope, &w(term), None, None, 50)
                    .await
                    .unwrap()
                    .iter()
                    .map(|n| n.id.as_uuid())
                    .collect();
                let mut b: Vec<Uuid> = memory
                    .search(scope, &w(term), None, None, 50)
                    .await
                    .unwrap()
                    .iter()
                    .map(|n| n.id.as_uuid())
                    .collect();
                a.sort_unstable();
                b.sort_unstable();
                assert_eq!(a, b, "search({term:?}) disagrees with {label}");
            }
        }

        // The acceptance side. Every assertion above is an equality, and two stores that both
        // returned nothing would satisfy all of them — which is exactly the shape of a scope
        // predicate that has stopped matching. So pin what the answers actually are.
        assert_eq!(sql.count(None).await.unwrap(), 3);
        assert_eq!(page_ids(&sql, Some(&[tokyo])).await, vec![in_tokyo]);
        assert!(
            !page_ids(&sql, Some(&[tokyo, osaka]))
                .await
                .contains(&ungrouped),
            "an ungrouped node must never reach a scoped caller"
        );
        assert!(page_ids(&sql, Some(&[osaka])).await.contains(&in_osaka));
        assert!(page_ids(&sql, Some(&[])).await.is_empty());
    }

    /// **`search` really is capped in SQL**, not only in the mirror.
    ///
    /// `guards.rs::the_search_cap_is_declared_once` reads both implementations' text and checks
    /// they clamp against the shared constant. What it cannot see is whether the clamped value
    /// reaches the `LIMIT`.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn the_sql_search_honours_the_limit_it_is_given(pool: sqlx::PgPool) {
        for i in 1..=5u8 {
            crate::pgtest::node(&pool, &format!("node-{i}"), i, None).await;
        }
        let sql = crate::pgtest::repo(pool.clone());
        assert_eq!(
            sql.search(None, &w(""), None, None, 50)
                .await
                .unwrap()
                .len(),
            5
        );
        assert_eq!(
            sql.search(None, &w(""), None, None, 2).await.unwrap().len(),
            2
        );
        // A caller asking for more than the shared cap gets the cap, not its own number.
        assert_eq!(
            sql.search(None, &w(""), None, None, NODE_SEARCH_MAX * 10)
                .await
                .unwrap()
                .len(),
            5
        );
    }

    /// **The address narrowing is an address comparison, in both stores** (ADR-139 Inc.2).
    ///
    /// The substring search cannot answer "who is at 10.0.0.1": it also matches `10.0.0.10`, and a
    /// name-ordered page capped at its limit can be full of those and miss the node actually there.
    /// So the check is two-sided — the near-miss address must not come back — and it runs the SQL
    /// and the skeleton mirror over the same fixture, IPv6 spelling included.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn the_address_filter_matches_the_address_and_not_a_substring_of_it(pool: sqlx::PgPool) {
        let one = crate::pgtest::node(&pool, "one", 1, None).await;
        let ten = crate::pgtest::node(&pool, "ten", 10, None).await;
        let v6 =
            crate::pgtest::node_at(&pool, "v6", "2001:db8::1".parse().expect("v6"), None).await;
        let sql = crate::pgtest::repo(pool.clone());
        let memory = StaticNodeList(sql.list_nodes().await.expect("read the fixture back"));

        for (addr, want) in [("10.0.0.1", one), ("10.0.0.10", ten), ("2001:DB8::1", v6)] {
            let addr: IpAddr = addr.parse().expect("addr");
            for (label, store) in [("sql", &sql as &dyn NodeListing), ("memory", &memory)] {
                let ids: Vec<Uuid> = store
                    .search(None, &w(""), Some(addr), None, 50)
                    .await
                    .unwrap()
                    .iter()
                    .map(|n| n.id.as_uuid())
                    .collect();
                assert_eq!(ids, vec![want], "{label} at {addr}");
            }
        }
        // Nobody there is an empty answer, not every node.
        let nobody: IpAddr = "192.0.2.1".parse().expect("addr");
        assert!(sql
            .search(None, &w(""), Some(nobody), None, 50)
            .await
            .unwrap()
            .is_empty());
        // And it narrows alongside the substring rather than replacing it.
        let one_addr: IpAddr = "10.0.0.1".parse().expect("addr");
        assert!(sql
            .search(None, &w("ten"), Some(one_addr), None, 50)
            .await
            .unwrap()
            .is_empty());
    }

    /// 🚨 **The tag narrowing keeps every node that may carry a wanted tag, in both stores**
    /// (ADR-265 Inc.2).
    ///
    /// The store cannot fold effective tags, so this is a superset the caller narrows again. What
    /// must hold is that nothing which could carry the tag is dropped: a node's own tag (also one
    /// stored with stray spaces, which the resolver counts by its trimmed form) and a node filed in
    /// a folder that supplies it. A case variant is a different tag and must not come back.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn both_node_stores_narrow_by_tag_identically(pool: sqlx::PgPool) {
        let site = crate::pgtest::group(&pool, "site").await;
        let other = crate::pgtest::group(&pool, "other").await;
        let own = crate::pgtest::node(&pool, "own", 1, None).await;
        let padded = crate::pgtest::node(&pool, "padded", 2, None).await;
        let in_site = crate::pgtest::node(&pool, "in-site", 3, Some(site)).await;
        let _cased = crate::pgtest::node(&pool, "cased", 4, None).await;
        let _elsewhere = crate::pgtest::node(&pool, "elsewhere", 5, Some(other)).await;
        for (name, tags) in [
            ("own", vec!["core", "wan"]),
            ("padded", vec![" core "]),
            ("cased", vec!["CORE"]),
        ] {
            sqlx::query("UPDATE nodes SET tags = $1 WHERE name = $2")
                .bind(&tags)
                .bind(name)
                .execute(&pool)
                .await
                .expect("tag the fixture");
        }
        let sql = crate::pgtest::repo(pool.clone());
        let memory = StaticNodeList(sql.list_nodes().await.expect("read the fixture back"));
        let narrowing = TagNarrowing {
            tags: vec!["core".to_owned()],
            folders: vec![site],
        };
        let mut want = vec![own, padded, in_site];
        want.sort_unstable();
        for (label, store) in [("sql", &sql as &dyn NodeListing), ("memory", &memory)] {
            let mut got: Vec<Uuid> = store
                .search(None, &[], None, Some(&narrowing), 50)
                .await
                .unwrap()
                .iter()
                .map(|n| n.id.as_uuid())
                .collect();
            got.sort_unstable();
            assert_eq!(got, want, "{label}");
            // It narrows alongside the words rather than replacing them.
            let with_word: Vec<Uuid> = store
                .search(None, &w("site"), None, Some(&narrowing), 50)
                .await
                .unwrap()
                .iter()
                .map(|n| n.id.as_uuid())
                .collect();
            assert_eq!(with_word, vec![in_site], "{label} with a word");
            // Nothing wanted is nothing, not everything.
            assert!(
                store
                    .search(None, &[], None, Some(&TagNarrowing::default()), 50)
                    .await
                    .unwrap()
                    .is_empty(),
                "{label} with an empty narrowing"
            );
        }
    }

    /// Ids a store answered, sorted.
    async fn ids(store: &dyn NodeListing, words: &[SearchWord]) -> Vec<Uuid> {
        let mut v: Vec<Uuid> = store
            .search(None, words, None, None, 50)
            .await
            .unwrap()
            .iter()
            .map(|n| n.id.as_uuid())
            .collect();
        v.sort_unstable();
        v
    }

    /// The skeleton mirror ANDs its words and honours a word's folders (ADR-258).
    #[tokio::test]
    async fn the_skeleton_store_needs_every_word() {
        let list = StaticNodeList(vec![
            node(1, "tokyo-core-01", "10.0.0.1", Some(100)),
            node(2, "tokyo-edge-01", "10.0.0.2", Some(100)),
            node(3, "osaka-core-01", "10.0.0.3", Some(200)),
        ]);
        let one = |n: u128| vec![Uuid::from_u128(n)];
        assert_eq!(ids(&list, &w("tokyo core")).await, one(1));
        assert_eq!(ids(&list, &w("core tokyo")).await, one(1));
        assert_eq!(ids(&list, &w("10.0.0.3 core")).await, one(3));
        assert!(ids(&list, &w("tokyo nothing")).await.is_empty());
        // "kanto" names folder 100, not a node: it matches through the folder.
        let words = vec![
            SearchWord {
                text: "kanto".into(),
                folders: vec![Uuid::from_u128(100)],
            },
            SearchWord::plain("edge"),
        ];
        assert_eq!(ids(&list, &words).await, one(2));
    }

    /// 🚨 **Every word has to match, in SQL** — and through each of the four things a word can
    /// match: the name, the monitored address, a port's address and a folder (ADR-258). The
    /// near-misses are what make it a test of AND rather than of OR.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn the_sql_search_needs_every_word(pool: sqlx::PgPool) {
        let tokyo = crate::pgtest::group(&pool, "tokyo").await;
        let core = crate::pgtest::node(&pool, "core-01", 1, Some(tokyo)).await;
        let edge = crate::pgtest::node(&pool, "edge-01", 2, Some(tokyo)).await;
        let elsewhere = crate::pgtest::node(&pool, "core-02", 3, None).await;
        let underscore = crate::pgtest::node(&pool, "sw_01", 4, None).await;
        let dash = crate::pgtest::node(&pool, "sw-01", 5, None).await;
        // A port address on `edge` that is nowhere in its name or monitored address.
        sqlx::query(
            "INSERT INTO node_l3 (node_id, l3_key, addresses, address_count) \
             VALUES ($1, 'v1', $2, 1)",
        )
        .bind(edge)
        .bind(serde_json::json!({
            "addresses": [{ "ifindex": 7, "ip": "198.51.100.17", "prefix_len": 24 }]
        }))
        .execute(&pool)
        .await
        .expect("insert a port address");
        let sql = crate::pgtest::repo(pool.clone());
        let sorted = |mut v: Vec<Uuid>| {
            v.sort_unstable();
            v
        };

        // Name AND name: both words, so edge-01 and core-02 each miss one.
        assert_eq!(ids(&sql, &w("core 01")).await, vec![core]);
        assert_eq!(ids(&sql, &w("sw 01")).await, sorted(vec![underscore, dash]));
        assert_eq!(ids(&sql, &w("core 02")).await, vec![elsewhere]);
        // Name AND monitored address.
        assert_eq!(ids(&sql, &w("core 10.0.0.1")).await, vec![core]);
        // A port's address, alone and beside a name.
        assert_eq!(ids(&sql, &w("198.51.100.17")).await, vec![edge]);
        assert_eq!(ids(&sql, &w("edge 198.51.100")).await, vec![edge]);
        assert!(ids(&sql, &w("core 198.51.100")).await.is_empty());
        // A folder the caller resolved, beside a name.
        let tokyo_core = vec![
            SearchWord {
                text: "tokyo".into(),
                folders: vec![tokyo],
            },
            SearchWord::plain("core"),
        ];
        assert_eq!(ids(&sql, &tokyo_core).await, vec![core]);
        // `_` is a character, not "any one character".
        assert_eq!(ids(&sql, &w("sw_01")).await, vec![underscore]);
        assert!(ids(&sql, &w("50%")).await.is_empty());

        // The mirror agrees wherever it can see (it has no port addresses).
        let memory = StaticNodeList(sql.list_nodes().await.expect("read the fixture back"));
        for term in ["core 01", "core 10.0.0.1", "sw_01", "edge"] {
            assert_eq!(
                ids(&sql, &w(term)).await,
                ids(&memory, &w(term)).await,
                "{term:?}"
            );
        }
        assert_eq!(
            ids(&sql, &tokyo_core).await,
            ids(&memory, &tokyo_core).await
        );
    }

    /// Ids of the first page, in the order the store returned them.
    async fn page_ids(store: &impl NodeListing, groups: GroupFilter<'_>) -> Vec<Uuid> {
        store
            .list_page(groups, None, 100)
            .await
            .unwrap()
            .iter()
            .map(|n| n.id.as_uuid())
            .collect()
    }
}
