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

/// Walk numeric table columns from `target` via GETBULK. Each column base yields one row
/// per ifIndex; rows whose instance OID isn't exactly `base + <single sub-id>` are skipped
/// (guards against multi-index tables we don't model). A per-column walk failure is logged
/// and skipped. Counters are returned **raw** (rates derived at query time, ADR-012).
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

/// Extract the ifIndex of an instance OID relative to its column `base`: the instance must
/// be `base` followed by exactly one sub-identifier. Returns `None` otherwise (a different
/// column, or a multi-index leaf we don't model).
fn ifindex_of(oid: &ObjectIdentifier, base: &ObjectIdentifier) -> Option<u32> {
    let tail = oid.relative_to(base)?;
    match tail.as_slice() {
        [ifindex] => Some(*ifindex),
        _ => None,
    }
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
    fn ifindex_extracted_only_for_single_trailing_subid() {
        let base = parse_oid("1.3.6.1.2.1.31.1.1.1.6").unwrap();
        // base + .7 → ifIndex 7.
        let instance = parse_oid("1.3.6.1.2.1.31.1.1.1.6.7").unwrap();
        assert_eq!(ifindex_of(&instance, &base), Some(7));
        // The column base itself has no row index.
        assert_eq!(ifindex_of(&base, &base), None);
        // A two-sub-id leaf (a table we don't model) is skipped, not mis-parsed.
        let multi = parse_oid("1.3.6.1.2.1.31.1.1.1.6.7.2").unwrap();
        assert_eq!(ifindex_of(&multi, &base), None);
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
