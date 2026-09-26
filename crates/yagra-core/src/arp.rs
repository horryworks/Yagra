// SPDX-License-Identifier: AGPL-3.0-only
//! ARP-based endpoint discovery: what has replied on the wire, and which of it nobody monitors
//! (ADR-043 Increment 3, migration 0070).
//!
//! Two stores and the pure rule between them:
//!
//! * [`ArpRepo`] holds one aggregated summary per node — the observation, replaced wholesale, with
//!   the same "a failed walk writes nothing" discipline as [`crate::neighbors`] and [`crate::l3`].
//! * [`DiscoveredRepo`] holds one row per **endpoint**, fleet-wide — the finding.
//! * [`unmonitored`] turns the first into the second, and is pure so the rule that decides what
//!   counts as "unmonitored" is testable without a database.
//!
//! ## Why the finding is keyed by the endpoint and not by the observation
//!
//! Every router on a segment sees the same host. Keyed by `(router, port, address)` an operator
//! reviewing what is unmonitored would read the same host once per router, and the count — the
//! number the whole feature exists to produce — would be inflated by the redundancy of the network
//! it is describing. So `via_node` is *who told us*, not part of the identity.
//!
//! ## Why these endpoints are not map vertices
//!
//! Deliberate, and the one design point most likely to be "fixed" later. An unmonitored endpoint has
//! no state: no liveness, no thresholds, nothing to colour. Drawing four thousand stateless boxes is
//! exactly what `MAP_CAP` exists to prevent, and it would bury the nodes that do have state. An
//! endpoint becomes a vertex by becoming a **node**, at which point Increment 1's derivation picks
//! it up with no special case anywhere.

use chrono::{DateTime, Utc};
use sqlx::types::Json;
use sqlx::{PgPool, Row};
use std::collections::{BTreeMap, BTreeSet};
use std::net::IpAddr;
use uuid::Uuid;
use yagra_common::{
    ArpSummary, NeighborCapability, NeighborProto, NeighborSet, NodeId, RoutingProto,
    RoutingSnapshot,
};

/// Default cadence for the ARP walk: six hours.
///
/// Slower than the neighbour and interface-address walks by design. Those read tables sized by the
/// device; this one reads a table sized by the network, and it is the only walk in ADR-043 that
/// costs a busy switch measurable work. Meraki's inventory tier made the same call at the same
/// number.
pub const DEFAULT_ARP_INTERVAL_SECS: u32 = 21_600;

/// Fleet-wide ceiling on stored endpoints.
///
/// Not a performance guess: a campus with a few thousand hosts fits comfortably, and a deployment
/// that blows past this is one where the answer has stopped being a review list and started being a
/// second inventory. The overflow is reported (`truncated_total`) and the **oldest-seen** rows are
/// the ones dropped, so what survives is what is currently on the network.
pub const MAX_DISCOVERED_ENDPOINTS: usize = 10_000;

/// How long an endpoint survives without being seen again: seven days.
///
/// Shorter than any other retention here on purpose. A laptop that appeared once and left is not a
/// finding, and without an age rule a busy campus fills the table with them until nothing in it can
/// be reviewed.
pub const DISCOVERED_RETENTION_SECS: i64 = 7 * 86_400;

/// How often the endpoint sweep runs when there is anything to sweep.
pub const ENDPOINT_SWEEP_INTERVAL_SECS: u64 = 300;

/// How many pieces of evidence one endpoint keeps (ADR-179 決定 5).
///
/// Enough to show every source and a couple of observers for each; a host every router on a campus
/// has in its ARP cache would otherwise carry dozens of identical "ARP on …" lines.
pub const MAX_EVIDENCE_PER_ENDPOINT: usize = 8;

/// The longest name or evidence text kept from a device (the `name` column's CHECK).
const MAX_DEVICE_TEXT: usize = 255;

/// Where an unmonitored endpoint was seen (ADR-179).
///
/// The declaration order is the order evidence is listed in, so `Ord` is derived from it on
/// purpose: ARP first because it is what this list always showed, then what a device says about its
/// neighbour, then routing, then what reached Yagra on its own.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    serde::Serialize,
    serde::Deserialize,
    utoipa::ToSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum EndpointSource {
    /// A monitored router's ARP or IPv6 neighbour cache.
    Arp,
    /// A monitored device's LLDP neighbour, by its advertised management address.
    Lldp,
    /// A monitored device's CDP neighbour, by its advertised address.
    Cdp,
    /// An OSPF neighbour of a monitored router.
    Ospf,
    /// A BGP peer of a monitored router.
    Bgp,
    /// It sent syslog to Yagra and no node claimed it.
    Syslog,
    /// It sent an SNMP trap to Yagra and no node claimed it.
    Trap,
}

impl EndpointSource {
    /// Whether the address is only the endpoint's own say-so: a syslog or trap source address, which
    /// anyone can forge over UDP. Every other source is a monitored device reporting what it saw.
    #[must_use]
    pub fn is_self_reported(self) -> bool {
        match self {
            EndpointSource::Syslog | EndpointSource::Trap => true,
            EndpointSource::Arp
            | EndpointSource::Lldp
            | EndpointSource::Cdp
            | EndpointSource::Ospf
            | EndpointSource::Bgp => false,
        }
    }
}

/// Whether nothing but a syslog or trap sender vouches for this address (ADR-179 増分 5 決定 2).
///
/// Such a row is listed but never probed or imported: its address may be forged, and probing it
/// would send every chosen credential to whoever forged it.
#[must_use]
pub fn only_senders_vouch(evidence: &[EndpointEvidence]) -> bool {
    !evidence.is_empty() && evidence.iter().all(|e| e.source.is_self_reported())
}

// Test-only: the production path serializes through serde, and this is what the token test compares
// it against.
#[cfg(test)]
impl EndpointSource {
    /// Every source, in listing order.
    pub const ALL: [EndpointSource; 7] = [
        EndpointSource::Arp,
        EndpointSource::Lldp,
        EndpointSource::Cdp,
        EndpointSource::Ospf,
        EndpointSource::Bgp,
        EndpointSource::Syslog,
        EndpointSource::Trap,
    ];

    /// The stable token — the serde tag, stored inside `l3_discovered.evidence`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            EndpointSource::Arp => "arp",
            EndpointSource::Lldp => "lldp",
            EndpointSource::Cdp => "cdp",
            EndpointSource::Ospf => "ospf",
            EndpointSource::Bgp => "bgp",
            EndpointSource::Syslog => "syslog",
            EndpointSource::Trap => "trap",
        }
    }
}

/// Drop every piece of evidence whose observing node is not in `visible` (ADR-179 増分 4).
///
/// A row is listed when its *lowest* observer is in the caller's scope, but its evidence names
/// every observer — so without this a folder-scoped caller read another folder's node id, port and
/// the platform it saw. The whole entry goes, not just its fields: that some hidden node saw the
/// address at all is itself about a segment the caller cannot see. A sender's own evidence (no
/// `via_node`) is about the endpoint and stays.
pub fn drop_hidden_evidence(evidence: &mut Vec<EndpointEvidence>, visible: &BTreeSet<Uuid>) {
    evidence.retain(|e| e.via_node.is_none_or(|n| visible.contains(&n)));
}

/// One observation that made an address a candidate.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
pub struct EndpointEvidence {
    /// What saw it.
    pub source: EndpointSource,
    /// The monitored node that reported it; `null` for a syslog or trap sender, which reported
    /// itself.
    #[serde(default)]
    pub via_node: Option<Uuid>,
    /// The reporting node's ifIndex, when the source names one.
    #[serde(default)]
    pub via_ifindex: Option<u32>,
    /// The reporting node's own port name, as its LLDP/CDP table names it.
    #[serde(default)]
    pub port: Option<String>,
    /// What the source said about the endpoint: its platform or system description (LLDP/CDP), or
    /// the hostname it put in its syslog messages. Device-supplied text.
    #[serde(default)]
    pub detail: Option<String>,
}

/// The two passive-event kinds that carry a sender address worth discovering (ADR-179 決定 3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SenderKind {
    Syslog,
    Trap,
}

impl SenderKind {
    /// The stable token stored in `event_senders.kind`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            SenderKind::Syslog => "syslog",
            SenderKind::Trap => "trap",
        }
    }

    /// Parse a stored token back.
    #[must_use]
    pub fn from_token(s: &str) -> Option<Self> {
        match s {
            "syslog" => Some(SenderKind::Syslog),
            "trap" => Some(SenderKind::Trap),
            _ => None,
        }
    }

    /// The evidence source a sender of this kind becomes.
    #[must_use]
    pub const fn source(self) -> EndpointSource {
        match self {
            SenderKind::Syslog => EndpointSource::Syslog,
            SenderKind::Trap => EndpointSource::Trap,
        }
    }
}

/// A passive-event sender no node claimed, as `event_senders` holds it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SenderObservation {
    pub ip: IpAddr,
    pub kind: SenderKind,
    pub hostname: Option<String>,
}

/// One endpoint the fleet has seen but does not monitor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveredEndpoint {
    pub id: Uuid,
    pub ip: IpAddr,
    pub mac: Option<String>,
    /// Which monitored node resolved it; `None` once that node has been deleted, and for an
    /// endpoint only a syslog or trap sender vouches for.
    pub via_node: Option<NodeId>,
    pub via_ifindex: Option<u32>,
    /// The best name any source gave it.
    pub name: Option<String>,
    /// Where it was seen. Never empty on a row read back: a row written before evidence existed
    /// reads as the one ARP observation it was.
    pub evidence: Vec<EndpointEvidence>,
    pub first_seen: DateTime<Utc>,
    pub last_seen: DateTime<Utc>,
    /// Set once the address became an inventory node — imported from here or added by hand.
    pub promoted_node_id: Option<NodeId>,
}

/// One endpoint as the sweep computed it, before it reaches the database.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EndpointObservation {
    pub ip: IpAddr,
    pub mac: Option<String>,
    pub via_node: Option<NodeId>,
    pub via_ifindex: Option<u32>,
    pub name: Option<String>,
    pub evidence: Vec<EndpointEvidence>,
}

/// Everything the sweep reads, fleet-wide (ADR-179 決定 2).
#[derive(Debug, Default, Clone, Copy)]
pub struct Signals<'a> {
    pub arp: &'a [(NodeId, ArpSummary)],
    pub neighbors: &'a [(NodeId, NeighborSet)],
    pub routing: &'a [(NodeId, RoutingSnapshot)],
    pub senders: &'a [SenderObservation],
}

#[cfg(test)]
/// Every endpoint the fleet observed through ARP alone that is **not** already an inventory
/// address — [`candidates`] with the other three signals empty.
#[must_use]
pub fn unmonitored(
    summaries: &[(NodeId, ArpSummary)],
    known: &BTreeSet<IpAddr>,
) -> Vec<EndpointObservation> {
    candidates(
        &Signals {
            arp: summaries,
            ..Signals::default()
        },
        known,
    )
}

