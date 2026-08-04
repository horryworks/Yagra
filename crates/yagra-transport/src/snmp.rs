// SPDX-License-Identifier: AGPL-3.0-only
//! SNMP v2c GET + GETBULK table walk over a pure-Rust client (`csnmp`) — ADR-021 PoC.
//!
//! This validates the pure-Rust path before any net-snmp FFI fallback. v3 (auth/priv)
//! is still pending. Values are returned **raw** (counters included) — rates are derived
//! at query time (ADR-012). Live-only (needs a device + UDP); the numeric mapping and
//! ifIndex extraction are unit-tested.

use crate::{
    SnmpInstanceRow, SnmpSample, SnmpTableSample, SnmpTableString, SnmpValue, TransportError,
};
use csnmp::{ObjectIdentifier, ObjectValue, Snmp2cClient};
use std::net::{IpAddr, SocketAddr};
use std::time::Duration;

/// Standard SNMP agent port.
const SNMP_PORT: u16 = 161;

/// GETBULK max-repetitions per request. Bounded so a huge table is paged, not pulled in
/// one oversized PDU; `csnmp::walk_bulk` repeats internally until the column is exhausted.
const WALK_MAX_REPETITIONS: u32 = 20;

/// Fetch `oids` from `target` via SNMP v2c. Per-OID failures are logged and skipped so a
/// single bad OID doesn't fail the whole poll.
pub async fn snmp_get_v2c(
    target: IpAddr,
    community: &str,
    oids: &[String],
    timeout: Duration,
) -> Result<Vec<SnmpSample>, TransportError> {
    let client = connect(target, community, timeout).await?;

    let mut samples = Vec::with_capacity(oids.len());
    for oid_str in oids {
        let oid = match parse_oid(oid_str) {
            Some(o) => o,
            None => {
                tracing::warn!(%oid_str, "skipping malformed OID");
                continue;
            }
        };
        match client.get(oid).await {
            Ok(value) => {
                if let Some(v) = numeric(&value) {
                    samples.push(SnmpSample {
                        oid: oid_str.clone(),
                        value: v,
                    });
                }
            }
            Err(e) => tracing::debug!(%oid_str, error = %e, "snmp get failed"),
        }
    }
    Ok(samples)
}

/// Walk numeric table columns from `target` via GETBULK. Each column base yields one row per
/// instance: a single trailing sub-identifier is the row key directly, while a multi-part index
/// is folded to a synthetic key (see [`ifindex_of`]) so multi-index tables — vendor memory
/// (HUAWEI-MEMORY-MIB `hwMemoryDevTable`), BGP4-MIB peers, … — are collected too. A per-column
/// walk failure is logged and skipped. Counters are returned **raw** (rates derived at query
/// time, ADR-012).
pub async fn snmp_walk_v2c(
    target: IpAddr,
    community: &str,
    column_oids: &[String],
    timeout: Duration,
) -> Result<Vec<SnmpTableSample>, TransportError> {
    let client = connect(target, community, timeout).await?;
    let mut rows = Vec::new();
    for base_str in column_oids {
        let Some(base) = parse_oid(base_str) else {
            tracing::warn!(%base_str, "skipping malformed table column OID");
            continue;
        };
        match client.walk_bulk(base, WALK_MAX_REPETITIONS).await {
            Ok(entries) => {
                for (oid, value) in entries {
                    if let (Some(ifindex), Some(v)) = (ifindex_of(&oid, &base), numeric(&value)) {
                        rows.push(SnmpTableSample {
                            oid_base: base_str.clone(),
                            ifindex,
                            value: v,
                        });
                    }
                }
            }
            Err(e) => tracing::debug!(%base_str, error = %e, "snmp table walk failed"),
        }
    }
    Ok(rows)
}

