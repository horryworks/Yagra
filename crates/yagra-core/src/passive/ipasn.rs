// SPDX-License-Identifier: AGPL-3.0-only
//! IP→ASN enrichment (ADR-031 Increment 3).
//!
//! Most non-BGP exporters (access switches, home/edge routers — including the lab USG) export flow
//! records with `AS = 0`. To still attribute traffic to autonomous systems, core can enrich `AS = 0`
//! flows from an **offline** IP→ASN table: the public-domain [iptoasn.com](https://iptoasn.com)
//! dataset (a TSV of `range_start  range_end  asn  country  as_name`, derived from RouteViews). The
//! table is loaded **once at startup**, opt-in via `YAGRA_IPASN_DB` (default off ⇒ no enrichment, no
//! runtime egress). Lookups are binary searches over sorted, non-overlapping range vectors.
//!
//! Export-provided AS always wins — the writer only enriches records whose AS is still 0. AS **names**
//! are resolved separately at the API layer via [`IpAsnDb::name_of`]; ClickHouse stores numbers only.
//!
//! The dataset is untrusted input: every row is parsed defensively (a malformed line is skipped, never
//! fatal) and memory is bounded by the file the operator supplied.

use std::collections::HashMap;
use std::net::IpAddr;
use std::path::Path;
use std::sync::Arc;

/// One IPv4 range mapped to an AS number.
struct V4Range {
    start: u32,
    end: u32,
    asn: u32,
}

/// One IPv6 range mapped to an AS number.
struct V6Range {
    start: u128,
    end: u128,
    asn: u32,
}

/// An in-memory IP→ASN table plus an ASN→name map, loaded from an iptoasn.com TSV.
pub struct IpAsnDb {
    /// IPv4 ranges, sorted by `start` (non-overlapping in the source data).
    v4: Vec<V4Range>,
    /// IPv6 ranges, sorted by `start`.
    v6: Vec<V6Range>,
    /// AS number → organization name (first non-empty name seen for the AS).
    names: HashMap<u32, String>,
}

/// Shared, hot-swappable handle to the loaded table. `None` ⇒ enrichment off. A background reloader
/// (`YAGRA_IPASN_RELOAD_SECS`) can swap in a freshly-fetched table without restarting core, so an
/// external updater (e.g. the compose `ipasn-updater` sidecar) keeps the data current (ADR-031).
pub type IpAsnHandle = Arc<std::sync::RwLock<Option<Arc<IpAsnDb>>>>;

/// An empty handle (enrichment off) — for skeleton mode and tests.
#[must_use]
pub fn empty_handle() -> IpAsnHandle {
    Arc::new(std::sync::RwLock::new(None))
}

impl IpAsnDb {
    /// Load an iptoasn TSV from `path` (plain text; gunzip the `.gz` download first). Returns an
    /// `Arc` so it can be shared by the writer and the API without cloning the tables.
    pub fn load(path: &Path) -> anyhow::Result<Arc<Self>> {
        let content = std::fs::read_to_string(path)
            .map_err(|e| anyhow::anyhow!("reading IP→ASN dataset {}: {e}", path.display()))?;
        Ok(Self::from_tsv(&content))
    }