/// Whether an address can name a device of its own. `0.0.0.0` is what an agent reports for a
/// neighbour it has not resolved, and every device's loopback would match every other's.
fn identifies_a_device(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            !v4.is_unspecified()
                && !v4.is_loopback()
                && !v4.is_link_local()
                && !v4.is_multicast()
                && !v4.is_broadcast()
        }
        IpAddr::V6(v6) => {
            !v6.is_unspecified()
                && !v6.is_loopback()
                && !v6.is_multicast()
                && (v6.segments()[0] & 0xffc0) != 0xfe80
        }
    }
}

/// Whether a neighbour capability describes an end station rather than network equipment.
fn is_end_station(c: NeighborCapability) -> bool {
    match c {
        NeighborCapability::Phone | NeighborCapability::Host => true,
        NeighborCapability::Router
        | NeighborCapability::Bridge
        | NeighborCapability::Switch
        | NeighborCapability::WlanAp
        | NeighborCapability::Repeater
        | NeighborCapability::CableDevice
        | NeighborCapability::Igmp
        | NeighborCapability::Other => false,
    }
}

/// Device text trimmed, bounded, and dropped when empty.
fn device_text(s: Option<&str>) -> Option<String> {
    let t = s?.trim();
    if t.is_empty() {
        return None;
    }
    Some(t.chars().take(MAX_DEVICE_TEXT).collect())
}

/// One address while the sweep is still gathering what it knows about it.
#[derive(Default)]
struct Gathered {
    /// `(observer, MAC)` from every ARP sighting — the lowest observer's wins.
    macs: Vec<(Uuid, String)>,
    /// `(rank, observer, name)` — lower rank wins, then the lower observer.
    names: Vec<(u8, Option<Uuid>, String)>,
    evidence: Vec<EndpointEvidence>,
}

/// Every endpoint any signal names that is **not** already an inventory address (ADR-179).
///
/// Pure: observations and the known-address set in, findings out. No clock, no database.
///
/// `known` must carry both `nodes.address` **and** every address in `node_l3`. Using only the node
/// addresses looks equivalent and is not: a router monitored on its management address answers ARP
/// for its LAN interface too, so its own `192.168.1.1` would be reported as an unmonitored endpoint
/// on every segment it terminates. That is the false positive that would make the list unreadable
/// on day one, and it is why this takes a set rather than a node list. The same set is what drops an
/// LLDP management address or a syslog sender that is some monitored device's *other* interface.
///
/// The row's `via_node` is the lowest observing node id across all its evidence, so the attribution
/// does not flip between sweeps as the map's redundancy shifts — a row whose `via_node` changed every
/// five minutes would look like the endpoint was moving. Evidence is ordered by source, then
/// observer, and capped at [`MAX_EVIDENCE_PER_ENDPOINT`].
///
/// What each signal contributes, and what it does not:
/// * **LLDP/CDP** — only a neighbour that advertised a management address (the table is keyed by
///   address), and never one whose capabilities are all end-station ones: the phones behind an
///   access switch would bury everything else. A neighbour that advertises no capabilities is kept.
/// * **Routing** — OSPF neighbours and BGP peers. The connected-route probe is not discovery: core
///   chose the addresses it asks about.
/// * **Senders** — syslog/trap sources no node claimed. Behind NAT that address is the translator's,
///   which is why this is evidence for a person to weigh and never an automatic import.
#[must_use]
pub fn candidates(signals: &Signals<'_>, known: &BTreeSet<IpAddr>) -> Vec<EndpointObservation> {
    fn admit<'m>(
        by_ip: &'m mut BTreeMap<IpAddr, Gathered>,
        known: &BTreeSet<IpAddr>,
        ip: IpAddr,
    ) -> Option<&'m mut Gathered> {
        if known.contains(&ip) || !identifies_a_device(ip) {
            return None;
        }
        Some(by_ip.entry(ip).or_default())
    }
    let mut by_ip: BTreeMap<IpAddr, Gathered> = BTreeMap::new();

    for (node, summary) in signals.arp {
        for entry in &summary.entries {
            let Some(g) = admit(&mut by_ip, known, entry.ip) else {
                continue;
            };
            if let Some(mac) = &entry.mac {
                g.macs.push((node.as_uuid(), mac.clone()));
            }
            g.evidence.push(EndpointEvidence {
                source: EndpointSource::Arp,
                via_node: Some(node.as_uuid()),
                via_ifindex: Some(entry.ifindex),
                port: None,
                detail: None,
            });
        }
    }

    for (node, set) in signals.neighbors {
        for nb in &set.neighbors {
            if !nb.capabilities.is_empty() && nb.capabilities.iter().all(|c| is_end_station(*c)) {
                continue;
            }
            let Some(ip) = nb
                .remote_mgmt_addr
                .as_deref()
                .and_then(|a| a.trim().parse::<IpAddr>().ok())
            else {
                continue;
            };
            let Some(g) = admit(&mut by_ip, known, ip) else {
                continue;
            };
            let (source, name, rank) = match nb.proto {
                NeighborProto::Lldp => (
                    EndpointSource::Lldp,
                    device_text(nb.remote_sys_name.as_deref()),
                    0,
                ),
                // CDP has no separate system name: its device id is the name.
                NeighborProto::Cdp => (
                    EndpointSource::Cdp,
                    device_text(nb.remote_sys_name.as_deref())
                        .or_else(|| device_text(Some(&nb.remote_chassis))),
                    1,
                ),
            };
            if let Some(name) = name {
                g.names.push((rank, Some(node.as_uuid()), name));
            }
            g.evidence.push(EndpointEvidence {
                source,
                via_node: Some(node.as_uuid()),
                via_ifindex: nb.local_ifindex,
                port: device_text(Some(&nb.local_port)),
                detail: device_text(nb.remote_platform.as_deref()).or_else(|| {
                    // The first line of a system description is the useful one; the rest is
                    // copyright boilerplate on most platforms.
                    device_text(nb.remote_sys_desc.as_deref().and_then(|d| d.lines().next()))
                }),
            });
        }
    }

    for (node, snapshot) in signals.routing {
        for adj in &snapshot.adjacencies {
            let source = match adj.proto {
                RoutingProto::Ospf => EndpointSource::Ospf,
                RoutingProto::Bgp => EndpointSource::Bgp,
                RoutingProto::Route => continue,
            };
            let Some(g) = admit(&mut by_ip, known, adj.peer) else {
                continue;
            };
            g.evidence.push(EndpointEvidence {
                source,
                via_node: Some(node.as_uuid()),
                via_ifindex: adj.local_ifindex,
                port: None,
                detail: None,
            });
        }
    }

    for sender in signals.senders {
        let Some(g) = admit(&mut by_ip, known, sender.ip) else {
            continue;
        };
        let hostname = device_text(sender.hostname.as_deref());
        if let Some(h) = &hostname {
            g.names.push((2, None, h.clone()));
        }
        g.evidence.push(EndpointEvidence {
            source: sender.kind.source(),
            via_node: None,
            via_ifindex: None,
            port: None,
            detail: hostname,
        });
    }

    // Truncated after the map is built, so which endpoints survive does not depend on the order the
    // inputs were read in. Rows a monitored device saw are kept before rows only a sender vouches
    // for (ADR-179 増分 5 決定 3): a sender's address can be forged, and ten thousand forged low
    // addresses must not push a real device off the list. Listed by address, which is also the
    // order an operator scans.
    let (observed, senders): (Vec<_>, Vec<_>) = by_ip
        .into_iter()
        .partition(|(_, g)| !only_senders_vouch(&g.evidence));
    let mut kept: Vec<_> = observed
        .into_iter()
        .chain(senders)
        .take(MAX_DISCOVERED_ENDPOINTS)
        .collect();
    kept.sort_by_key(|(ip, _)| *ip);
    kept.into_iter()
        .map(|(ip, mut g)| {
            // `None` sorts after every observer, so a sender's line follows the nodes' lines of the
            // same source — and there are none, since only senders lack an observer.
            g.evidence.sort_by(|a, b| {
                (
                    a.source,
                    a.via_node.is_none(),
                    a.via_node,
                    a.via_ifindex,
                    &a.port,
                )
                    .cmp(&(
                        b.source,
                        b.via_node.is_none(),
                        b.via_node,
                        b.via_ifindex,
                        &b.port,
                    ))
            });
            g.evidence.dedup();
            g.evidence.truncate(MAX_EVIDENCE_PER_ENDPOINT);
            // The lowest observer across every source, and the port it saw the endpoint on.
            let via = g
                .evidence
                .iter()
                .filter_map(|e| e.via_node.map(|n| (n, e.via_ifindex)))
                .min_by_key(|(n, _)| *n);
            g.macs.sort();
            g.names.sort_by_key(|n| (n.0, n.1.is_none(), n.1));
            EndpointObservation {
                ip,
                mac: g.macs.into_iter().next().map(|(_, m)| m),
                via_node: via.map(|(n, _)| NodeId(n)),
                via_ifindex: via.and_then(|(_, i)| i),
                name: g.names.into_iter().next().map(|(_, _, n)| n),
                evidence: g.evidence,
            }
        })
        .collect()
}

/// PostgreSQL-backed store for per-node ARP observations.
pub struct ArpRepo {
    pool: PgPool,
}