/// Walk string table columns (e.g. `ifName`, `ifAlias`) for interface metadata. Same
/// per-column skip-on-error behaviour as [`snmp_walk_v2c`]; non-string values are skipped.
pub async fn snmp_walk_strings_v2c(
    target: IpAddr,
    community: &str,
    column_oids: &[String],
    timeout: Duration,
) -> Result<Vec<SnmpTableString>, TransportError> {
    let client = connect(target, community, timeout).await?;
    let mut rows = Vec::new();
    for base_str in column_oids {
        let Some(base) = parse_oid(base_str) else {
            tracing::warn!(%base_str, "skipping malformed table column OID");
            continue;
        };
        match client.walk_bulk(base, WALK_MAX_REPETITIONS).await {
            Ok(entries) => {
                for (oid, value) in entries {
                    if let (Some(ifindex), Some(s)) =
                        (ifindex_of(&oid, &base), string_value(&value))
                    {
                        rows.push(SnmpTableString {
                            oid_base: base_str.clone(),
                            ifindex,
                            value: s,
                        });
                    }
                }
            }
            Err(e) => tracing::debug!(%base_str, error = %e, "snmp string table walk failed"),
        }
    }
    Ok(rows)
}

/// Walk table columns keeping each row's **full instance index** and **raw** value (ADR-038).
///
/// The two walkers above each collapse something on purpose — non-numeric values, or the
/// multi-part index — and both losses are fatal for adjacency data: `lldpRemTable` is indexed by
/// `(lldpRemTimeMark, lldpRemLocalPortNum, lldpRemIndex)` and a chassis id is typed octets. Same
/// per-column skip-on-error behaviour as the others; a value type this build has no representation
/// for is skipped rather than coerced.
/// `max_rows` bounds the **whole** call, across every column, and it is enforced *during* paging
/// rather than by truncating the result. That distinction is the point: memory is consumed while
/// the pages arrive, so a post-hoc truncation of a 400,000-row ARP table has already cost the
/// allocation it was meant to prevent. ADR-043 Increment 3 walks `ipNetToPhysicalPhysAddress` on
/// devices where that number is real.
pub async fn snmp_walk_instances_v2c(
    target: IpAddr,
    community: &str,
    column_oids: &[String],
    timeout: Duration,
    max_rows: usize,
) -> Result<Vec<SnmpInstanceRow>, TransportError> {
    let client = connect(target, community, timeout).await?;
    let mut rows = Vec::new();
    for base_str in column_oids {
        let Some(base) = parse_oid(base_str) else {
            tracing::warn!(%base_str, "skipping malformed table column OID");
            continue;
        };
        if rows.len() >= max_rows {
            tracing::debug!(%base_str, max_rows, "instance walk budget spent; skipping column");
            break;
        }
        walk_column_capped(&client, base_str, &base, max_rows - rows.len(), &mut rows).await;
    }
    Ok(rows)
}

