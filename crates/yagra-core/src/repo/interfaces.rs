// SPDX-License-Identifier: AGPL-3.0-only
//! The `interfaces` table: what a node's ports are called, how fast they are, and the optical
//! thresholds their transceivers declare.
//!
//! ⚠️ The upsert is `COALESCE(EXCLUDED.col, interfaces.col)` on every column because the row has
//! **several writers** — the metadata walk, the optical probe and the MAU walk each fill different
//! columns of it. The direct consequence is that a poller which starts sending `NULL` leaves the
//! previous value in place forever: correcting a stored value takes the poller change **and** a
//! one-time `UPDATE` (`extensibility.md` §6, migrations 0086 and 0088).
//!
//! ⚠️ **It also writes only the rows that would actually change** (ADR-110 Increment 1). A table
//! walk re-reports every port every time and a live device changes none of these columns, so
//! writing unconditionally was 1,159,864 row updates per cycle at fleet scale and was what broke
//! the receiving side first. The clock column is on a lazy touch; [`INTERFACE_TOUCH_SECS`] carries
//! the invariant that makes that safe, and `repo/guards.rs` holds both it and the statement's
//! shape. **Nothing about which value ends up stored changed** — only how often a row is rewritten
//! with the value it already had.

use std::collections::{BTreeMap, HashMap};
use std::sync::LazyLock;

use serde::Serialize;
use sqlx::Row;
use uuid::Uuid;

// Only the settings struct: `retention::Row` would collide with `sqlx::Row` above.

use super::*;

/// An interface whose metadata has not been refreshed within this window is flagged stale. The UI
/// shows the row either way — a switch that stopped answering SNMP still has ports.
///
/// It lives here, not at either reader, because it is a fact about this table's `last_seen` column
/// and because it is one half of an invariant whose other half is [`INTERFACE_TOUCH_SECS`]: neither
/// number means anything alone. Both the REST edge and the MCP node-status tool read it, so a
/// second copy would let the two surfaces disagree about which interfaces are current (ADR-042 I4).
pub const INTERFACE_STALE_SECS: i64 = 900;

/// How stale a row's `last_seen` must be before a walk that changed **nothing** bothers to advance
/// it (ADR-110 Increment 1).
///
/// **Why this exists.** `last_seen = now()` is a different value on every poll, so writing it
/// unconditionally rewrote every polled port's row whether or not the device had said anything
/// new. Measured at 50,000 nodes × 24 ports that is **1,159,864 row updates per cycle**, and it was
/// the first thing to break the receiving side: the upsert held all twenty PostgreSQL connections
/// while a zero-row `SELECT` took 74.6 s. A live device changes none of the other columns —
/// `jpmyj01fw01` (16 ports, ten of them down) changed **0 of 16 rows** over five cycles, and only
/// `last_seen` moved, identically on all sixteen because the walk is per node.
///
/// 🚨 **The safety argument is the invariant `2 * INTERFACE_TOUCH_SECS < INTERFACE_STALE_SECS`,
/// and `repo/guards.rs` holds it.** A row is skipped only while it is newer than this window, which
/// can only happen on a node polled *faster* than the window; a node polled more slowly finds its
/// row already older on every poll and is written exactly as before, so it cannot regress. When a
/// skip does happen the row reaches at most `TOUCH + interval` old, and `interval < TOUCH` in that
/// branch, so at most `2 * TOUCH` — which the invariant keeps under the staleness threshold. A live
/// port therefore is never wrongly called stale, and a port that has genuinely left the walk still
/// ages out at `INTERFACE_STALE_SECS`, because nothing writes its row at all.
///
/// ⚠️ **Not the same as moving `last_seen` to node granularity.** That is cheaper still (one row
/// per node rather than one per port) and was rejected: a per-row `last_seen` is the only evidence
/// that a *particular* port was in the last walk, which is what makes a pulled line card visible.
/// The lazy touch keeps both properties.
pub const INTERFACE_TOUCH_SECS: i64 = 300;