impl ArpRepo {
    #[must_use]
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Record one observed summary, replacing whatever was stored.
    ///
    /// Unlike its siblings there is no history append here — see the migration header. `first_seen`
    /// still holds while the content key is unchanged, so "this port has looked like this for three
    /// weeks" remains answerable without a log.
    ///
    /// Only ever called with a summary the poller actually observed. A *failed* walk sends none, so
    /// this is never reached with an empty stand-in — which is what stops one timed-out walk from
    /// ageing every endpoint behind a router out of the table.
    pub async fn record_observation(
        &self,
        node_id: Uuid,
        summary: &ArpSummary,
    ) -> anyhow::Result<()> {
        let key = summary.content_key();
        sqlx::query(
            "INSERT INTO node_arp \
                 (node_id, arp_key, summary, entry_count, truncated, first_seen, last_seen) \
             VALUES ($1, $2, $3, $4, $5, now(), now()) \
             ON CONFLICT (node_id) DO UPDATE SET \
                 arp_key = EXCLUDED.arp_key, \
                 summary = EXCLUDED.summary, \
                 entry_count = EXCLUDED.entry_count, \
                 truncated = EXCLUDED.truncated, \
                 first_seen = CASE WHEN node_arp.arp_key = EXCLUDED.arp_key \
                                   THEN node_arp.first_seen ELSE now() END, \
                 last_seen = now()",
        )
        .bind(node_id)
        .bind(&key)
        .bind(Json(summary))
        .bind(i32::try_from(summary.observed).unwrap_or(i32::MAX))
        .bind(summary.truncated)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Every node's current summary — the endpoint sweep's input.
    ///
    /// Unpaged for the same reason `L3Repo::all_current` is: this is a whole-fleet computation and a
    /// slice of it produces a wrong answer rather than a partial one. Bounded by design — one
    /// document per node, each capped at `MAX_ARP_ENTRIES_PER_NODE`.
    pub async fn all_current(&self) -> anyhow::Result<Vec<(NodeId, ArpSummary)>> {
        let rows = sqlx::query("SELECT node_id, summary FROM node_arp")
            .fetch_all(&self.pool)
            .await?;
        rows.into_iter()
            .map(|row| {
                let summary: Json<ArpSummary> = row.try_get("summary")?;
                Ok((NodeId(row.try_get("node_id")?), summary.0))
            })
            .collect()
    }

    /// `(address, MAC)` from every node's ARP cache, for the addresses asked about — the duplicate
    /// check's `arp_mac` evidence (ADR-148).
    ///
    /// Read from these per-node summaries and **not** from `l3_discovered`: that table keeps only the
    /// endpoints nobody monitors ([`candidates`] drops every known address), so the MAC behind a
    /// monitored address is never in it. Each summary is a bounded sample, so an address that fell
    /// out of it is simply not reported.
    pub async fn macs_for(&self, addresses: &[String]) -> anyhow::Result<Vec<(IpAddr, String)>> {
        let rows = sqlx::query(concat!(
            "SELECT DISTINCT e->>'ip' AS ip, e->>'mac' AS mac ",
            "FROM node_arp, ",
            "jsonb_array_elements(coalesce(summary->'entries', '[]'::jsonb)) e ",
            "WHERE e->>'mac' IS NOT NULL AND e->>'ip' = ANY($1::text[])",
        ))
        .bind(addresses)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .filter_map(|row| {
                let ip = row
                    .try_get::<Option<String>, _>("ip")
                    .ok()
                    .flatten()?
                    .parse::<IpAddr>()
                    .ok()?;
                let mac = row.try_get::<Option<String>, _>("mac").ok().flatten()?;
                Some((ip, mac))
            })
            .collect())
    }

    /// The newest `last_seen` across every node, or `None` when no ARP walk has ever landed.
    ///
    /// The sweep's whole trigger, and its off switch: a deployment that never enabled ARP discovery
    /// has no rows here, so the sweep returns before it reads the inventory or the address
    /// projection. That is what keeps a feature nobody opted into from costing the leader anything.
    pub async fn observation_watermark(&self) -> anyhow::Result<Option<DateTime<Utc>>> {
        let row = sqlx::query("SELECT max(last_seen) AS w FROM node_arp")
            .fetch_one(&self.pool)
            .await?;
        Ok(row.try_get("w")?)
    }

    /// Fleet totals for the coverage line: endpoints observed, nodes reporting, nodes truncated.
    ///
    /// The third number is the one that matters. A truncated walk means the endpoint list is a
    /// sample, and a list presented as complete when it is a sample is the kind of quiet wrongness
    /// this codebase writes caps to avoid.
    pub async fn totals(&self) -> anyhow::Result<(i64, i64, i64)> {
        let row = sqlx::query(
            "SELECT coalesce(sum(entry_count), 0)::BIGINT AS observed, \
                    count(*)::BIGINT AS nodes, \
                    count(*) FILTER (WHERE truncated)::BIGINT AS truncated \
             FROM node_arp",
        )
        .fetch_one(&self.pool)
        .await?;
        Ok((
            row.try_get("observed")?,
            row.try_get("nodes")?,
            row.try_get("truncated")?,
        ))
    }
}

/// PostgreSQL-backed store for discovered endpoints.
pub struct DiscoveredRepo {
    pool: PgPool,
}

impl DiscoveredRepo {
    #[must_use]
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Every address the fleet already monitors: node addresses plus every interface address any
    /// node has reported.
    ///
    /// One query rather than two so the two halves cannot be read at different instants and produce
    /// an endpoint that is "unmonitored" only because a node was created between them. Addresses
    /// that fail to parse are dropped rather than failing the sweep — a malformed row must not stop
    /// discovery for the whole fleet.
    ///
    /// 🚨 **`host(address)`, never `address::TEXT`.** An explicit cast to text renders an `inet`
    /// *with* its masklen (`10.0.0.1/32`, `2001:db8::1/128`) even though the default display omits
    /// it, and `IpAddr::from_str` rejects that — so the drop-on-parse-failure above silently threw
    /// away **every** node address, and every monitored node was reported as an unmonitored
    /// endpoint. `host()` is documented to return the address alone.
    pub async fn known_addresses(&self) -> anyhow::Result<BTreeSet<IpAddr>> {
        let rows = sqlx::query(
            "SELECT host(address) AS ip FROM nodes \
             UNION \
             SELECT a->>'ip' AS ip FROM node_l3, \
                    jsonb_array_elements(coalesce(addresses->'addresses', '[]'::jsonb)) a",
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .filter_map(|r| r.try_get::<Option<String>, _>("ip").ok().flatten())
            .filter_map(|s| s.parse::<IpAddr>().ok())
            .collect())
    }

    /// Upsert one sweep's worth of observations, preserving `first_seen`.
    ///
    /// `first_seen` is the only thing here that cannot be recomputed, and it is what answers "how
    /// long was this host on the network before anyone monitored it" — so the upsert never touches
    /// it. Everything else is last-observation-wins.
    pub async fn upsert_batch(&self, rows: &[EndpointObservation]) -> anyhow::Result<u64> {
        if rows.is_empty() {
            return Ok(0);
        }
        let ips: Vec<String> = rows.iter().map(|r| r.ip.to_string()).collect();
        let macs: Vec<Option<String>> = rows.iter().map(|r| r.mac.clone()).collect();
        let vias: Vec<Option<Uuid>> = rows
            .iter()
            .map(|r| r.via_node.map(|n| n.as_uuid()))
            .collect();
        let ifs: Vec<Option<i32>> = rows
            .iter()
            .map(|r| r.via_ifindex.and_then(|i| i32::try_from(i).ok()))
            .collect();
        let names: Vec<Option<String>> = rows.iter().map(|r| r.name.clone()).collect();
        // As JSON text and cast in SQL: one array of documents, bound like every other column.
        let evidence: Vec<String> = rows
            .iter()
            .map(|r| serde_json::to_string(&r.evidence))
            .collect::<Result<_, _>>()?;
        let res = sqlx::query(
            "INSERT INTO l3_discovered \
                 (ip, mac, via_node, via_ifindex, name, evidence, first_seen, last_seen) \
             SELECT u.ip::INET, u.mac, u.via, u.ifidx, u.name, u.ev::JSONB, now(), now() \
             FROM UNNEST($1::TEXT[], $2::TEXT[], $3::UUID[], $4::INT[], $5::TEXT[], $6::TEXT[]) \
                  AS u(ip, mac, via, ifidx, name, ev) \
             ON CONFLICT (ip) DO UPDATE SET \
                 mac = EXCLUDED.mac, \
                 via_node = EXCLUDED.via_node, \
                 via_ifindex = EXCLUDED.via_ifindex, \
                 name = EXCLUDED.name, \
                 evidence = EXCLUDED.evidence, \
                 last_seen = now()",
        )
        .bind(&ips)
        .bind(&macs)
        .bind(&vias)
        .bind(&ifs)
        .bind(&names)
        .bind(&evidence)
        .execute(&self.pool)
        .await?;
        Ok(res.rows_affected())
    }

    /// Point every row whose address is now an inventory **device** node at that node.
    ///
    /// Called by the sweep **and** by the import handler, which is the point: an endpoint can become
    /// a node either way, and a rule expressed once cannot disagree with itself. Without it an
    /// operator who added the host by hand would keep reading it in the unmonitored list for a week.
    ///
    /// ⚠️ **A URL or DNS monitor at the same address does not count** (ADR-139 決定 1). Both store a
    /// resolved address in `nodes.address`, and matching on the address alone marked a router
    /// "monitored" because its web page was — while the scan table beside this one, which asks
    /// the same question through [`crate::repo::NodeRepo::DEVICE_NODE_PREDICATE`], said it was not.
    pub async fn reconcile_promotions(&self) -> anyhow::Result<u64> {
        let res = sqlx::query(Self::RECONCILE_PROMOTIONS)
            .execute(&self.pool)
            .await?;
        Ok(res.rows_affected())
    }

    /// The statement [`Self::reconcile_promotions`] runs.
    ///
    /// ⚠️ **Its last clause is a second spelling of `NodeRepo::DEVICE_NODE_PREDICATE`**, written out
    /// rather than interpolated because this module refuses `format!` in production code — a check
    /// that cannot tell a constant from a value. `the_promotion_statement_ends_in_the_device_predicate`
    /// pins the two together, so changing one without the other fails the build.
    const RECONCILE_PROMOTIONS: &'static str =
        "UPDATE l3_discovered d SET promoted_node_id = n.id \
         FROM nodes n \
         WHERE d.ip = n.address AND d.promoted_node_id IS DISTINCT FROM n.id \
         AND NOT EXISTS (SELECT 1 FROM url_checks uc WHERE uc.node_id = n.id) \
         AND NOT EXISTS (SELECT 1 FROM dns_checks dc WHERE dc.node_id = n.id)";

    /// Drop endpoints not seen inside the retention window, then enforce the fleet ceiling.
    ///
    /// Returns how many rows went. The ceiling deletes the **oldest-seen** rows, so what survives is
    /// what is currently on the network rather than whatever happened to be inserted first.
    pub async fn prune(&self, retention_secs: i64, cap: usize) -> anyhow::Result<u64> {
        let aged = sqlx::query(
            "DELETE FROM l3_discovered WHERE last_seen < now() - make_interval(secs => $1)",
        )
        .bind(retention_secs as f64)
        .execute(&self.pool)
        .await?;
        // `row_number()` rather than OFFSET: OFFSET here would shift under concurrent inserts, and
        // ADR-019's rule against it is about exactly that instability.
        let over = sqlx::query(
            "DELETE FROM l3_discovered d USING ( \
                 SELECT id, \
                        row_number() OVER (ORDER BY (via_node IS NULL), last_seen DESC, id DESC) AS rn \
                 FROM l3_discovered \
             ) r WHERE d.id = r.id AND r.rn > $1",
        )
        .bind(i64::try_from(cap).unwrap_or(i64::MAX))
        .execute(&self.pool)
        .await?;
        Ok(aged.rows_affected() + over.rows_affected())
    }