    /// Parse the iptoasn TSV body. Lines that don't parse are skipped. `asn = 0` rows ("Not routed" /
    /// private ranges) are dropped so those addresses stay `unknown` rather than getting a bogus AS.
    /// `pub(crate)` so the flow-enrichment tests in `flow_ingest.rs` can build a small in-memory table.
    pub(crate) fn from_tsv(content: &str) -> Arc<Self> {
        let mut v4 = Vec::new();
        let mut v6 = Vec::new();
        let mut names: HashMap<u32, String> = HashMap::new();
        for line in content.lines() {
            if line.is_empty() {
                continue;
            }
            // range_start \t range_end \t asn \t country \t as_name
            let mut it = line.split('\t');
            let (Some(start_s), Some(end_s), Some(asn_s)) = (it.next(), it.next(), it.next())
            else {
                continue;
            };
            let _country = it.next();
            let name = it.next().unwrap_or("").trim();
            let Ok(asn) = asn_s.trim().parse::<u32>() else {
                continue;
            };
            if asn == 0 {
                continue; // unrouted / private range — leave as unknown
            }
            match (start_s.parse::<IpAddr>(), end_s.parse::<IpAddr>()) {
                (Ok(IpAddr::V4(a)), Ok(IpAddr::V4(b))) => v4.push(V4Range {
                    start: u32::from(a),
                    end: u32::from(b),
                    asn,
                }),
                (Ok(IpAddr::V6(a)), Ok(IpAddr::V6(b))) => v6.push(V6Range {
                    start: u128::from(a),
                    end: u128::from(b),
                    asn,
                }),
                _ => continue,
            }
            if !name.is_empty() {
                names.entry(asn).or_insert_with(|| name.to_owned());
            }
        }
        v4.sort_by_key(|r| r.start);
        v6.sort_by_key(|r| r.start);
        Arc::new(Self { v4, v6, names })
    }

    /// Total number of ranges loaded (v4 + v6) — for the startup log / observability.
    #[must_use]
    pub fn len(&self) -> usize {
        self.v4.len() + self.v6.len()
    }

    /// Whether no ranges were loaded (an empty or unparseable dataset).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.v4.is_empty() && self.v6.is_empty()
    }

    /// Look up the AS number owning `ip`, or `None` if unknown / unrouted. A v4-mapped v6 address is
    /// resolved against the IPv4 table.
    #[must_use]
    pub fn lookup(&self, ip: IpAddr) -> Option<u32> {
        match ip {
            IpAddr::V4(v4) => Self::find_v4(&self.v4, u32::from(v4)),
            IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
                Some(m) => Self::find_v4(&self.v4, u32::from(m)),
                None => Self::find_v6(&self.v6, u128::from(v6)),
            },
        }
    }

    fn find_v4(ranges: &[V4Range], key: u32) -> Option<u32> {
        let idx = ranges.partition_point(|r| r.start <= key);
        let r = ranges.get(idx.checked_sub(1)?)?;
        (key <= r.end).then_some(r.asn)
    }

    fn find_v6(ranges: &[V6Range], key: u128) -> Option<u32> {
        let idx = ranges.partition_point(|r| r.start <= key);
        let r = ranges.get(idx.checked_sub(1)?)?;
        (key <= r.end).then_some(r.asn)
    }

    /// The organization name for `asn`, if the dataset carried one.
    #[must_use]
    pub fn name_of(&self, asn: u32) -> Option<&str> {
        self.names.get(&asn).map(String::as_str)
    }
}

/// Load the configured dataset once and hand back the hot-swappable handle.
///
/// Opt-in/default-OFF (ADR-031 Increment 3): `None` leaves the handle empty and AS names unset.
/// A missing, unreadable or empty file **logs and disables enrichment** rather than failing startup
/// — a stale path must never take core down. Moved here from `run_live` by ADR-090.
///
/// The handle is shared by the flow writer (IP→AS) and the flow API (AS→name), and
/// [`start_reload`] can replace what is behind it without a restart.
pub(crate) fn open(db_path: Option<&str>) -> IpAsnHandle {
    let initial = db_path.and_then(|p| match IpAsnDb::load(Path::new(p)) {
        Ok(db) if db.is_empty() => {
            tracing::warn!(
                path = p,
                "IP→ASN dataset loaded 0 ranges — AS enrichment disabled"
            );
            None
        }
        Ok(db) => {
            tracing::info!(
                ranges = db.len(),
                path = p,
                "IP→ASN enrichment enabled (ADR-031)"
            );
            Some(db)
        }
        Err(e) => {
            tracing::warn!(error = %e, path = p, "IP→ASN dataset load failed — AS enrichment disabled");
            None
        }
    });
    Arc::new(std::sync::RwLock::new(initial))
}