/// Page one column with GETBULK, stopping at the subtree edge, at end-of-MIB, or at `budget` rows.
///
/// Hand-rolled rather than `Snmp2cClient::walk_bulk` because that collects the entire subtree before
/// returning — there is no point at which a caller can say "enough". The paging *decisions* live in
/// [`page_slice`], which is pure and tested; this function is the I/O around it.
///
/// A column that errors or times out ends here and only here, as in the other walkers: one bad
/// column must not fail the whole poll.
async fn walk_column_capped(
    client: &Snmp2cClient,
    base_str: &str,
    base: &ObjectIdentifier,
    budget: usize,
    out: &mut Vec<SnmpInstanceRow>,
) {
    // Defensive ceiling on requests as well as rows: an agent that answers with OIDs that do not
    // advance would otherwise spin forever, and `budget` alone cannot catch that because such a
    // page yields no rows either.
    const MAX_REQUESTS: usize = 4096;
    let mut cursor = *base;
    let mut taken = 0usize;
    for _ in 0..MAX_REQUESTS {
        let page = match client.get_bulk(&[cursor], 0, WALK_MAX_REPETITIONS).await {
            Ok(p) => p,
            Err(e) => {
                tracing::debug!(%base_str, error = %e, "snmp instance walk failed");
                return;
            }
        };
        // `GetBulkResult::values` is a `BTreeMap`, so it already arrives in OID order — which is
        // what "leading entries" in `page_slice` means. Materialized as a slice (≤
        // `WALK_MAX_REPETITIONS` entries) so the paging decision stays a pure function over one.
        let entries: Vec<(ObjectIdentifier, ObjectValue)> = page.values.into_iter().collect();
        let (take, next) = page_slice(base, &entries, budget - taken);
        for (oid, value) in entries.iter().take(take) {
            let Some(tail) = oid.relative_to(base) else {
                continue;
            };
            let instance = tail.as_slice().to_vec();
            if instance.is_empty() {
                continue; // the column base itself: no instance
            }
            out.push(SnmpInstanceRow {
                oid_base: base_str.to_owned(),
                instance,
                value: raw_value(value),
            });
        }
        taken += take;
        match next {
            // `next != cursor` guards the non-advancing agent; GETBULK is specified to return
            // strictly greater OIDs, but a buggy one that repeats itself must not loop us.
            Some(n) if !page.end_of_mib_view && n != cursor => cursor = n,
            _ => return,
        }
    }
    tracing::debug!(%base_str, "instance walk hit its request ceiling");
}

/// How much of one GETBULK page this walk may keep, and where to continue from.
///
/// Returns the number of **leading** entries that are inside `base`'s subtree and within `budget`,
/// plus the OID to continue after — `None` meaning stop. Leading, because a page that leaves the
/// subtree ends the walk: everything after that point belongs to another table.
///
/// Pure so the four stop conditions can be tested without an agent. They are the whole correctness
/// of a bounded walk, and three of them (budget, subtree edge, empty page) are silent when wrong —
/// the walk simply returns less, or more, than it should.
fn page_slice(
    base: &ObjectIdentifier,
    page: &[(ObjectIdentifier, ObjectValue)],
    budget: usize,
) -> (usize, Option<ObjectIdentifier>) {
    let mut take = 0usize;
    for (oid, _) in page {
        if !base.is_prefix_of_or_equal(oid) {
            // Out of subtree: keep what came before it and stop for good.
            return (take, None);
        }
        if take == budget {
            return (take, None);
        }
        take += 1;
    }
    // The whole page was in-subtree and affordable. Continue after its last OID — unless the budget
    // is now spent, in which case there is nothing more to fetch.
    if take == budget {
        return (take, None);
    }
    (take, page.last().map(|(oid, _)| *oid))
}

/// Map an SNMP value onto [`SnmpValue`] without coercing.
///
/// Total, and matched variant-by-variant rather than with a wildcard: this is the walker that must
/// not silently drop a column, so a value type gaining a representation should be a compile error
/// here rather than a row that quietly disappears from an operator's neighbour table.
pub(crate) fn raw_value(value: &ObjectValue) -> SnmpValue {
    match value {
        ObjectValue::Integer(i) => SnmpValue::Int(i64::from(*i)),
        ObjectValue::Counter32(c) | ObjectValue::Unsigned32(c) | ObjectValue::TimeTicks(c) => {
            SnmpValue::Int(i64::from(*c))
        }
        // Saturate rather than wrap: a negative value would be read as a different subtype.
        ObjectValue::Counter64(c) => SnmpValue::Int(i64::try_from(*c).unwrap_or(i64::MAX)),
        ObjectValue::String(bytes) | ObjectValue::Opaque(bytes) => SnmpValue::Bytes(bytes.clone()),
        // Kept as octets so `render_bare_address` reads it the same way it reads a CDP address.
        ObjectValue::IpAddress(ip) => SnmpValue::Bytes(ip.octets().to_vec()),
        ObjectValue::ObjectId(oid) => SnmpValue::Oid(oid.to_string()),
    }
}