    /// A keyset page of endpoints, newest-seen first (ADR-019 — never OFFSET).
    ///
    /// `groups` is the caller's scope: `None` is unrestricted, `Some(&[])` matches nothing. The
    /// predicate joins through `via_node`, so an endpoint seen only by a node the caller cannot see
    /// is not listed — otherwise the list would leak the existence of segments outside the scope.
    ///
    /// A row whose `via_node` has been deleted is visible **only** to an unrestricted caller: there
    /// is no node left to resolve a group from, and guessing either way would be a decision about
    /// somebody else's visibility.
    pub async fn list_page(
        &self,
        groups: Option<&[Uuid]>,
        via_node: Option<Uuid>,
        include_promoted: bool,
        before: Option<(DateTime<Utc>, Uuid)>,
        limit: i64,
    ) -> anyhow::Result<Vec<DiscoveredEndpoint>> {
        // One statement with nullable bind parameters rather than four assembled shapes: the filters
        // are independent, and a builder would put caller-supplied values next to a format string.
        let rows = sqlx::query(
            "SELECT d.id, host(d.ip) AS ip, d.mac, d.via_node, d.via_ifindex, d.name, \
                    d.evidence, d.first_seen, d.last_seen, d.promoted_node_id \
             FROM l3_discovered d \
             LEFT JOIN nodes n ON n.id = d.via_node \
             WHERE ($1::UUID[] IS NULL OR n.group_id = ANY($1)) \
               AND ($2::UUID IS NULL OR d.via_node = $2) \
               AND ($3::BOOL OR d.promoted_node_id IS NULL) \
               AND ($4::TIMESTAMPTZ IS NULL OR (d.last_seen, d.id) < ($4, $5)) \
             ORDER BY d.last_seen DESC, d.id DESC LIMIT $6",
        )
        .bind(groups.map(<[Uuid]>::to_vec))
        .bind(via_node)
        .bind(include_promoted)
        .bind(before.map(|(at, _)| at))
        .bind(before.map(|(_, id)| id))
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;

        rows.iter().map(endpoint_from_row).collect()
    }

    /// Which of `observers` a caller scoped to `groups` may see (ADR-179 増分 4) — one read for a
    /// whole page's evidence. Only a scoped caller asks: an unrestricted one sees every observer.
    pub async fn visible_observers(
        &self,
        observers: &[Uuid],
        groups: &[Uuid],
    ) -> anyhow::Result<BTreeSet<Uuid>> {
        if observers.is_empty() || groups.is_empty() {
            return Ok(BTreeSet::new());
        }
        let ids: Vec<Uuid> =
            sqlx::query_scalar("SELECT id FROM nodes WHERE id = ANY($1) AND group_id = ANY($2)")
                .bind(observers)
                .bind(groups)
                .fetch_all(&self.pool)
                .await?;
        Ok(ids.into_iter().collect())
    }

    /// How many endpoints the caller can see that are still unmonitored — the Discovery tab's count
    /// (ADR-179 決定 8). The same scope predicate as [`Self::list_page`], so the number and the list
    /// cannot disagree about what the caller may see.
    pub async fn unmonitored_total(&self, groups: Option<&[Uuid]>) -> anyhow::Result<i64> {
        let row = sqlx::query(
            "SELECT count(*)::BIGINT AS n \
             FROM l3_discovered d \
             LEFT JOIN nodes n ON n.id = d.via_node \
             WHERE ($1::UUID[] IS NULL OR n.group_id = ANY($1)) \
               AND d.promoted_node_id IS NULL",
        )
        .bind(groups.map(<[Uuid]>::to_vec))
        .fetch_one(&self.pool)
        .await?;
        Ok(row.try_get("n")?)
    }

    /// Which of `addresses` the caller would find on the Discovery ▸ Unregistered list (ADR-180) —
    /// the same scope predicate and the same "not yet imported" rule as [`Self::list_page`], so the
    /// Neighbors tab never links to a row the list will not show. Each address comes back with its
    /// row's id — `ip` is unique on the table — so the tab can act on the row too (ADR-179 増分 3).
    pub async fn listed_among(
        &self,
        addresses: &[IpAddr],
        groups: Option<&[Uuid]>,
    ) -> anyhow::Result<BTreeMap<IpAddr, Uuid>> {
        if addresses.is_empty() {
            return Ok(BTreeMap::new());
        }
        let text: Vec<String> = addresses.iter().map(ToString::to_string).collect();
        let rows = sqlx::query(
            "SELECT d.id, host(d.ip) AS ip \
             FROM l3_discovered d \
             LEFT JOIN nodes n ON n.id = d.via_node \
             WHERE ($1::UUID[] IS NULL OR n.group_id = ANY($1)) \
               AND d.promoted_node_id IS NULL \
               AND d.ip = ANY($2::text[]::inet[])",
        )
        .bind(groups.map(<[Uuid]>::to_vec))
        .bind(&text)
        .fetch_all(&self.pool)
        .await?;
        let mut out = BTreeMap::new();
        for r in rows {
            let id: Uuid = r.try_get("id")?;
            let ip: Option<String> = r.try_get("ip")?;
            if let Some(ip) = ip.and_then(|s| s.parse::<IpAddr>().ok()) {
                out.insert(ip, id);
            }
        }
        Ok(out)
    }

    /// One endpoint by id, if the caller can see it — what the import and probe handlers read
    /// before acting on a row (ADR-179 増分 2). The same scope predicate as [`Self::list_page`], so
    /// an id is actionable exactly when its row is listable: a row outside the scope reads as
    /// absent, never as "exists, but not yours".
    pub async fn get(
        &self,
        id: Uuid,
        groups: Option<&[Uuid]>,
    ) -> anyhow::Result<Option<DiscoveredEndpoint>> {
        let row = sqlx::query(
            "SELECT d.id, host(d.ip) AS ip, d.mac, d.via_node, d.via_ifindex, d.name, \
                    d.evidence, d.first_seen, d.last_seen, d.promoted_node_id \
             FROM l3_discovered d \
             LEFT JOIN nodes n ON n.id = d.via_node \
             WHERE d.id = $1 AND ($2::UUID[] IS NULL OR n.group_id = ANY($2))",
        )
        .bind(id)
        .bind(groups.map(<[Uuid]>::to_vec))
        .fetch_optional(&self.pool)
        .await?;
        row.as_ref().map(endpoint_from_row).transpose()
    }
}