/// Re-read the dataset from disk every `reload_secs` and hot-swap it in, so an external updater
/// (the compose `ipasn-updater` sidecar writing to a shared volume) keeps the table fresh without
/// restarting core. `0` (the default) starts nothing. Moved here from `run_live` by ADR-090.
///
/// **Runs on every core, deliberately not leader-gated** — the flow API resolves names everywhere,
/// so a standby needs the same table the leader has.
///
/// A failed or empty reload **keeps the previous table**: a truncated file mid-write must not erase
/// working enrichment. It also recovers if the file appeared only after startup.
pub(crate) fn start_reload(
    handle: IpAsnHandle,
    db_path: Option<&str>,
    reload_secs: u64,
    shutdown: &yagra_telemetry::CancellationToken,
) {
    let Some(path) = db_path.map(str::to_owned) else {
        return;
    };
    if reload_secs == 0 {
        return;
    }
    let sd = shutdown.clone();
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(std::time::Duration::from_secs(reload_secs));
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        ticker.tick().await; // consume the immediate first tick
        loop {
            tokio::select! {
                () = sd.cancelled() => break,
                _ = ticker.tick() => {
                    // Offload the blocking file read + parse/sort of the ~500k-row iptoasn TSV
                    // onto the blocking pool so a reload tick never stalls a Tokio worker
                    // (same discipline as the store-and-forward disk read).
                    let p = path.clone();
                    let loaded = tokio::task::spawn_blocking(move || IpAsnDb::load(Path::new(&p))).await;
                    match loaded {
                        Ok(Ok(db)) if !db.is_empty() => {
                            let ranges = db.len();
                            *handle.write().unwrap() = Some(db);
                            tracing::info!(ranges, "IP→ASN dataset reloaded (ADR-031)");
                        }
                        Ok(Ok(_)) => tracing::warn!("IP→ASN reload read 0 ranges — keeping previous table"),
                        Ok(Err(e)) => tracing::warn!(error = %e, "IP→ASN reload failed — keeping previous table"),
                        Err(e) => tracing::warn!(error = %e, "IP→ASN reload task panicked — keeping previous table"),
                    }
                }
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "\
1.0.0.0\t1.0.0.255\t13335\tUS\tCLOUDFLARENET
8.8.8.0\t8.8.8.255\t15169\tUS\tGOOGLE
10.0.0.0\t10.255.255.255\t0\tNone\tNot routed
2001:4860::\t2001:4860:ffff:ffff:ffff:ffff:ffff:ffff\t15169\tUS\tGOOGLE";

    #[test]
    fn looks_up_v4_and_resolves_name() {
        let db = IpAsnDb::from_tsv(SAMPLE);
        assert_eq!(db.lookup("8.8.8.8".parse().unwrap()), Some(15169));
        assert_eq!(db.lookup("1.0.0.1".parse().unwrap()), Some(13335));
        assert_eq!(db.name_of(15169), Some("GOOGLE"));
        assert_eq!(db.name_of(13335), Some("CLOUDFLARENET"));
    }

    #[test]
    fn misses_return_none() {
        let db = IpAsnDb::from_tsv(SAMPLE);
        // A gap between the loaded ranges.
        assert_eq!(db.lookup("9.9.9.9".parse().unwrap()), None);
        // A private range (asn 0 in the data) is dropped → unknown, never a bogus AS.
        assert_eq!(db.lookup("10.1.2.3".parse().unwrap()), None);
        assert_eq!(db.name_of(64500), None);
    }

    #[test]
    fn looks_up_v6_and_v4_mapped() {
        let db = IpAsnDb::from_tsv(SAMPLE);
        assert_eq!(
            db.lookup("2001:4860:4860::8888".parse().unwrap()),
            Some(15169)
        );
        // v4-mapped v6 resolves against the v4 table.
        assert_eq!(db.lookup("::ffff:8.8.8.8".parse().unwrap()), Some(15169));
    }

    #[test]
    fn empty_or_garbage_is_safe() {
        assert!(IpAsnDb::from_tsv("").is_empty());
        let db = IpAsnDb::from_tsv("not\ta\tvalid\trow\nalso-bad");
        assert!(db.is_empty());
        assert_eq!(db.lookup("8.8.8.8".parse().unwrap()), None);
    }
}