/// The device-supplied columns of `interfaces` — every column the upsert COALESCEs.
///
/// `node_id` and `ifindex` are the key and `last_seen` is the row's clock, so neither is here.
/// Both the `DO UPDATE SET` list and the "would this row actually change" predicate are generated
/// from this array, so those two cannot disagree. What still can drift is a column added to the
/// INSERT list and forgotten here, which would leave that column **inserted once and never
/// updated** — a change the device reports and Yagra silently discards, with no compile error and
/// no runtime error. `repo/guards.rs` reads the generated statement back and refuses exactly that.
pub(super) const VALUE_COLUMNS: [&str; 11] = [
    "if_name",
    "if_alias",
    "if_speed",
    "if_duplex",
    "if_type",
    "if_media",
    "transceiver_model",
    "rx_power_low_dbm",
    "rx_power_high_dbm",
    "tx_power_low_dbm",
    "tx_power_high_dbm",
];

/// The one statement [`NodeRepo::upsert_interfaces_batch`] runs, built once.
///
/// ⚠️ **Adding a column still means four coordinated edits** — the INSERT list, the unnest cast
/// list, the `.bind()` and [`VALUE_COLUMNS`] — and only the first two are checked against each
/// other by Postgres. A `.bind()` in the wrong order is not a compile error and not a runtime
/// error; it silently writes one column's values into another. The `SET` list and the change
/// predicate used to be two further hand-written copies and are now generated, which is the whole
/// reason this is a `format!` rather than a literal.
pub(super) static UPSERT_SQL: LazyLock<String> = LazyLock::new(|| {
    // `COALESCE` on every column because the row has **several writers** — the metadata walk, the
    // optical probe and the MAU walk each fill different columns of it, and a walk that is
    // switched off must not blank what another one wrote.
    let set = VALUE_COLUMNS
        .iter()
        .map(|c| format!("{c} = COALESCE(EXCLUDED.{c}, interfaces.{c})"))
        .collect::<Vec<_>>()
        .join(", ");
    // Given that `SET`, "this row would change" is exactly "the walk said something about this
    // column, and what it said differs from what is stored". `IS DISTINCT FROM` rather than `<>`:
    // the stored side is nullable and `<>` would answer NULL, which a WHERE reads as false.
    let changed = VALUE_COLUMNS
        .iter()
        .map(|c| {
            format!("(EXCLUDED.{c} IS NOT NULL AND interfaces.{c} IS DISTINCT FROM EXCLUDED.{c})")
        })
        .collect::<Vec<_>>()
        .join(" OR ");
    format!(
        "INSERT INTO interfaces (node_id, ifindex, if_name, if_alias, if_speed, \
             if_duplex, if_type, if_media, transceiver_model, \
             rx_power_low_dbm, rx_power_high_dbm, tx_power_low_dbm, tx_power_high_dbm, \
             last_seen) \
         SELECT t.node_id, t.ifindex, t.if_name, t.if_alias, t.if_speed, \
             t.if_duplex, t.if_type, t.if_media, t.transceiver_model, \
             t.rx_power_low_dbm, t.rx_power_high_dbm, t.tx_power_low_dbm, \
             t.tx_power_high_dbm, now() \
         FROM unnest($1::uuid[], $2::int[], $3::text[], $4::text[], $5::int8[], \
                     $6::text[], $7::int[], $8::text[], $9::text[], \
                     $10::float8[], $11::float8[], $12::float8[], $13::float8[]) \
              AS t(node_id, ifindex, if_name, if_alias, if_speed, \
                   if_duplex, if_type, if_media, transceiver_model, \
                   rx_power_low_dbm, rx_power_high_dbm, tx_power_low_dbm, tx_power_high_dbm) \
         ON CONFLICT (node_id, ifindex) DO UPDATE SET {set}, last_seen = now() \
         WHERE {changed} \
            OR interfaces.last_seen < now() - interval '1 second' * $14::float8"
    )
});