/// Open an SNMP v2c client to `target`.
async fn connect(
    target: IpAddr,
    community: &str,
    timeout: Duration,
) -> Result<Snmp2cClient, TransportError> {
    let addr = SocketAddr::new(target, SNMP_PORT);
    Snmp2cClient::new(addr, community.as_bytes().to_vec(), None, Some(timeout))
        .await
        .map_err(|e| TransportError::Io(format!("snmp connect {addr}: {e}")))
}

/// Row key for an instance OID relative to its column `base` — the numeric identity of a row,
/// delegated to the shared [`crate::ifindex_from_tail`] so the v2c and v3 walkers key rows
/// identically. Returns `None` when the OID isn't under `base`, or is `base` itself (no instance).
fn ifindex_of(oid: &ObjectIdentifier, base: &ObjectIdentifier) -> Option<u32> {
    let tail = oid.relative_to(base)?;
    crate::ifindex_from_tail(tail.as_slice())
}

/// Map an SNMP string-ish value to a Rust `String`: `OCTET STRING` (lossy UTF-8) or
/// `OBJECT IDENTIFIER` (dotted decimal — e.g. `sysObjectID`). Other value types yield
/// `None`. Device-supplied — callers must treat the result as untrusted.
fn string_value(value: &ObjectValue) -> Option<String> {
    match value {
        ObjectValue::String(bytes) => Some(String::from_utf8_lossy(bytes).into_owned()),
        ObjectValue::ObjectId(oid) => Some(oid.to_string()),
        _ => None,
    }
}

/// Parse a dotted OID string into a `csnmp` [`ObjectIdentifier`].
fn parse_oid(s: &str) -> Option<ObjectIdentifier> {
    let parts: Vec<u32> = s
        .split('.')
        .map(|p| p.parse().ok())
        .collect::<Option<_>>()?;
    ObjectIdentifier::try_from(parts.as_slice()).ok()
}