/// One `l3_discovered` row, as both readers project it.
fn endpoint_from_row(row: &sqlx::postgres::PgRow) -> anyhow::Result<DiscoveredEndpoint> {
    let ip: String = row.try_get("ip")?;
    let via_node: Option<Uuid> = row.try_get("via_node")?;
    let via_ifindex = row
        .try_get::<Option<i32>, _>("via_ifindex")?
        .and_then(|v| u32::try_from(v).ok());
    // A document that will not parse reads as no evidence rather than failing the page: only the
    // sweep writes the column, so such a row came from a newer core naming a source this one does
    // not know — and the fallback below still says truthfully how rows were first found.
    let mut evidence: Vec<EndpointEvidence> = row
        .try_get::<Json<Vec<EndpointEvidence>>, _>("evidence")
        .map(|j| j.0)
        .unwrap_or_default();
    if evidence.is_empty() {
        // Written before ADR-179, or by an older core's sweep, which only ever read ARP.
        evidence.push(EndpointEvidence {
            source: EndpointSource::Arp,
            via_node,
            via_ifindex,
            port: None,
            detail: None,
        });
    }
    Ok(DiscoveredEndpoint {
        id: row.try_get("id")?,
        // A row whose address will not parse should not fail the page; `0.0.0.0` is visibly wrong
        // rather than silently absent, and the column is INET so this is unreachable short of a
        // manual edit.
        //
        // 🚨 It was reachable on **every** row until the projection became `host(ip)`: `ip::TEXT`
        // renders an `inet` with its masklen, which does not parse, so the whole list read
        // `0.0.0.0`. See `known_addresses`.
        ip: ip
            .parse()
            .unwrap_or(IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED)),
        mac: row.try_get("mac")?,
        via_node: via_node.map(NodeId),
        via_ifindex,
        name: row.try_get("name")?,
        evidence,
        first_seen: row.try_get("first_seen")?,
        last_seen: row.try_get("last_seen")?,
        promoted_node_id: row
            .try_get::<Option<Uuid>, _>("promoted_node_id")?
            .map(NodeId),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use yagra_common::ArpEntry;

    /// This module's code, with its test items and comments dropped — the reader every
    /// SQL-shape assertion below uses. The upsert's `first_seen` rule, the scope predicate and
    /// the keyset cursor live entirely inside SQL strings, so nothing else can catch a rewrite
    /// that changes their meaning; the peer stores pin their statements the same way.
    ///
    /// ⚠️ **Read through `module_source`, never `include_str!`** (ADR-102). The raw file includes
    /// this test module, so a positive `contains("<literal>")` was satisfied by the needle's own
    /// line and could not fail. Thirty-two of those were live across seven modules — all of them
    /// here, because the negated side already read this function and only the positive side was
    /// left on the raw text. Loud on one side and silent on the other is why they survived
    /// ADR-091's sweep.
    fn production_source() -> String {
        crate::module_source::code_no_comments("src", "arp")
    }

    fn node(n: u128) -> NodeId {
        NodeId(Uuid::from_u128(n))
    }
    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    fn summary(entries: &[(u32, &str)]) -> ArpSummary {
        ArpSummary::new(
            entries
                .iter()
                .map(|(ifidx, addr)| ArpEntry::new(*ifidx, ip(addr)))
                .collect(),
            false,
        )
    }

    #[test]
    fn an_address_the_fleet_already_monitors_is_not_a_discovery() {
        let known: BTreeSet<IpAddr> = [ip("192.168.1.1"), ip("192.168.1.3")].into_iter().collect();
        let found = unmonitored(
            &[(
                node(1),
                summary(&[(8, "192.168.1.1"), (8, "192.168.1.3"), (8, "192.168.1.50")]),
            )],
            &known,
        );
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].ip, ip("192.168.1.50"));
        assert_eq!(found[0].via_ifindex, Some(8));
    }

    #[test]
    fn a_routers_own_lan_address_is_not_reported_as_unmonitored() {
        // The false positive that would make the list unreadable on day one. The router is monitored
        // on 10.0.0.1 and answers ARP for its LAN interface 192.168.1.1 — which is in `node_l3`, not
        // in `nodes.address`. Feeding only node addresses here would report every router's every
        // interface as a discovery.
        let node_addresses: BTreeSet<IpAddr> = [ip("10.0.0.1")].into_iter().collect();
        let with_l3: BTreeSet<IpAddr> = [ip("10.0.0.1"), ip("192.168.1.1")].into_iter().collect();
        let obs = [(node(1), summary(&[(8, "192.168.1.1")]))];

        assert_eq!(
            unmonitored(&obs, &node_addresses).len(),
            1,
            "this is the wrong answer, and the reason `known` must include node_l3"
        );
        assert!(unmonitored(&obs, &with_l3).is_empty());
    }

    #[test]
    fn one_endpoint_seen_by_three_routers_is_one_finding() {
        // Redundancy in the network must not inflate the count the feature exists to produce.
        let obs = [
            (node(3), summary(&[(1, "192.168.1.50")])),
            (node(1), summary(&[(2, "192.168.1.50")])),
            (node(2), summary(&[(3, "192.168.1.50")])),
        ];
        let found = unmonitored(&obs, &BTreeSet::new());
        assert_eq!(found.len(), 1);
        // Lowest node id wins, so attribution does not flip between sweeps and make the endpoint
        // look like it is moving around the network.
        assert_eq!(found[0].via_node, Some(node(1)));
        assert_eq!(found[0].via_ifindex, Some(2));
    }

    #[test]
    fn the_sweep_is_order_independent_and_bounded() {
        let mut obs: Vec<(NodeId, ArpSummary)> = (0..40u128)
            .map(|i| {
                let entries: Vec<ArpEntry> = (0..400u32)
                    .map(|j| {
                        #[allow(clippy::cast_possible_truncation)]
                        let addr = IpAddr::V4(std::net::Ipv4Addr::from(
                            (10u32 << 24) + (i as u32) * 400 + j + 1,
                        ));
                        ArpEntry::new(1, addr)
                    })
                    .collect();
                (node(i + 1), ArpSummary::new(entries, false))
            })
            .collect();
        let forward = unmonitored(&obs, &BTreeSet::new());
        obs.reverse();
        let reversed = unmonitored(&obs, &BTreeSet::new());
        assert_eq!(
            forward.len(),
            MAX_DISCOVERED_ENDPOINTS,
            "40 nodes × 400 endpoints must be capped, not stored"
        );
        assert_eq!(
            forward, reversed,
            "which endpoints survive the cap must not depend on read order"
        );
    }

    #[test]
    fn an_endpoint_with_no_mac_still_counts() {
        // An incomplete ARP entry names a host that replied to something. Requiring a MAC would drop
        // exactly the hosts that are hardest to reach — which are the interesting ones.
        let mut entry = ArpEntry::new(4, ip("10.9.9.9"));
        entry.mac = None;
        let found = unmonitored(
            &[(node(1), ArpSummary::new(vec![entry], false))],
            &BTreeSet::new(),
        );
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].mac, None);
    }

    #[test]
    fn first_seen_survives_an_endpoint_being_seen_again() {
        // The column's only purpose is "how long has this host been on the network unmonitored";
        // touching it on every sweep would reset that to five minutes, forever.
        assert!(production_source().contains("last_seen = now()"));
        assert!(
            !production_source().contains("first_seen = now()"),
            "the upsert must never move first_seen"
        );
    }

    #[test]
    fn the_scope_predicate_filters_on_the_observing_nodes_group() {
        // Security-critical: without it a scoped operator reads the addresses of segments they
        // cannot see. The NULL branch is the unrestricted fast path, not a missing filter.
        assert!(production_source().contains("($1::UUID[] IS NULL OR n.group_id = ANY($1))"));
    }

    #[test]
    fn paging_is_keyset_and_never_offset() {
        assert!(production_source().contains("(d.last_seen, d.id) < ($4, $5)"));
        assert!(production_source().contains("ORDER BY d.last_seen DESC, d.id DESC LIMIT"));
        assert!(
            !production_source().contains("OFFSET"),
            "OFFSET paging — rows shift under the reader as the sweep updates last_seen"
        );
    }

    /// The promotion statement spells out the device predicate instead of interpolating it (this
    /// module refuses `format!`), so the two copies are compared here — a URL monitor excluded on
    /// the scan table but not on this one is the disagreement ADR-139 決定 1 exists to prevent.
    #[test]
    fn the_promotion_statement_ends_in_the_device_predicate() {
        let tail = format!("AND {}", crate::repo::NodeRepo::DEVICE_NODE_PREDICATE);
        assert!(
            DiscoveredRepo::RECONCILE_PROMOTIONS.ends_with(&tail),
            "RECONCILE_PROMOTIONS no longer ends in NodeRepo::DEVICE_NODE_PREDICATE:\n{}\n--- expected tail ---\n{tail}",
            DiscoveredRepo::RECONCILE_PROMOTIONS
        );
    }

    #[test]
    fn every_statement_binds_its_values_instead_of_interpolating_them() {
        let src = production_source();
        for builder in ["format!(", "push_str("] {
            assert!(
                !src.contains(builder),
                "SQL may be being built by string concatenation ({builder}); bind the value instead"
            );
        }
    }

    #[test]
    fn the_stored_key_is_the_models_own_content_key() {
        assert!(production_source().contains("summary.content_key()"));
    }

    #[test]
    fn the_sweep_has_an_observation_watermark_to_trigger_on() {
        // Also its off switch: no ARP rows ⇒ no watermark ⇒ the sweep returns before it reads the
        // inventory. A deployment that never opted in must not pay for the feature.
        assert!(production_source().contains("SELECT max(last_seen) AS w FROM node_arp"));
    }

    #[test]
    fn the_retention_window_is_short_and_the_cap_is_finite() {
        // Tripwires, not tautologies. Lengthening the window turns a review list into a second
        // inventory of every laptop that ever joined the wifi.
        assert_eq!(DISCOVERED_RETENTION_SECS, 7 * 86_400);
        const { assert!(MAX_DISCOVERED_ENDPOINTS <= 10_000) };
        // And the ARP cadence stays in the band the API edge enforces for the other two walks.
        assert!(crate::neighbors::interval_in_bounds(
            DEFAULT_ARP_INTERVAL_SECS
        ));
        const { assert!(DEFAULT_ARP_INTERVAL_SECS > crate::neighbors::DEFAULT_NEIGHBOR_INTERVAL_SECS) };
    }

    /// **An address leaves PostgreSQL through `host()`, never through a cast to text.**
    ///
    /// `inet`'s text output carries the masklen (`10.0.0.1/32`) while its default display does not,
    /// so `address::TEXT` looks right in psql and hands Rust a string `IpAddr::from_str` rejects.
    /// All three statements here had it, and all three failed silently: node addresses vanished
    /// from the known set, and every listed endpoint read `0.0.0.0`.
    ///
    /// The database tests below are the stronger check — this one exists to name the trap in the
    /// place someone would reintroduce it, and to fail rather than merely be un-read.
    #[test]
    fn an_address_is_read_through_host_and_never_cast_to_text() {
        let src = production_source();
        // Four since ADR-180 added `listed_among`.
        assert_eq!(
            src.matches("host(").count(),
            4,
            "the four address projections are no longer reading through `host()`"
        );
        for needle in ["address::TEXT", "ip::TEXT"] {
            assert!(
                !src.contains(needle),
                "{needle} renders an inet with its masklen, which does not parse as an address"
            );
        }
    }

    // --- The other signals (ADR-179) ------------------------------------------------------------

    fn lldp(
        local_port: &str,
        mgmt: Option<&str>,
        sys_name: Option<&str>,
    ) -> yagra_common::Neighbor {
        let mut n = yagra_common::Neighbor::new(
            NeighborProto::Lldp,
            local_port,
            "00:11:22:00:00:01",
            "Gi1/0/48",
        );
        n.remote_mgmt_addr = mgmt.map(str::to_owned);
        n.remote_sys_name = sys_name.map(str::to_owned);
        n
    }

    fn neighbors(node_id: NodeId, list: Vec<yagra_common::Neighbor>) -> (NodeId, NeighborSet) {
        (node_id, NeighborSet::new(list))
    }

    fn routing(node_id: NodeId, list: &[(RoutingProto, &str)]) -> (NodeId, RoutingSnapshot) {
        (
            node_id,
            RoutingSnapshot::new(
                list.iter()
                    .map(|(p, a)| yagra_common::RoutingAdjacency::new(*p, ip(a)))
                    .collect(),
                false,
            ),
        )
    }

    #[test]
    fn an_lldp_neighbour_with_a_management_address_is_a_candidate_with_its_name_and_port() {
        let mut nb = lldp("Gi1/0/1", Some("192.0.2.20"), Some("sw-07"));
        nb.remote_platform = Some("C9300-48P".to_owned());
        let nbs = [neighbors(node(1), vec![nb])];
        let found = candidates(
            &Signals {
                neighbors: &nbs,
                ..Signals::default()
            },
            &BTreeSet::new(),
        );
        assert_eq!(found.len(), 1);
        let e = &found[0];
        assert_eq!(e.ip, ip("192.0.2.20"));
        assert_eq!(e.name.as_deref(), Some("sw-07"));
        assert_eq!(e.via_node, Some(node(1)));
        assert_eq!(e.mac, None, "a MAC comes from ARP only");
        assert_eq!(
            e.evidence,
            vec![EndpointEvidence {
                source: EndpointSource::Lldp,
                via_node: Some(node(1).as_uuid()),
                via_ifindex: None,
                port: Some("Gi1/0/1".to_owned()),
                detail: Some("C9300-48P".to_owned()),
            }]
        );
    }

    #[test]
    fn a_neighbour_that_is_only_a_phone_or_a_host_is_not_a_candidate() {
        let mut phone = lldp("Gi1/0/2", Some("192.0.2.31"), None);
        phone.capabilities = vec![NeighborCapability::Phone];
        let mut host = lldp("Gi1/0/3", Some("192.0.2.32"), None);
        host.capabilities = vec![NeighborCapability::Host];
        // A phone that is also a bridge is network equipment enough to keep; one that says
        // nothing about itself is kept too, because silence is not evidence of an end station.
        let mut phone_bridge = lldp("Gi1/0/4", Some("192.0.2.33"), None);
        phone_bridge.capabilities = vec![NeighborCapability::Phone, NeighborCapability::Bridge];
        let silent = lldp("Gi1/0/5", Some("192.0.2.34"), None);
        let nbs = [neighbors(node(1), vec![phone, host, phone_bridge, silent])];
        let found: Vec<IpAddr> = candidates(
            &Signals {
                neighbors: &nbs,
                ..Signals::default()
            },
            &BTreeSet::new(),
        )
        .into_iter()
        .map(|e| e.ip)
        .collect();
        assert_eq!(found, vec![ip("192.0.2.33"), ip("192.0.2.34")]);
    }

    #[test]
    fn a_neighbour_with_no_usable_address_or_a_known_one_is_not_a_candidate() {
        let known: BTreeSet<IpAddr> = [ip("192.0.2.40")].into_iter().collect();
        let nbs = [neighbors(
            node(1),
            vec![
                lldp("Gi1/0/1", None, Some("no-address")),
                lldp("Gi1/0/2", Some("not an address"), None),
                lldp("Gi1/0/3", Some("0.0.0.0"), None),
                lldp("Gi1/0/4", Some("127.0.0.1"), None),
                // Another interface of a monitored device: `known` carries node_l3 for this.
                lldp("Gi1/0/5", Some("192.0.2.40"), None),
            ],
        )];
        assert!(candidates(
            &Signals {
                neighbors: &nbs,
                ..Signals::default()
            },
            &known,
        )
        .is_empty());
    }

    #[test]
    fn a_cdp_neighbour_is_named_by_its_device_id() {
        let mut nb = yagra_common::Neighbor::new(
            NeighborProto::Cdp,
            "GigabitEthernet0/1",
            "rt-02.example.com",
            "GigabitEthernet0/0",
        );
        nb.remote_mgmt_addr = Some("192.0.2.50".to_owned());
        nb.local_ifindex = Some(3);
        let nbs = [neighbors(node(2), vec![nb])];
        let found = candidates(
            &Signals {
                neighbors: &nbs,
                ..Signals::default()
            },
            &BTreeSet::new(),
        );
        assert_eq!(found[0].name.as_deref(), Some("rt-02.example.com"));
        assert_eq!(found[0].evidence[0].source, EndpointSource::Cdp);
        assert_eq!(found[0].via_ifindex, Some(3));
    }

    #[test]
    fn ospf_and_bgp_peers_are_candidates_and_the_route_probe_is_not() {
        let rts = [routing(
            node(1),
            &[
                (RoutingProto::Ospf, "198.51.100.1"),
                (RoutingProto::Bgp, "198.51.100.2"),
                (RoutingProto::Route, "198.51.100.3"),
                (RoutingProto::Ospf, "0.0.0.0"),
            ],
        )];
        let found = candidates(
            &Signals {
                routing: &rts,
                ..Signals::default()
            },
            &BTreeSet::new(),
        );
        let got: Vec<(IpAddr, EndpointSource)> =
            found.iter().map(|e| (e.ip, e.evidence[0].source)).collect();
        assert_eq!(
            got,
            vec![
                (ip("198.51.100.1"), EndpointSource::Ospf),
                (ip("198.51.100.2"), EndpointSource::Bgp),
            ]
        );
    }

    #[test]
    fn a_sender_has_no_observing_node_and_is_named_by_its_hostname() {
        let senders = [
            SenderObservation {
                ip: ip("203.0.113.9"),
                kind: SenderKind::Syslog,
                hostname: Some("fw-01".to_owned()),
            },
            SenderObservation {
                ip: ip("203.0.113.9"),
                kind: SenderKind::Trap,
                hostname: None,
            },
        ];
        let found = candidates(
            &Signals {
                senders: &senders,
                ..Signals::default()
            },
            &BTreeSet::new(),
        );
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].via_node, None);
        assert_eq!(found[0].name.as_deref(), Some("fw-01"));
        let sources: Vec<EndpointSource> = found[0].evidence.iter().map(|e| e.source).collect();
        assert_eq!(sources, vec![EndpointSource::Syslog, EndpointSource::Trap]);
    }

    #[test]
    fn one_address_seen_several_ways_is_one_row_with_every_piece_of_evidence() {
        let arp = [(node(5), {
            let mut s = summary(&[(7, "192.0.2.60")]);
            s.entries[0].mac = Some("00:11:22:33:44:55".to_owned());
            s
        })];
        let nbs = [neighbors(
            node(3),
            vec![lldp("Gi1/0/9", Some("192.0.2.60"), Some("sw-09"))],
        )];
        let senders = [SenderObservation {
            ip: ip("192.0.2.60"),
            kind: SenderKind::Syslog,
            hostname: Some("from-syslog".to_owned()),
        }];
        let found = candidates(
            &Signals {
                arp: &arp,
                neighbors: &nbs,
                senders: &senders,
                ..Signals::default()
            },
            &BTreeSet::new(),
        );
        assert_eq!(found.len(), 1);
        let e = &found[0];
        let sources: Vec<EndpointSource> = e.evidence.iter().map(|v| v.source).collect();
        assert_eq!(
            sources,
            vec![
                EndpointSource::Arp,
                EndpointSource::Lldp,
                EndpointSource::Syslog
            ],
            "listed in source order, whatever order the inputs came in"
        );
        // The lowest observer across every source — node 3 saw it by LLDP, node 5 by ARP.
        assert_eq!(e.via_node, Some(node(3)));
        assert_eq!(e.mac.as_deref(), Some("00:11:22:33:44:55"));
        assert_eq!(
            e.name.as_deref(),
            Some("sw-09"),
            "LLDP's name outranks a syslog hostname"
        );
    }

    #[test]
    fn hidden_observers_lose_their_evidence_and_a_senders_own_stays() {
        let seen = |n: u128, source| EndpointEvidence {
            source,
            via_node: Some(node(n).as_uuid()),
            via_ifindex: Some(7),
            port: Some("Gi1/0/7".into()),
            detail: Some("C9300-48P".into()),
        };
        let sender = EndpointEvidence {
            source: EndpointSource::Syslog,
            via_node: None,
            via_ifindex: None,
            port: None,
            detail: Some("host-a".into()),
        };
        let mut evidence = vec![
            seen(1, EndpointSource::Arp),
            seen(2, EndpointSource::Lldp),
            sender.clone(),
        ];
        let visible: BTreeSet<Uuid> = [node(1).as_uuid()].into_iter().collect();
        drop_hidden_evidence(&mut evidence, &visible);
        assert_eq!(evidence, vec![seen(1, EndpointSource::Arp), sender]);

        let mut none_visible = vec![seen(2, EndpointSource::Lldp)];
        drop_hidden_evidence(&mut none_visible, &BTreeSet::new());
        assert!(
            none_visible.is_empty(),
            "nothing about a hidden observer is kept"
        );
    }

    #[test]
    fn forged_senders_cannot_push_an_observed_endpoint_off_the_list() {
        // More low-address senders than the list holds, and one real device a router saw at a
        // higher address. Ordered by address alone, the senders filled the list and the device
        // was cut (ADR-179 増分 5 決定 3).
        let senders: Vec<SenderObservation> = (0..=MAX_DISCOVERED_ENDPOINTS as u32)
            .map(|i| SenderObservation {
                ip: IpAddr::from(std::net::Ipv4Addr::from(0x0a00_0000 + i)),
                kind: SenderKind::Syslog,
                hostname: None,
            })
            .collect();
        let arp = [(node(1), summary(&[(1, "192.0.2.70")]))];
        let found = candidates(
            &Signals {
                arp: &arp,
                senders: &senders,
                ..Signals::default()
            },
            &BTreeSet::new(),
        );
        assert_eq!(found.len(), MAX_DISCOVERED_ENDPOINTS);
        assert!(
            found.iter().any(|e| e.ip.to_string() == "192.0.2.70"),
            "the device a router saw survives the flood"
        );
        assert!(
            found.windows(2).all(|w| w[0].ip < w[1].ip),
            "the list is still in address order"
        );
    }

    #[test]
    fn only_a_sender_vouching_is_what_makes_a_row_untouchable() {
        let ev = |source, via: Option<u128>| EndpointEvidence {
            source,
            via_node: via.map(|n| node(n).as_uuid()),
            via_ifindex: None,
            port: None,
            detail: None,
        };
        assert!(only_senders_vouch(&[ev(EndpointSource::Syslog, None)]));
        assert!(only_senders_vouch(&[
            ev(EndpointSource::Syslog, None),
            ev(EndpointSource::Trap, None)
        ]));
        assert!(!only_senders_vouch(&[
            ev(EndpointSource::Lldp, Some(1)),
            ev(EndpointSource::Syslog, None)
        ]));
        // An observation whose node has since been deleted is still a device's report, not the
        // address's own say-so.
        assert!(!only_senders_vouch(&[ev(EndpointSource::Arp, None)]));
        assert!(!only_senders_vouch(&[]));
    }

    #[test]
    fn evidence_is_capped_per_endpoint() {
        let arp: Vec<(NodeId, ArpSummary)> = (1..=20u128)
            .map(|n| (node(n), summary(&[(1, "192.0.2.70")])))
            .collect();
        let found = candidates(
            &Signals {
                arp: &arp,
                ..Signals::default()
            },
            &BTreeSet::new(),
        );
        assert_eq!(found[0].evidence.len(), MAX_EVIDENCE_PER_ENDPOINT);
        assert_eq!(found[0].evidence[0].via_node, Some(node(1).as_uuid()));
    }

    #[test]
    fn every_source_token_is_its_serde_tag() {
        for s in EndpointSource::ALL {
            assert_eq!(
                serde_json::to_value(s).unwrap(),
                serde_json::Value::String(s.as_str().to_owned())
            );
        }
        for k in [SenderKind::Syslog, SenderKind::Trap] {
            assert_eq!(SenderKind::from_token(k.as_str()), Some(k));
            assert_eq!(k.source().as_str(), k.as_str());
        }
    }

    // --- Running the SQL, not reading it (ADR-114/116) -----------------------------------------
    //
    // Everything above is either the pure rule (`unmonitored`) or a reading of this module's text.
    // Neither can say whether the eleven statements do what the words claim, and one of the two
    // stores here is the only one in ADR-043 whose reader joins to a *second* table — which is
    // exactly where a scope predicate goes wrong quietly.
    use crate::l3::L3Repo;
    use crate::pgtest;
    use yagra_common::{L3Address, L3Snapshot};

    /// A node with an address of the test's choosing, through the production writer.
    async fn node_at(pool: &sqlx::PgPool, name: &str, addr: &str) -> Uuid {
        pgtest::repo(pool.clone())
            .create_node(name, ip(addr), None, None, None, None, None, None)
            .await
            .expect("create node")
    }

    fn observation(addr: &str, via: Uuid, ifindex: u32) -> EndpointObservation {
        EndpointObservation {
            ip: ip(addr),
            mac: Some("aa:bb:cc:dd:ee:ff".to_owned()),
            via_node: Some(NodeId(via)),
            via_ifindex: Some(ifindex),
            name: None,
            evidence: vec![EndpointEvidence {
                source: EndpointSource::Arp,
                via_node: Some(via),
                via_ifindex: Some(ifindex),
                port: None,
                detail: None,
            }],
        }
    }

    /// A summary goes in whole, comes back whole, and the coverage line counts it — including the
    /// truncation flag, which is what says the endpoint list is a sample rather than the network.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn an_observed_summary_reads_back_and_the_totals_count_it(pool: sqlx::PgPool) {
        let repo = ArpRepo::new(pool.clone());
        assert!(
            repo.observation_watermark()
                .await
                .expect("watermark")
                .is_none(),
            "a fresh database reported an ARP walk that never happened"
        );
        assert_eq!(repo.totals().await.expect("totals"), (0, 0, 0));

        let a = pgtest::node(&pool, "a", 1, None).await;
        let b = pgtest::node(&pool, "b", 2, None).await;
        let seen_by_a = summary(&[(8, "192.168.50.10"), (8, "192.168.50.11")]);
        let seen_by_b = ArpSummary::new(vec![ArpEntry::new(9, ip("192.168.50.12"))], true);
        repo.record_observation(a, &seen_by_a).await.expect("a");
        repo.record_observation(b, &seen_by_b).await.expect("b");

        let all = repo.all_current().await.expect("all_current");
        assert_eq!(all.len(), 2, "the unpaged read did not return every node");
        assert_eq!(
            all.iter().find(|(id, _)| id.0 == a).map(|(_, s)| s),
            Some(&seen_by_a),
            "a summary came back attached to the wrong node, or not at all"
        );
        assert_eq!(
            all.iter().find(|(id, _)| id.0 == b).map(|(_, s)| s),
            Some(&seen_by_b)
        );

        assert_eq!(
            repo.totals().await.expect("totals"),
            (3, 2, 1),
            "the coverage line does not agree with what was stored (observed, nodes, truncated)"
        );
        assert_eq!(
            repo.observation_watermark().await.expect("watermark"),
            Some(pgtest::node_timestamp(&pool, "node_arp", "last_seen", b).await),
            "the watermark is not the newest last_seen in the table"
        );
    }

    /// `first_seen` answers "this port has looked like this for three weeks", so an unchanged walk
    /// must not restart it and a changed one must.
    ///
    /// ⚠️ Read through [`pgtest::node_timestamp`] because `node_arp.first_seen` has **no reader in
    /// production** — the column is written by this statement and consulted by nothing else, so
    /// without the fixture the rule is only assertable as text.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn an_unchanged_walk_keeps_first_seen_and_a_changed_one_restarts_it(pool: sqlx::PgPool) {
        let node = pgtest::node(&pool, "rtr", 1, None).await;
        let repo = ArpRepo::new(pool.clone());
        let held = summary(&[(8, "192.168.50.10")]);
        repo.record_observation(node, &held).await.expect("first");
        let started = pgtest::node_timestamp(&pool, "node_arp", "first_seen", node).await;

        repo.record_observation(node, &held).await.expect("same");
        assert_eq!(
            pgtest::rows(&pool, "node_arp").await,
            1,
            "the summary was stored twice — the conflict target is not the node"
        );
        assert_eq!(
            pgtest::node_timestamp(&pool, "node_arp", "first_seen", node).await,
            started,
            "an unchanged walk restarted the clock, so every endpoint looks new every six hours"
        );
        assert!(
            pgtest::node_timestamp(&pool, "node_arp", "last_seen", node).await > started,
            "last_seen did not move, so the sweep would never trigger again"
        );

        repo.record_observation(node, &summary(&[(8, "192.168.50.99")]))
            .await
            .expect("changed");
        assert!(
            pgtest::node_timestamp(&pool, "node_arp", "first_seen", node).await > started,
            "first_seen did not restart when the walk actually saw something else"
        );
    }

    /// **The false positive the sweep exists to avoid.** A router monitored on its management
    /// address answers ARP for its LAN interface too, so the known set has to carry every reported
    /// interface address as well as every node address — otherwise the router's own gateway
    /// address is reported as an unmonitored endpoint on every segment it terminates.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn known_addresses_covers_node_addresses_and_reported_interface_addresses(
        pool: sqlx::PgPool,
    ) {
        let rtr = node_at(&pool, "rtr", "10.20.0.1").await;
        L3Repo::new(pool.clone())
            .record_observation(
                rtr,
                &L3Snapshot::new(vec![L3Address::new(8, ip("192.168.50.1"), 24)]),
            )
            .await
            .expect("record l3");

        let known = DiscoveredRepo::new(pool.clone())
            .known_addresses()
            .await
            .expect("known");
        assert!(
            known.contains(&ip("10.20.0.1")),
            "the node's own address is not in the known set: {known:?}"
        );
        assert!(
            known.contains(&ip("192.168.50.1")),
            "a reported interface address is not in the known set, so the router's own gateway \
             address would be reported as an unmonitored endpoint: {known:?}"
        );
        assert!(
            !known.contains(&ip("192.168.50.77")),
            "an address nobody reported is in the known set, which would hide real endpoints"
        );
    }

    /// An endpoint seen again keeps `first_seen` — how long it was on the network before anyone
    /// monitored it — and takes the newer observation for everything else.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn an_endpoint_seen_again_keeps_first_seen_and_takes_the_new_observation(
        pool: sqlx::PgPool,
    ) {
        let a = pgtest::node(&pool, "a", 1, None).await;
        let b = pgtest::node(&pool, "b", 2, None).await;
        let repo = DiscoveredRepo::new(pool.clone());

        assert_eq!(
            repo.upsert_batch(&[]).await.expect("empty"),
            0,
            "an empty sweep wrote something"
        );
        assert_eq!(
            repo.upsert_batch(&[observation("192.168.50.10", a, 8)])
                .await
                .expect("upsert"),
            1
        );
        let first = repo
            .list_page(None, None, false, None, 10)
            .await
            .expect("list")
            .remove(0);
        assert_eq!(first.ip, ip("192.168.50.10"));
        assert_eq!(first.via_node, Some(NodeId(a)));
        assert_eq!(first.via_ifindex, Some(8));
        assert_eq!(first.mac.as_deref(), Some("aa:bb:cc:dd:ee:ff"));
        assert_eq!(first.promoted_node_id, None);
        assert_eq!(
            first.first_seen, first.last_seen,
            "an endpoint seen once already looks re-observed"
        );

        let mut again = observation("192.168.50.10", b, 3);
        again.mac = None;
        repo.upsert_batch(&[again]).await.expect("re-upsert");
        assert_eq!(
            pgtest::rows(&pool, "l3_discovered").await,
            1,
            "the same endpoint stored twice — the identity is not the address"
        );
        let second = repo
            .list_page(None, None, false, None, 10)
            .await
            .expect("list")
            .remove(0);
        assert_eq!(second.id, first.id);
        assert_eq!(
            second.first_seen, first.first_seen,
            "first_seen moved, so 'how long was this here unmonitored' is now wrong"
        );
        assert!(second.last_seen > first.last_seen);
        assert_eq!(second.via_node, Some(NodeId(b)));
        assert_eq!(second.via_ifindex, Some(3));
        assert_eq!(second.mac, None, "the conflict path did not update the row");
    }

    /// An endpoint that becomes a node stops being a finding — and asking twice changes nothing,
    /// which is what the `IS DISTINCT FROM` guard is for.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn promotion_points_the_row_at_the_node_that_now_monitors_it(pool: sqlx::PgPool) {
        let via = pgtest::node(&pool, "rtr", 1, None).await;
        let repo = DiscoveredRepo::new(pool.clone());
        repo.upsert_batch(&[
            observation("192.168.50.10", via, 8),
            observation("192.168.50.11", via, 8),
        ])
        .await
        .expect("upsert");
        assert_eq!(
            repo.reconcile_promotions().await.expect("reconcile"),
            0,
            "an endpoint nobody monitors was reported as promoted"
        );

        let host = node_at(&pool, "host", "192.168.50.10").await;
        assert_eq!(repo.reconcile_promotions().await.expect("reconcile"), 1);
        assert_eq!(
            repo.reconcile_promotions().await.expect("reconcile"),
            0,
            "reconciling twice rewrote a row that was already correct"
        );

        let listed = repo
            .list_page(None, None, false, None, 10)
            .await
            .expect("list");
        assert_eq!(
            listed.len(),
            1,
            "a promoted endpoint is still in the unmonitored list"
        );
        assert_eq!(listed[0].ip, ip("192.168.50.11"));

        let with_promoted = repo
            .list_page(None, None, true, None, 10)
            .await
            .expect("list");
        assert_eq!(
            with_promoted.len(),
            2,
            "asking for promoted rows did not bring the promoted one back"
        );
        let promoted = with_promoted
            .iter()
            .find(|e| e.ip == ip("192.168.50.10"))
            .expect("the promoted endpoint");
        assert_eq!(promoted.promoted_node_id, Some(NodeId(host)));

        let fetched = repo
            .get(promoted.id, None)
            .await
            .expect("get")
            .expect("the row just listed");
        assert_eq!(&fetched, promoted, "get and list disagree about one row");
        assert!(
            repo.get(Uuid::new_v4(), None).await.expect("get").is_none(),
            "an unknown id returned an endpoint"
        );
    }

    /// A URL monitor at the endpoint's address does not make the endpoint "monitored"; a device
    /// node at it does (ADR-139 決定 1). The scan table on the same screen asks through the same
    /// predicate, so the two cannot disagree about one address.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn a_url_monitor_at_the_address_does_not_promote_the_endpoint(pool: sqlx::PgPool) {
        let via = pgtest::node(&pool, "rtr", 1, None).await;
        let repo = DiscoveredRepo::new(pool.clone());
        repo.upsert_batch(&[observation("192.168.60.10", via, 8)])
            .await
            .expect("upsert");

        let page = node_at(&pool, "web-page", "192.168.60.10").await;
        sqlx::query("INSERT INTO url_checks (node_id, url) VALUES ($1, 'https://192.168.60.10/')")
            .bind(page)
            .execute(&pool)
            .await
            .expect("url check");
        assert_eq!(
            repo.reconcile_promotions().await.expect("reconcile"),
            0,
            "a URL monitor's resolved address promoted the device behind it"
        );

        let device = node_at(&pool, "device", "192.168.60.10").await;
        assert_eq!(repo.reconcile_promotions().await.expect("reconcile"), 1);
        let row = repo
            .list_page(None, None, true, None, 10)
            .await
            .expect("list")
            .into_iter()
            .find(|e| e.ip == ip("192.168.60.10"))
            .expect("the endpoint");
        assert_eq!(row.promoted_node_id, Some(NodeId(device)));
    }

    /// Pruning drops what aged out, then enforces the ceiling by dropping the **oldest seen** —
    /// so what survives is what is currently on the network.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn pruning_ages_rows_out_then_enforces_the_ceiling_oldest_first(pool: sqlx::PgPool) {
        let via = pgtest::node(&pool, "rtr", 1, None).await;
        let repo = DiscoveredRepo::new(pool.clone());
        // One call each, so `last_seen` genuinely orders them; a single batch stamps one `now()`
        // and the ceiling would then be deciding on the surrogate id alone.
        for addr in ["192.168.50.10", "192.168.50.11", "192.168.50.12"] {
            repo.upsert_batch(&[observation(addr, via, 8)])
                .await
                .expect("upsert");
        }

        assert_eq!(
            repo.prune(3600, 10).await.expect("prune"),
            0,
            "rows seen a moment ago were pruned by an hour-long window under a ceiling of ten"
        );
        assert_eq!(pgtest::rows(&pool, "l3_discovered").await, 3);

        assert_eq!(
            repo.prune(3600, 1).await.expect("prune"),
            2,
            "the ceiling did not take the two rows over it"
        );
        let left = repo
            .list_page(None, None, false, None, 10)
            .await
            .expect("list");
        assert_eq!(left.len(), 1);
        assert_eq!(
            left[0].ip,
            ip("192.168.50.12"),
            "the ceiling kept the oldest-seen row instead of the newest"
        );

        assert_eq!(repo.prune(0, 10).await.expect("prune"), 1);
        assert_eq!(pgtest::rows(&pool, "l3_discovered").await, 0);
    }

    /// **The scope rule, executed**, plus the two filters and the cursor that share its statement.
    ///
    /// The predicate joins through `via_node`, so an endpoint seen only by a node the caller cannot
    /// see must not be listed — otherwise the list leaks the existence of segments outside the
    /// scope.
    /// The Neighbors tab's "listed as unregistered" (ADR-180) answers exactly what the list shows:
    /// the same scope, and an imported row no longer counts.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn listed_among_follows_the_list_scope_and_drops_imported_rows(pool: sqlx::PgPool) {
        let mine = pgtest::group(&pool, "mine").await;
        let theirs = pgtest::group(&pool, "theirs").await;
        let ours = pgtest::node(&pool, "ours", 1, Some(mine)).await;
        let alien = pgtest::node(&pool, "alien", 2, Some(theirs)).await;
        let repo = DiscoveredRepo::new(pool.clone());
        repo.upsert_batch(&[
            observation("192.0.2.10", ours, 8),
            observation("192.0.2.11", alien, 8),
        ])
        .await
        .expect("upsert");
        let asked = [ip("192.0.2.10"), ip("192.0.2.11"), ip("192.0.2.12")];

        let all = repo.listed_among(&asked, None).await.expect("listed");
        assert_eq!(
            all.keys().copied().collect::<BTreeSet<_>>(),
            BTreeSet::from([ip("192.0.2.10"), ip("192.0.2.11")])
        );
        // Each id is the row that address is listed as — what probe and import act on.
        for (addr, id) in &all {
            let row = repo.get(*id, None).await.expect("get").expect("row exists");
            assert_eq!(row.ip, *addr, "{addr} answered another row's id");
        }
        let scoped = repo
            .listed_among(&asked, Some(&[mine]))
            .await
            .expect("listed");
        assert_eq!(
            scoped.keys().copied().collect::<BTreeSet<_>>(),
            BTreeSet::from([ip("192.0.2.10")])
        );

        // Imported: a node now stands at the address, and promotion points the row at it.
        node_at(&pool, "now-a-node", "192.0.2.10").await;
        repo.reconcile_promotions().await.expect("reconcile");
        let after = repo.listed_among(&asked, None).await.expect("listed");
        assert_eq!(
            after.keys().copied().collect::<BTreeSet<_>>(),
            BTreeSet::from([ip("192.0.2.11")])
        );
    }

    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn the_listing_is_scoped_by_the_observing_node_and_pages_by_cursor(pool: sqlx::PgPool) {
        let mine = pgtest::group(&pool, "mine").await;
        let theirs = pgtest::group(&pool, "theirs").await;
        let ours = pgtest::node(&pool, "ours", 1, Some(mine)).await;
        let alien = pgtest::node(&pool, "alien", 2, Some(theirs)).await;
        let repo = DiscoveredRepo::new(pool.clone());
        for (addr, via) in [
            ("192.168.50.10", ours),
            ("192.168.50.11", ours),
            ("192.168.60.10", alien),
        ] {
            repo.upsert_batch(&[observation(addr, via, 8)])
                .await
                .expect("upsert");
        }

        // Acceptance first: a predicate that refuses everything reads exactly like one that works.
        assert_eq!(
            repo.list_page(None, None, false, None, 10)
                .await
                .expect("list")
                .len(),
            3,
            "an unrestricted caller must see every endpoint"
        );

        let scoped = repo
            .list_page(Some(&[mine]), None, false, None, 10)
            .await
            .expect("list");
        assert_eq!(
            scoped.len(),
            2,
            "the scope did not filter on the observing node's group"
        );
        assert!(
            scoped.iter().all(|e| e.via_node == Some(NodeId(ours))),
            "an endpoint seen only outside the scope was listed"
        );
        assert!(
            repo.list_page(Some(&[]), None, false, None, 10)
                .await
                .expect("list")
                .is_empty(),
            "an empty scope matched something"
        );

        let by_observer = repo
            .list_page(None, Some(alien), false, None, 10)
            .await
            .expect("list");
        assert_eq!(by_observer.len(), 1);
        assert_eq!(by_observer[0].ip, ip("192.168.60.10"));

        // ⚠️ The bounded loop is part of the assertion: a cursor that stopped being applied would
        // hand back the same newest row forever, and an unbounded `loop` would hang, not fail.
        let mut seen: Vec<(DateTime<Utc>, Uuid)> = Vec::new();
        let mut before: Option<(DateTime<Utc>, Uuid)> = None;
        for _ in 0..8 {
            let page = repo
                .list_page(None, None, false, before, 1)
                .await
                .expect("list");
            let Some(row) = page.first() else { break };
            assert_eq!(page.len(), 1, "LIMIT is not being applied");
            seen.push((row.last_seen, row.id));
            before = Some((row.last_seen, row.id));
        }
        assert_eq!(
            seen.len(),
            3,
            "the cursor walk did not end after the three endpoints: {seen:?}"
        );
        let mut descending = seen.clone();
        descending.sort_unstable();
        descending.reverse();
        descending.dedup();
        assert_eq!(
            descending, seen,
            "the page did not come back newest-seen first, or a row came back twice: {seen:?}"
        );
    }

    /// Evidence and a name go in through the sweep's writer and come back through both readers; a
    /// row written before ADR-179 reads as the ARP observation it was; and the tab's count uses the
    /// list's scope (ADR-179 決定 1, 8).
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn evidence_and_a_name_round_trip_and_the_count_follows_the_scope(pool: sqlx::PgPool) {
        let mine = pgtest::group(&pool, "mine").await;
        let theirs = pgtest::group(&pool, "theirs").await;
        let ours = pgtest::node(&pool, "ours", 1, Some(mine)).await;
        let alien = pgtest::node(&pool, "alien", 2, Some(theirs)).await;
        let repo = DiscoveredRepo::new(pool.clone());

        let lldp_row = EndpointObservation {
            ip: ip("192.0.2.20"),
            mac: None,
            via_node: Some(NodeId(ours)),
            via_ifindex: None,
            name: Some("sw-07".to_owned()),
            evidence: vec![EndpointEvidence {
                source: EndpointSource::Lldp,
                via_node: Some(ours),
                via_ifindex: None,
                port: Some("Gi1/0/1".to_owned()),
                detail: Some("C9300-48P".to_owned()),
            }],
        };
        let sender_row = EndpointObservation {
            ip: ip("203.0.113.9"),
            mac: None,
            via_node: None,
            via_ifindex: None,
            name: Some("fw-01".to_owned()),
            evidence: vec![EndpointEvidence {
                source: EndpointSource::Syslog,
                via_node: None,
                via_ifindex: None,
                port: None,
                detail: Some("fw-01".to_owned()),
            }],
        };
        repo.upsert_batch(&[
            lldp_row.clone(),
            sender_row,
            observation("192.168.60.10", alien, 8),
        ])
        .await
        .expect("upsert");
        // A row as an older core writes it: no name, no evidence.
        sqlx::query(
            "INSERT INTO l3_discovered (ip, via_node, via_ifindex) VALUES ('192.168.50.99', $1, 4)",
        )
        .bind(ours)
        .execute(&pool)
        .await
        .expect("legacy row");

        let all = repo
            .list_page(None, None, false, None, 10)
            .await
            .expect("list");
        assert_eq!(
            all.len(),
            4,
            "every row must be listed to an unrestricted caller"
        );
        let by_ip = |a: &str| all.iter().find(|e| e.ip == ip(a)).expect("row").clone();

        let got = by_ip("192.0.2.20");
        assert_eq!(got.name.as_deref(), Some("sw-07"));
        assert_eq!(
            got.evidence, lldp_row.evidence,
            "evidence did not round-trip"
        );
        let fetched = repo.get(got.id, None).await.expect("get").expect("row");
        assert_eq!(
            fetched.evidence, lldp_row.evidence,
            "`get` projects differently from the list"
        );

        let legacy = by_ip("192.168.50.99");
        assert_eq!(
            legacy.evidence,
            vec![EndpointEvidence {
                source: EndpointSource::Arp,
                via_node: Some(ours),
                via_ifindex: Some(4),
                port: None,
                detail: None,
            }],
            "a row with no evidence must read as the ARP observation it was"
        );

        assert_eq!(repo.unmonitored_total(None).await.expect("count"), 4);
        // Scoped to `mine`: the LLDP row and the legacy row. The sender has no observer, so a
        // scoped caller cannot see it (決定 7); the alien row is outside the scope.
        assert_eq!(
            repo.unmonitored_total(Some(&[mine])).await.expect("count"),
            2
        );
        let scoped = repo
            .list_page(Some(&[mine]), None, false, None, 10)
            .await
            .expect("list");
        assert_eq!(
            scoped.len(),
            2,
            "the count and the list disagree about the scope"
        );
        assert_eq!(repo.unmonitored_total(Some(&[])).await.expect("count"), 0);
        // `get` answers exactly what the list shows (ADR-179 増分 2): a row the scope hides reads
        // as absent, so an id copied from somebody else's list cannot be acted on.
        let alien_row = by_ip("192.168.60.10");
        assert!(repo
            .get(got.id, Some(&[mine]))
            .await
            .expect("get")
            .is_some());
        assert!(repo
            .get(alien_row.id, Some(&[mine]))
            .await
            .expect("get")
            .is_none());
        assert!(repo
            .get(by_ip("203.0.113.9").id, Some(&[mine]))
            .await
            .expect("get")
            .is_none());

        // A second sweep that no longer sees the name keeps the row and clears the name: the
        // columns are last-observation-wins, like `mac`.
        let mut renamed = lldp_row;
        renamed.name = None;
        repo.upsert_batch(&[renamed]).await.expect("upsert");
        let again = repo
            .list_page(None, None, false, None, 10)
            .await
            .expect("list");
        let row = again
            .iter()
            .find(|e| e.ip == ip("192.0.2.20"))
            .expect("row");
        assert_eq!(row.name, None);
        assert_eq!(
            row.first_seen, got.first_seen,
            "first_seen must survive the upsert"
        );
    }
}