/// One interface's stored metadata (from a table walk). Descriptive attributes only —
/// joined to per-interface metrics at query time (thin-label model, ADR-011).
///
/// `Serialize` is here so the support bundle can carry these rows **verbatim** (ADR-045 Inc.1)
/// rather than mirroring them into a second struct. The mirror was the obvious shape and is the
/// wrong one: a new column on `interfaces` would then have to be added in two places, and the
/// bundle — the artefact nobody reads until something is already broken — is exactly the copy
/// that would be forgotten. Serializing the repo type means the field names *are* the column
/// names, which is what a diagnostic reader wants anyway. It carries no `ToSchema`, so this does
/// not put the type into the published OpenAPI contract.
#[derive(Debug, Clone, Serialize)]
pub struct InterfaceMeta {
    pub ifindex: i32,
    pub if_name: Option<String>,
    pub if_alias: Option<String>,
    pub if_speed: Option<i64>,
    /// Negotiated duplex token (`half` / `full`), from EtherLike-MIB, Huawei's `hwEthernetDuplex`
    /// or MAU-MIB, in that precedence (ADR-063 Inc.1/Inc.3). `None` means "not known" and covers
    /// all three being absent, the port being down, and the agent's own `unknown(1)` alike — see
    /// migration 0085.
    pub if_duplex: Option<String>,
    /// `ifType` (IANAifType) as the raw integer, if the device reported one.
    pub if_type: Option<i32>,
    /// Canonical IEEE media designation (`1000BASE-T`), from the MAU walk — `ifMauType`,
    /// CISCO-STACK-MIB `portType`, Huawei `hwEthernetPortType` (copper only) or a pluggable's
    /// part string, in that precedence (ADR-063 Inc.2/Inc.4/Inc.7).
    pub if_media: Option<String>,
    /// The pluggable's vendor part string, verbatim. **Not a media type** — see migration 0087.
    pub transceiver_model: Option<String>,
    /// The transceiver's own acceptable power window, dBm (ADR-062 Inc.4). `None` on every
    /// interface that is not optical, and on optical ones whose dialect publishes no thresholds.
    pub rx_power_low_dbm: Option<f64>,
    /// See [`InterfaceMeta::rx_power_low_dbm`].
    pub rx_power_high_dbm: Option<f64>,
    /// See [`InterfaceMeta::rx_power_low_dbm`].
    pub tx_power_low_dbm: Option<f64>,
    /// See [`InterfaceMeta::rx_power_low_dbm`].
    pub tx_power_high_dbm: Option<f64>,
    /// `last_seen` as Unix seconds, for staleness checks.
    pub last_seen_s: Option<i64>,
}

/// Everything one poll learned about one interface, for [`NodeRepo::upsert_interfaces_batch`].
///
/// **Every `None` here means "leave whatever is stored alone"**, not "set it to null" — the upsert
/// COALESCEs each column. That is what lets two different probes write disjoint columns of the same
/// row: the interface-metadata walk fills the names and speed and leaves the optical window `None`,
/// the optical probe does the exact opposite, and neither erases the other's work.
///
/// ⚠️ **A struct rather than a tuple, and the four optical bounds are why.** They are four
/// `Option<f64>` in a row, so a transposed pair would compile silently and paint a receive window
/// around the transmit line — a band in the wrong place accuses a healthy link, which is worse than
/// no band at all. Naming them is the only thing that makes the call site checkable.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct InterfaceUpsert {
    pub ifindex: i32,
    pub if_name: Option<String>,
    pub if_alias: Option<String>,
    pub if_speed: Option<i64>,
    pub if_duplex: Option<String>,
    pub if_type: Option<i32>,
    pub if_media: Option<String>,
    pub transceiver_model: Option<String>,
    pub rx_power_low_dbm: Option<f64>,
    pub rx_power_high_dbm: Option<f64>,
    pub tx_power_low_dbm: Option<f64>,
    pub tx_power_high_dbm: Option<f64>,
}

/// One row for [`NodeRepo::upsert_interfaces_batch`]: which node, and what was learned.
pub type InterfaceBatchRow = (Uuid, InterfaceUpsert);

/// Interface identity for a fleet Top-N name join (no timestamp — just labels + speed).
pub struct InterfaceIdent {
    pub if_name: Option<String>,
    pub if_alias: Option<String>,
    pub if_speed: Option<i64>,
}

