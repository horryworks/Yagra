//! SNMP v2c GET + GETBULK table walk over a pure-Rust client (`csnmp`) — ADR-021 PoC.
//!
//! This validates the pure-Rust path before any net-snmp FFI fallback. v3 (auth/priv)
//! is still pending. Values are returned **raw** (counters included) — rates are derived
//! at query time (ADR-012). Live-only (needs a device + UDP); the numeric mapping and
//! ifIndex extraction are unit-tested.

use crate::{SnmpSample, SnmpTableSample, SnmpTableString, TransportError};
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

/// Row key for an instance OID relative to its column `base`. A single trailing sub-identifier
/// (the common case — ifIndex, entPhysicalIndex, …) is used directly. A **multi-part** instance
/// — a multi-index table such as HUAWEI-MEMORY-MIB `hwMemoryDevTable` (`.frame.slot.cpu`) or
/// BGP4-MIB peers (`.a.b.c.d`) — is folded into a stable synthetic key so the row is still
/// collected (node-level health aggregates over rows, so the key only needs to be distinct, not
/// meaningful). Returns `None` when the OID isn't under `base`, or is `base` itself (no instance).
fn ifindex_of(oid: &ObjectIdentifier, base: &ObjectIdentifier) -> Option<u32> {
    let tail = oid.relative_to(base)?;
    match tail.as_slice() {
        [] => None,
        [ifindex] => Some(*ifindex),
        multi => Some(fold_subids(multi)),
    }
}

/// Fold a multi-part instance index into a stable `u32` row key (FNV-1a over the sub-ids). Used
/// only for multi-index tables, where the key just needs to be deterministic and collision-rare.
fn fold_subids(subids: &[u32]) -> u32 {
    let mut h: u32 = 0x811c_9dc5;
    for &n in subids {
        for b in n.to_be_bytes() {
            h ^= u32::from(b);
            h = h.wrapping_mul(0x0100_0193);
        }
    }
    h
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
}