/// Map a numeric SNMP value to `f64`; non-numeric values yield `None` (skipped).
#[allow(clippy::cast_precision_loss)]
fn numeric(value: &ObjectValue) -> Option<f64> {
    match value {
        ObjectValue::Integer(i) => Some(*i as f64),
        ObjectValue::Counter32(c) => Some(f64::from(*c)),
        ObjectValue::Unsigned32(u) => Some(f64::from(*u)),
        ObjectValue::TimeTicks(t) => Some(f64::from(*t)),
        ObjectValue::Counter64(c) => Some(*c as f64),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Bounded instance walk (ADR-043 I3) ───────────────────────────────────
    //
    // The cap exists because `ipNetToPhysicalPhysAddress` on a campus router is a real
    // hundred-thousand-row table, and every one of these stop conditions fails *silently* when it
    // is wrong: the walk just returns fewer rows, or keeps paging past its budget, and nothing
    // downstream can tell.

    fn oid(s: &str) -> ObjectIdentifier {
        parse_oid(s).expect("test oid")
    }

    fn page(oids: &[&str]) -> Vec<(ObjectIdentifier, ObjectValue)> {
        oids.iter()
            .map(|s| (oid(s), ObjectValue::Integer(1)))
            .collect()
    }

    #[test]
    fn a_full_in_subtree_page_is_taken_and_continues_after_its_last_oid() {
        let base = oid("1.3.6.1.2.1.4.35.1.4");
        let p = page(&[
            "1.3.6.1.2.1.4.35.1.4.1",
            "1.3.6.1.2.1.4.35.1.4.2",
            "1.3.6.1.2.1.4.35.1.4.3",
        ]);
        let (take, next) = page_slice(&base, &p, 100);
        assert_eq!(take, 3);
        assert_eq!(next, Some(oid("1.3.6.1.2.1.4.35.1.4.3")));
    }

    #[test]
    fn a_page_that_leaves_the_subtree_keeps_only_what_came_before_and_stops() {
        // GETBULK walks off the end of a column into the next one. Everything past that boundary
        // belongs to another table, and continuing would attribute its rows to this column.
        let base = oid("1.3.6.1.2.1.4.35.1.4");
        let p = page(&[
            "1.3.6.1.2.1.4.35.1.4.1",
            "1.3.6.1.2.1.4.35.1.5.1", // next column
            "1.3.6.1.2.1.4.35.1.5.2",
        ]);
        let (take, next) = page_slice(&base, &p, 100);
        assert_eq!(take, 1);
        assert_eq!(next, None, "leaving the subtree ends the walk for good");
    }

    #[test]
    fn the_budget_stops_the_walk_mid_page_rather_than_after_it() {
        // The property the whole change exists for. Taking the page and truncating afterwards has
        // already paid for the rows it meant to refuse.
        let base = oid("1.3.6.1.2.1.4.35.1.4");
        let p = page(&[
            "1.3.6.1.2.1.4.35.1.4.1",
            "1.3.6.1.2.1.4.35.1.4.2",
            "1.3.6.1.2.1.4.35.1.4.3",
        ]);
        let (take, next) = page_slice(&base, &p, 2);
        assert_eq!(take, 2);
        assert_eq!(next, None);
    }

    #[test]
    fn a_page_that_exactly_spends_the_budget_does_not_ask_for_another() {
        let base = oid("1.3.6.1.2.1.4.35.1.4");
        let p = page(&["1.3.6.1.2.1.4.35.1.4.1", "1.3.6.1.2.1.4.35.1.4.2"]);
        let (take, next) = page_slice(&base, &p, 2);
        assert_eq!(take, 2);
        assert_eq!(next, None, "the next request could only be refused anyway");
    }

    #[test]
    fn a_zero_budget_takes_nothing() {
        let base = oid("1.3.6.1.2.1.4.35.1.4");
        assert_eq!(
            page_slice(&base, &page(&["1.3.6.1.2.1.4.35.1.4.1"]), 0),
            (0, None)
        );
    }

    #[test]
    fn an_empty_page_stops_instead_of_looping() {
        // An agent that answers with nothing must end the walk; continuing from an unchanged cursor
        // is the infinite loop the request ceiling exists as a second guard against.
        let base = oid("1.3.6.1.2.1.4.35.1.4");
        assert_eq!(page_slice(&base, &[], 100), (0, None));
    }

    #[test]
    fn the_column_base_itself_is_still_paged_past() {
        // `is_prefix_of_or_equal` means the base can appear in a page. It carries no instance, so
        // the row is dropped later — but it must not end the walk, or a column would return empty.
        let base = oid("1.3.6.1.2.1.4.35.1.4");
        let p = page(&["1.3.6.1.2.1.4.35.1.4", "1.3.6.1.2.1.4.35.1.4.1"]);
        let (take, next) = page_slice(&base, &p, 100);
        assert_eq!(take, 2);
        assert_eq!(next, Some(oid("1.3.6.1.2.1.4.35.1.4.1")));
    }

    #[test]
    fn parses_valid_oid_and_rejects_garbage() {
        assert!(parse_oid("1.3.6.1.2.1.1.3.0").is_some());
        assert!(parse_oid("1.3.x.1").is_none());
        assert!(parse_oid("").is_none());
    }

    #[test]
    fn maps_numeric_values_and_skips_others() {
        assert_eq!(numeric(&ObjectValue::Counter64(1_000)), Some(1_000.0));
        assert_eq!(numeric(&ObjectValue::Integer(-5)), Some(-5.0));
        assert_eq!(numeric(&ObjectValue::TimeTicks(42)), Some(42.0));
        assert_eq!(numeric(&ObjectValue::String(vec![1, 2, 3])), None);
    }

    #[test]
    fn ifindex_uses_single_subid_directly_and_folds_multi_index() {
        let base = parse_oid("1.3.6.1.2.1.31.1.1.1.6").unwrap();
        // base + .7 → ifIndex 7 (single trailing sub-id, used as-is).
        let instance = parse_oid("1.3.6.1.2.1.31.1.1.1.6.7").unwrap();
        assert_eq!(ifindex_of(&instance, &base), Some(7));
        // The column base itself has no instance.
        assert_eq!(ifindex_of(&base, &base), None);
        // A multi-part instance (multi-index table, e.g. hwMemoryDevTable .1.0.0) folds to a
        // stable non-None key, and two distinct instances fold to distinct keys.
        let multi_a = parse_oid("1.3.6.1.2.1.31.1.1.1.6.1.0.0").unwrap();
        let multi_b = parse_oid("1.3.6.1.2.1.31.1.1.1.6.2.0.0").unwrap();
        let ka = ifindex_of(&multi_a, &base);
        let kb = ifindex_of(&multi_b, &base);
        assert!(ka.is_some() && kb.is_some());
        assert_ne!(ka, kb);
        assert_eq!(ifindex_of(&multi_a, &base), ka, "folding is deterministic");
        // An OID under a different column is not relative to this base.
        let other = parse_oid("1.3.6.1.2.1.2.2.1.8.7").unwrap();
        assert_eq!(ifindex_of(&other, &base), None);
    }

    #[test]
    fn string_value_decodes_octet_string_and_skips_numerics() {
        assert_eq!(
            string_value(&ObjectValue::String(b"Gi0/1".to_vec())),
            Some("Gi0/1".to_owned())
        );
        assert_eq!(string_value(&ObjectValue::Counter32(5)), None);
    }

    #[test]
    fn string_value_renders_object_id_as_dotted_decimal() {
        // sysObjectID comes back as an OBJECT IDENTIFIER — render it dotted for classification.
        let oid = parse_oid("1.3.6.1.4.1.9.1.516").unwrap();
        assert_eq!(
            string_value(&ObjectValue::ObjectId(oid)),
            Some("1.3.6.1.4.1.9.1.516".to_owned())
        );
    }

    /// The point of the instance walk: octets survive as octets. A chassis id that went through
    /// `string_value` would come back as replacement characters and could no longer be told apart
    /// from a different undecodable id.
    #[test]
    fn raw_value_keeps_octets_verbatim_where_string_value_would_mangle_them() {
        let mac = vec![0x00, 0x1b, 0x54, 0xff, 0x00, 0x9a];
        assert_eq!(
            raw_value(&ObjectValue::String(mac.clone())),
            SnmpValue::Bytes(mac.clone())
        );
        // The existing string walker's mapper loses those bytes.
        assert_ne!(
            string_value(&ObjectValue::String(mac.clone())).map(String::into_bytes),
            Some(mac)
        );
    }

    #[test]
    fn raw_value_maps_every_value_type() {
        assert_eq!(raw_value(&ObjectValue::Integer(-5)), SnmpValue::Int(-5));
        assert_eq!(raw_value(&ObjectValue::Counter32(7)), SnmpValue::Int(7));
        assert_eq!(
            raw_value(&ObjectValue::Counter64(u64::MAX)),
            SnmpValue::Int(i64::MAX),
            "an out-of-range counter saturates rather than wrapping into a negative subtype"
        );
        let oid = parse_oid("1.3.6.1.4.1.9").unwrap();
        assert_eq!(
            raw_value(&ObjectValue::ObjectId(oid)),
            SnmpValue::Oid("1.3.6.1.4.1.9".to_owned())
        );
        // An IpAddress arrives as its octets, so the same renderer reads it as a CDP address does.
        assert_eq!(
            raw_value(&ObjectValue::IpAddress(std::net::Ipv4Addr::new(
                10, 0, 0, 1
            ))),
            SnmpValue::Bytes(vec![10, 0, 0, 1])
        );
    }
}