impl NodeRepo {
    /// Interfaces discovered on a node (metadata for the interfaces view), ordered by index.
    pub async fn list_interfaces(&self, node_id: Uuid) -> anyhow::Result<Vec<InterfaceMeta>> {
        let rows = sqlx::query(
            "SELECT ifindex, if_name, if_alias, if_speed, if_duplex, if_type, \
                    if_media, transceiver_model, \
                    rx_power_low_dbm, rx_power_high_dbm, tx_power_low_dbm, tx_power_high_dbm, \
                    extract(epoch FROM last_seen)::bigint AS last_seen_s \
             FROM interfaces WHERE node_id = $1 ORDER BY ifindex",
        )
        .bind(node_id)
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter()
            .map(|row| {
                Ok(InterfaceMeta {
                    ifindex: row.try_get("ifindex")?,
                    if_name: row.try_get("if_name")?,
                    if_alias: row.try_get("if_alias")?,
                    if_speed: row.try_get("if_speed")?,
                    if_duplex: row.try_get("if_duplex")?,
                    if_type: row.try_get("if_type")?,
                    if_media: row.try_get("if_media")?,
                    transceiver_model: row.try_get("transceiver_model")?,
                    rx_power_low_dbm: row.try_get("rx_power_low_dbm")?,
                    rx_power_high_dbm: row.try_get("rx_power_high_dbm")?,
                    tx_power_low_dbm: row.try_get("tx_power_low_dbm")?,
                    tx_power_high_dbm: row.try_get("tx_power_high_dbm")?,
                    last_seen_s: row.try_get("last_seen_s")?,
                })
            })
            .collect()
    }

    /// Interface identity (name/alias/speed) for every interface on the given node ids, keyed by
    /// `(node_id, ifindex)`. For joining a fleet interface Top-N (which carries only node UUID +
    /// ifindex from the TSDB, ADR-011) back to human-readable names. Over-fetches all interfaces
    /// of the few nodes in a Top-N result, then the caller filters by the exact pairs — one query.
    pub async fn interface_idents_for(
        &self,
        node_ids: &[Uuid],
    ) -> anyhow::Result<HashMap<(Uuid, i32), InterfaceIdent>> {
        if node_ids.is_empty() {
            return Ok(HashMap::new());
        }
        let rows = sqlx::query(
            "SELECT node_id, ifindex, if_name, if_alias, if_speed \
             FROM interfaces WHERE node_id = ANY($1)",
        )
        .bind(node_ids)
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter()
            .map(|row| {
                let node_id: Uuid = row.try_get("node_id")?;
                let ifindex: i32 = row.try_get("ifindex")?;
                Ok((
                    (node_id, ifindex),
                    InterfaceIdent {
                        if_name: row.try_get("if_name")?,
                        if_alias: row.try_get("if_alias")?,
                        if_speed: row.try_get("if_speed")?,
                    },
                ))
            })
            .collect()
    }

    /// The slowest usable interface speed in bits/sec, over the whole fleet or over `node_ids`.
    ///
    /// This is the denominator of the interface-utilisation evaluator's query floor (ADR-076): the
    /// smallest percentage any rule names, times this, is a rate below which no covered port can
    /// breach anything. Ports with no speed, a zero speed, or the saturated 32-bit sentinel are
    /// excluded — none of them is a rate, and including one would drag the floor to a value that
    /// admits everything (or, for the sentinel, one that is simply wrong).
    ///
    /// `None` when no covered port has a usable speed, which the caller reads as "evaluate
    /// nothing" rather than "evaluate everything" — the narrowing failure.
    ///
    /// An empty `node_ids` means the whole fleet, not "no nodes": the caller passes the set only
    /// when the rules in force are narrow enough to enumerate.
    pub async fn slowest_interface_speed_bps(
        &self,
        node_ids: &[Uuid],
    ) -> anyhow::Result<Option<i64>> {
        // `u32::MAX` is the "too fast to express" marker of the 32-bit gauge, not a rate
        // (ADR-063 decision 7). The poller stopped storing it, and migration 0086 cleared the rows
        // that already had it — this exclusion is the belt to that braces, because a single stale
        // row would otherwise be a plausible-looking 4.29 Gbps floor.
        let sql = if node_ids.is_empty() {
            "SELECT min(if_speed) AS slowest FROM interfaces              WHERE if_speed > 0 AND if_speed <> 4294967295"
        } else {
            "SELECT min(if_speed) AS slowest FROM interfaces              WHERE node_id = ANY($1) AND if_speed > 0 AND if_speed <> 4294967295"
        };
        let row = if node_ids.is_empty() {
            sqlx::query(sql).fetch_one(&self.pool).await?
        } else {
            sqlx::query(sql)
                .bind(node_ids)
                .fetch_one(&self.pool)
                .await?
        };
        Ok(row.try_get::<Option<i64>, _>("slowest")?)
    }

