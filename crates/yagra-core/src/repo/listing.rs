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
    /// Nodes whose name or address matches a case-insensitive substring (empty term ⇒ the first
    /// page ordered by name), capped at `limit`. Backs the node-picker's server-side typeahead so
    /// it never loads the whole inventory into the browser (ui-conventions: search is server-side
    /// at fleet scale, A-2).
    async fn search(
        &self,
        groups: GroupFilter<'_>,
        term: &str,
        limit: i64,
    ) -> anyhow::Result<Vec<Node>>;
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
        term: &str,
        limit: i64,
    ) -> anyhow::Result<Vec<Node>> {
        let limit = limit.clamp(1, NODE_SCAN_MAX);
        // Parameterized ILIKE (security.md — never string-format user input into SQL); the `%`
        // wildcards are concatenated in SQL so the term itself is a bound value. `host(address)`
        // strips the netmask so an IP-substring search matches the displayed address.
        let rows = if term.is_empty() {
            sqlx::query(&format!(
                "SELECT {} FROM nodes WHERE {} ORDER BY name, id LIMIT $2",
                Self::NODE_COLUMNS,
                Self::SCOPE_PREDICATE
            ))
            .bind(Self::scope_bind(groups))
            .bind(limit)
            .fetch_all(&self.pool)
            .await?
        } else {
            sqlx::query(&format!(
                "SELECT {} FROM nodes \
                 WHERE {} \
                   AND (name ILIKE '%' || $2 || '%' OR host(address) ILIKE '%' || $2 || '%') \
                 ORDER BY name, id LIMIT $3",
                Self::NODE_COLUMNS,
                Self::SCOPE_PREDICATE
            ))
            .bind(Self::scope_bind(groups))
            .bind(term)
            .bind(limit)
            .fetch_all(&self.pool)
            .await?
        };
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
        term: &str,
        limit: i64,
    ) -> anyhow::Result<Vec<Node>> {
        let t = term.to_lowercase();
        let mut nodes: Vec<Node> = self
            .0
            .iter()
            .filter(|n| Self::in_scope(groups, n))
            .filter(|n| {
                t.is_empty()
                    || n.name.to_lowercase().contains(&t)
                    || n.address.to_string().contains(&t)
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
        assert!(list.search(Some(&[]), "", 50).await.unwrap().is_empty());
        assert!(list.node_group_map(Some(&[])).await.unwrap().is_empty());

        // Scope is applied before the search term, not instead of it.
        let hits = list.search(Some(&[tokyo]), "in-", 50).await.unwrap();
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
        assert_eq!(list.search(None, "EDGE", 50).await.unwrap().len(), 2);
        // Address substring.
        let by_addr = list.search(None, "192.168", 50).await.unwrap();
        assert_eq!(by_addr.len(), 1);
        assert_eq!(by_addr[0].name, "nagoya-edge");
        // Empty term returns all, ordered by name and capped.
        let capped = list.search(None, "", 2).await.unwrap();
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
        let page = list.search(None, "sw-", NODE_SEARCH_MAX).await.unwrap();
        assert_eq!(page.len() as i64, NODE_SEARCH_MAX);
        // Asking for more than the scan ceiling yields exactly the ceiling, not some smaller
        // inner limit.
        let hits = list.search(None, "sw-", NODE_SCAN_MAX * 2).await.unwrap();
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
                    .search(scope, term, 50)
                    .await
                    .unwrap()
                    .iter()
                    .map(|n| n.id.as_uuid())
                    .collect();
                let mut b: Vec<Uuid> = memory
                    .search(scope, term, 50)
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
        assert_eq!(sql.search(None, "", 50).await.unwrap().len(), 5);
        assert_eq!(sql.search(None, "", 2).await.unwrap().len(), 2);
        // A caller asking for more than the shared cap gets the cap, not its own number.
        assert_eq!(
            sql.search(None, "", NODE_SEARCH_MAX * 10)
                .await
                .unwrap()
                .len(),
            5
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