    /// Upsert interfaces for MANY nodes in one statement — the async ingest writer (ADR-025)
    /// coalesces many polls, so this must not fan out per node. Names/aliases are device-supplied
    /// metadata kept in PostgreSQL (joined to metrics at query time) — never TSDB labels (ADR-011).
    /// Rows are [`InterfaceBatchRow`]: `(node_id, ifindex, if_name, if_alias, if_speed)`. `unnest`
    /// binds arrays, so the row count is unbounded by the 65535-parameter ceiling. Dedups within the
    /// batch keeping the last occurrence per `(node_id, ifindex)` — `ON CONFLICT` cannot touch the
    /// same key twice in one statement.
    pub async fn upsert_interfaces_batch(&self, rows: &[InterfaceBatchRow]) -> anyhow::Result<()> {
        if rows.is_empty() {
            return Ok(());
        }
        let mut by_key: BTreeMap<(Uuid, i32), InterfaceUpsert> = BTreeMap::new();
        for (node, iface) in rows {
            by_key.insert((*node, iface.ifindex), iface.clone());
        }
        let n = by_key.len();
        let mut node_ids: Vec<Uuid> = Vec::with_capacity(n);
        let mut ifindexes: Vec<i32> = Vec::with_capacity(n);
        let mut names: Vec<Option<String>> = Vec::with_capacity(n);
        let mut aliases: Vec<Option<String>> = Vec::with_capacity(n);
        let mut speeds: Vec<Option<i64>> = Vec::with_capacity(n);
        let mut duplexes: Vec<Option<String>> = Vec::with_capacity(n);
        let mut if_types: Vec<Option<i32>> = Vec::with_capacity(n);
        let mut medias: Vec<Option<String>> = Vec::with_capacity(n);
        let mut models: Vec<Option<String>> = Vec::with_capacity(n);
        let mut rx_lows: Vec<Option<f64>> = Vec::with_capacity(n);
        let mut rx_highs: Vec<Option<f64>> = Vec::with_capacity(n);
        let mut tx_lows: Vec<Option<f64>> = Vec::with_capacity(n);
        let mut tx_highs: Vec<Option<f64>> = Vec::with_capacity(n);
        for ((node, _), iface) in by_key {
            node_ids.push(node);
            ifindexes.push(iface.ifindex);
            names.push(iface.if_name);
            aliases.push(iface.if_alias);
            speeds.push(iface.if_speed);
            duplexes.push(iface.if_duplex);
            if_types.push(iface.if_type);
            medias.push(iface.if_media);
            models.push(iface.transceiver_model);
            rx_lows.push(iface.rx_power_low_dbm);
            rx_highs.push(iface.rx_power_high_dbm);
            tx_lows.push(iface.tx_power_low_dbm);
            tx_highs.push(iface.tx_power_high_dbm);
        }
        // Only the rows that would actually change are written — see `INTERFACE_TOUCH_SECS` for
        // why the clock column is on a lazy touch and why that cannot make a live port look stale.
        let written = sqlx::query(&UPSERT_SQL)
            .bind(&node_ids)
            .bind(&ifindexes)
            .bind(&names)
            .bind(&aliases)
            .bind(&speeds)
            .bind(&duplexes)
            .bind(&if_types)
            .bind(&medias)
            .bind(&models)
            .bind(&rx_lows)
            .bind(&rx_highs)
            .bind(&tx_lows)
            .bind(&tx_highs)
            .bind(INTERFACE_TOUCH_SECS as f64)
            .execute(&self.pool)
            .await?
            .rows_affected();
        // What this fleet costs the table, permanently visible rather than only during a load
        // test. `skipped` is the whole point of ADR-110 Increment 1; `written` climbing to meet it
        // means either a fleet whose ports really are changing or a predicate that has stopped
        // matching, and neither is visible from a queue depth.
        let offered = n as u64;
        metrics::counter!("yagra_interface_upsert_rows_total", "outcome" => "written")
            .increment(written);
        metrics::counter!("yagra_interface_upsert_rows_total", "outcome" => "skipped")
            .increment(offered.saturating_sub(written));
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A walk that reports one port, filling only the two columns the metadata walk fills.
    fn walk(node: Uuid, ifindex: i32, name: &str, alias: Option<&str>) -> InterfaceBatchRow {
        (
            node,
            InterfaceUpsert {
                ifindex,
                if_name: Some(name.to_owned()),
                if_alias: alias.map(str::to_owned),
                if_speed: None,
                if_duplex: None,
                if_type: None,
                if_media: None,
                transceiver_model: None,
                rx_power_low_dbm: None,
                rx_power_high_dbm: None,
                tx_power_low_dbm: None,
                tx_power_high_dbm: None,
            },
        )
    }

    /// `last_seen` for one row, at full precision.
    ///
    /// [`InterfaceMeta::last_seen_s`] is Unix **seconds**, which cannot tell "not rewritten" from
    /// "rewritten twice within the same second" — the exact distinction every test below turns on.
    async fn last_seen(
        pool: &sqlx::PgPool,
        node: Uuid,
        ifindex: i32,
    ) -> chrono::DateTime<chrono::Utc> {
        sqlx::query_scalar("SELECT last_seen FROM interfaces WHERE node_id = $1 AND ifindex = $2")
            .bind(node)
            .bind(ifindex)
            .fetch_one(pool)
            .await
            .expect("read last_seen")
    }

    /// Move one row's clock back, as if the walk had last touched it `secs` ago.
    async fn age(pool: &sqlx::PgPool, node: Uuid, ifindex: i32, secs: i64) {
        sqlx::query(
            "UPDATE interfaces SET last_seen = now() - interval '1 second' * $3 \
             WHERE node_id = $1 AND ifindex = $2",
        )
        .bind(node)
        .bind(ifindex)
        .bind(secs as f64)
        .execute(pool)
        .await
        .expect("age the row");
    }

    /// 🚨 **A second walk reporting the same thing writes nothing** (ADR-110 Increment 1).
    ///
    /// A table walk re-reports every port every time and a live device changes none of these
    /// columns, so writing unconditionally was 1,159,864 row updates per cycle at fleet scale and
    /// was what broke the receiving side first. The saving was measured on hardware (5.02×); what
    /// was never checked anywhere is the statement itself, and the `WHERE` clause that carries it
    /// is assembled from a `LazyLock<String>` at runtime.
    ///
    /// "Nothing was written" is read off `last_seen`, because the clock column is on the same
    /// lazy touch: a row the `WHERE` excluded keeps the timestamp it had.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn a_walk_that_reports_no_change_rewrites_no_row(pool: sqlx::PgPool) {
        let node = crate::pgtest::node(&pool, "sw1", 1, None).await;
        let repo = crate::pgtest::repo(pool.clone());
        let rows = vec![
            walk(node, 1, "Gi0/1", Some("uplink")),
            walk(node, 2, "Gi0/2", Some("access")),
        ];

        repo.upsert_interfaces_batch(&rows).await.expect("first");
        let first = (
            last_seen(&pool, node, 1).await,
            last_seen(&pool, node, 2).await,
        );

        repo.upsert_interfaces_batch(&rows).await.expect("second");
        let second = (
            last_seen(&pool, node, 1).await,
            last_seen(&pool, node, 2).await,
        );

        assert_eq!(first, second, "an unchanged walk rewrote its rows");
        // The acceptance side: a statement that had stopped inserting anything would make the two
        // reads fail rather than pass, but one that had stopped *updating* would satisfy the
        // equality above forever. The next test is the other half.
        assert_eq!(repo.list_interfaces(node).await.unwrap().len(), 2);
    }

    /// **A changed column is written, and only its own row is.**
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn only_the_row_that_changed_is_rewritten(pool: sqlx::PgPool) {
        let node = crate::pgtest::node(&pool, "sw1", 1, None).await;
        let repo = crate::pgtest::repo(pool.clone());
        repo.upsert_interfaces_batch(&[
            walk(node, 1, "Gi0/1", Some("uplink")),
            walk(node, 2, "Gi0/2", Some("access")),
        ])
        .await
        .expect("first");
        let before = (
            last_seen(&pool, node, 1).await,
            last_seen(&pool, node, 2).await,
        );

        repo.upsert_interfaces_batch(&[
            walk(node, 1, "Gi0/1", Some("uplink-2")),
            walk(node, 2, "Gi0/2", Some("access")),
        ])
        .await
        .expect("second");

        let after = (
            last_seen(&pool, node, 1).await,
            last_seen(&pool, node, 2).await,
        );
        assert!(after.0 > before.0, "the changed row was not rewritten");
        assert_eq!(
            after.1, before.1,
            "an unchanged row was rewritten beside it"
        );

        let stored = repo.list_interfaces(node).await.unwrap();
        let alias = |ifindex| {
            stored
                .iter()
                .find(|i| i.ifindex == ifindex)
                .and_then(|i| i.if_alias.clone())
        };
        assert_eq!(alias(1).as_deref(), Some("uplink-2"));
        assert_eq!(alias(2).as_deref(), Some("access"));
    }

    /// ⚠️ **A writer that says nothing about a column must not blank it.**
    ///
    /// The row has several writers — the metadata walk, the optical probe and the MAU walk each
    /// fill different columns — so every column is `COALESCE(EXCLUDED.c, interfaces.c)`. The
    /// documented consequence is real and worth pinning from both sides: a walk sending `NULL`
    /// leaves the previous value in place *forever*, which is why correcting a stored value takes
    /// a poller change **and** a one-time `UPDATE` (migrations 0086 and 0088).
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn a_writer_with_nothing_to_say_about_a_column_leaves_it_alone(pool: sqlx::PgPool) {
        let node = crate::pgtest::node(&pool, "sw1", 1, None).await;
        let repo = crate::pgtest::repo(pool.clone());

        // The optical probe fills the power window and nothing else.
        let mut optical = walk(node, 1, "Gi0/1", Some("uplink"));
        optical.1.rx_power_low_dbm = Some(-9.5);
        repo.upsert_interfaces_batch(&[optical])
            .await
            .expect("optical");

        // The metadata walk then reports the same port, knowing nothing about optics.
        repo.upsert_interfaces_batch(&[walk(node, 1, "Gi0/1", Some("uplink-2"))])
            .await
            .expect("metadata");

        let stored = repo.list_interfaces(node).await.unwrap();
        let row = stored.iter().find(|i| i.ifindex == 1).expect("the row");
        assert_eq!(
            row.if_alias.as_deref(),
            Some("uplink-2"),
            "the change landed"
        );
        assert_eq!(
            row.rx_power_low_dbm,
            Some(-9.5),
            "the other writer's column was blanked"
        );
    }

    /// **The lazy clock touch fires only once the row is old enough** — both directions.
    ///
    /// This is the half of ADR-110 Increment 1 that keeps "write only what changed" from making a
    /// live port look stale: `last_seen` is still advanced by a walk that changed nothing, just
    /// not on every cycle. [`INTERFACE_TOUCH_SECS`] is the boundary, and the invariant
    /// `2 * INTERFACE_TOUCH_SECS < INTERFACE_STALE_SECS` is what makes the lag safe.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn an_unchanged_row_is_touched_only_once_it_is_old_enough(pool: sqlx::PgPool) {
        let node = crate::pgtest::node(&pool, "sw1", 1, None).await;
        let repo = crate::pgtest::repo(pool.clone());
        let rows = vec![walk(node, 1, "Gi0/1", Some("uplink"))];
        repo.upsert_interfaces_batch(&rows).await.expect("insert");

        // Not yet old enough: still skipped.
        age(&pool, node, 1, INTERFACE_TOUCH_SECS / 2).await;
        let young = last_seen(&pool, node, 1).await;
        repo.upsert_interfaces_batch(&rows).await.expect("young");
        assert_eq!(
            last_seen(&pool, node, 1).await,
            young,
            "a row inside the touch window was rewritten"
        );

        // Past the window: touched, even though nothing about it changed.
        age(&pool, node, 1, INTERFACE_TOUCH_SECS + 60).await;
        let old = last_seen(&pool, node, 1).await;
        repo.upsert_interfaces_batch(&rows).await.expect("old");
        assert!(
            last_seen(&pool, node, 1).await > old,
            "a row past the touch window was never refreshed — every port would go stale"
        );
    }
}
