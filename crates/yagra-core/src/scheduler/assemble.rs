// SPDX-License-Identifier: AGPL-3.0-only
//! Which checks a node gets: the policy, the single-purpose-monitor precedence, the v2c/v3
//! split, and [`assemble_node_jobs`] itself.
//!
//! Needs an already-resolved node and its config — [`dispatch`](super::dispatch) does the
//! resolving. **Never `.await`s** (`guards.rs` enforces it).
//!
//! 🚨 This is where "what gets polled" is decided. Get it wrong and a node is **silently not**
//! **polled** — the failure mode that recurred in v0.2.13.

use crate::l3_routing::RoutingPlan;
use crate::neighbors::AdjacencySettings;
use crate::secrets::SnmpV3Secret;
use std::collections::HashSet;
use std::sync::Arc;
use uuid::Uuid;

// The builders this file chooses between: one file per stage, `checks` makes one check and
// this one decides which checks a node gets (ADR-096).
use super::checks::*;
use super::{SnmpAuth, SNMP_TIMEOUT_MS};
use yagra_bus::{CheckSpec, IcmpCheck, PollJob};
use yagra_common::{
    CollectionItem, DnsCheckConfig, HttpAuth, Node, NodeKind, NodeRows, UrlCheckConfig,
};

/// Whether this deployment collects connectivity data, and how often — resolved once per sweep and
/// passed into [`assemble_node_jobs`] so that function stays pure (no store, no clock).
///
/// One struct for both walks (L2 adjacency, ADR-038; L3 interface addresses, ADR-043) so the sweep
/// resolves settings once. Increment 3's ARP cadence and Increment 4's routing cadence become
/// fields here rather than another settings query in the scheduling loop.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdjacencyPolicy {
    /// Whether CDP/LLDP neighbour jobs are issued.
    pub neighbors_enabled: bool,
    /// Neighbour cadence in seconds. Deliberately slower than the node's metric interval; the
    /// poller's local scheduler honours it per spec, and the legacy publish path gates on it
    /// separately.
    pub neighbors_interval_secs: u32,
    /// Whether interface-address jobs are issued.
    pub l3_enabled: bool,
    /// Interface-address cadence in seconds. Same reasoning as the neighbour cadence: addressing
    /// changes on the order of months.
    pub l3_interval_secs: u32,
    /// Whether ARP / IPv6-neighbour jobs are issued. Off unless an operator turned it on — the one
    /// walk here that costs the device measurable work.
    pub arp_enabled: bool,
    /// ARP cadence in seconds. Slower again than the two above: what it discovers is "which hosts
    /// exist on my segments", which is an inventory question, not a state one.
    pub arp_interval_secs: u32,
    /// Whether routing-adjacency jobs are issued (ADR-043 Increment 4).
    pub routing_enabled: bool,
    /// Routing-adjacency cadence in seconds.
    pub routing_interval_secs: u32,
    /// Whether media-type jobs are issued (ADR-063 Inc.2).
    pub media_enabled: bool,
    /// Media cadence in seconds. Same reasoning as the neighbour cadence: a port's medium changes
    /// when someone swaps a module.
    pub media_interval_secs: u32,
    /// Which host addresses each node is asked to probe for, resolved once per sweep alongside the
    /// settings. Shared rather than cloned per node — it is the same fleet-wide fact for all of
    /// them, and nearly every node's entry is empty.
    pub routing_plan: Arc<RoutingPlan>,
}

impl From<AdjacencySettings> for AdjacencyPolicy {
    /// Settings alone, with an **empty** probe plan.
    ///
    /// The plan needs a database read, so a policy built from settings only issues the two walked
    /// columns and no route probes. Every caller that can afford the read uses
    /// [`PollDispatcher::adjacency_policy`] instead, which fills it in.
    fn from(s: AdjacencySettings) -> Self {
        Self {
            neighbors_enabled: s.neighbors_enabled,
            neighbors_interval_secs: s.neighbors_interval_secs,
            l3_enabled: s.l3_enabled,
            l3_interval_secs: s.l3_interval_secs,
            arp_enabled: s.arp_enabled,
            arp_interval_secs: s.arp_interval_secs,
            routing_enabled: s.routing_enabled,
            routing_interval_secs: s.routing_interval_secs,
            media_enabled: s.media_enabled,
            media_interval_secs: s.media_interval_secs,
            routing_plan: Arc::new(RoutingPlan::default()),
        }
    }
}

impl Default for AdjacencyPolicy {
    fn default() -> Self {
        AdjacencySettings::default().into()
    }
}

/// A node bound to a single-purpose monitor, which replaces the ordinary ICMP/SNMP jobs entirely.
///
/// This is [`NodeKind`] carrying the config the job builder needs. The precedence itself lives in
/// [`NodeKind::resolve`] and nowhere else — it used to be expressed twice here (the load order of
/// [`PollDispatcher::build_scheduled_jobs_hinted`] and again in `assemble_node_jobs`), and a third
/// time as the Meraki short-circuit below, with nothing checking that the three agreed.
// Not `Copy`: the resolved credential is owned (`String`s), and a credential that copies
// implicitly is one that is easy to leave lying around in a second place.
#[derive(Debug, Clone)]
pub enum SpecialMonitor<'a> {
    /// The node has a `url_checks` row: one HTTP job. `auth` is the *resolved* credential, filled
    /// in by [`SpecialMonitor::with_http_auth`] after the store lookup — [`Self::resolve`] is sync
    /// and has no store, so it always produces `None` and the caller upgrades it.
    Url {
        cfg: &'a UrlCheckConfig,
        auth: Option<HttpAuth>,
    },
    /// The node has a `dns_checks` row: one DNS job.
    Dns(&'a DnsCheckConfig),
}

impl<'a> SpecialMonitor<'a> {
    /// The single-purpose monitor a node's rows resolve to, if any.
    ///
    /// Meraki is passed as `false` because **both callers have already excluded Meraki nodes** —
    /// the sweep by its preloaded id set, the on-demand path by its short-circuit — which is what
    /// lets this skip a `meraki_devices` round trip per node per round. It is an invariant of the
    /// call sites, not an assumption about the data.
    #[must_use]
    pub fn resolve(
        url: Option<&'a UrlCheckConfig>,
        dns: Option<&'a DnsCheckConfig>,
    ) -> Option<Self> {
        let rows = NodeRows {
            meraki: false,
            url: url.is_some(),
            dns: dns.is_some(),
        };
        match NodeKind::resolve(rows) {
            NodeKind::Url => url.map(|cfg| Self::Url { cfg, auth: None }),
            NodeKind::Dns => dns.map(Self::Dns),
            NodeKind::Meraki | NodeKind::Device => None,
        }
    }

    /// Attach resolved HTTP credentials. A no-op for a DNS monitor, which has none.
    #[must_use]
    pub fn with_http_auth(self, auth: Option<HttpAuth>) -> Self {
        match self {
            Self::Url { cfg, .. } => Self::Url { cfg, auth },
            Self::Dns(cfg) => Self::Dns(cfg),
        }
    }
}

/// The sweep's once-per-round preloaded id sets, one per single-purpose monitor kind.
///
/// Each kind lives in its own 1:1 side table, so resolving a node's kind means a query against
/// every one of them — at 50k nodes that is 50k round trips *per table* per round for the 99.9% of
/// the fleet that are ordinary devices. The sweep loads the ids once and hands them down; a node
/// absent from the set is known not to be that kind, and its lookup is skipped.
///
/// ⚠️ `None` and an **empty set** mean different things and must not be conflated.
/// `None` = "no hint was preloaded" ⇒ query directly (the on-demand single-node path).
/// `Some(∅)` = "there are no monitors of this kind at all" ⇒ skip every lookup.
/// [`Default`] is both-`None`, which is exactly the on-demand behaviour.
///
/// This is one struct rather than one `Option<&HashSet<Uuid>>` parameter per kind because the
/// second kind already made the parameter list ambiguous at the call sites, and a third would make
/// it worse — see `extensibility.md` §3.
#[derive(Debug, Clone, Copy, Default)]
pub struct MonitorHints<'a> {
    /// Nodes carrying a `url_checks` row, or `None` for "not preloaded".
    pub url: Option<&'a HashSet<Uuid>>,
    /// Nodes carrying a `dns_checks` row, or `None` for "not preloaded".
    pub dns: Option<&'a HashSet<Uuid>>,
}

/// Whether a per-node side-table lookup is worth paying for, given the sweep's preloaded id set.
///
/// One function rather than the `is_none_or` idiom written at each call site: the whole failure
/// mode is silent (get it backwards and a monitor is never polled, or every node queries every
/// table again), and inline it is untestable — `build_scheduled_jobs_hinted` needs a live database.
#[must_use]
pub(super) fn hint_admits(hint: Option<&HashSet<Uuid>>, node: Uuid) -> bool {
    hint.is_none_or(|ids| ids.contains(&node))
}

/// A check ready to become a job, carrying the job-kind label that must travel with it.
///
/// The label is operator-visible — [`publish`] writes it into the `dispatch.poll_job` tracing
/// span and into the warn line a failed publish emits — so it is returned *with* the spec rather
/// than written at the `jobs.push` call, where the two could drift apart (ADR-084 Inc.3).
type LabelledSpec = (CheckSpec, &'static str);

/// The half of [`assemble_node_jobs`] that differs between SNMP v2c and v3.
///
/// That function used to carry sixteen blocks — eight per authentication scheme, structurally
/// identical, differing only in which builder they called, which [`CheckSpec`] variant they wrapped,
/// and the job-kind label. Adding an SNMP check kind meant writing the block twice, and the two
/// copies had nothing keeping them in step (ADR-084 Inc.3).
///
/// 🚨 **Each method returns its label beside its spec, and that pairing is the point.** The sixteen
/// labels reach an operator: [`publish`] puts them in the `dispatch.poll_job` tracing span and in
/// the warn line a failed publish emits, and a test branches on `kind.contains("mau")`. They used to
/// be written at the `jobs.push` call, one line away from the builder that chose the variant, so the
/// spelling and the spec could drift apart in silence. Returned together, they cannot.
///
/// ⚠️ **Do not "simplify" the labels into a computed `"snmp_v3_" + suffix`.** They are the strings an
/// operator greps for; both spellings must survive verbatim, and `kind.contains("mau")` matches either
/// one, so a test would not notice if only one did.
trait SnmpJobSource {
    /// Scalar and table checks, which share one walk of the collection set — hence one method
    /// returning both rather than two that would resolve the set twice.
    fn scalar_and_table(
        &self,
        items: &[CollectionItem],
        timeout_ms: u32,
    ) -> (Option<LabelledSpec>, Option<LabelledSpec>);
    /// The optical-power probe, if any collection item asks for one.
    fn optical(&self, items: &[CollectionItem], timeout_ms: u32) -> Option<LabelledSpec>;
    /// The CDP/LLDP neighbour walk (ADR-038).
    fn neighbors(&self, timeout_ms: u32) -> LabelledSpec;
    /// The MAU/ENTITY media-type walk (ADR-063).
    fn media(&self, timeout_ms: u32) -> LabelledSpec;
    /// The interface-address walk (ADR-043).
    fn l3(&self, timeout_ms: u32) -> LabelledSpec;
    /// The ARP/neighbour-cache walk.
    fn arp(&self, timeout_ms: u32) -> LabelledSpec;
    /// The routing-adjacency walk, probing this node's assigned targets (ADR-043 Inc.4).
    fn routing(&self, targets: &[std::net::IpAddr], timeout_ms: u32) -> LabelledSpec;
}

/// SNMP v2c: the credential is a community string.
struct V2c<'a>(&'a str);

/// SNMP v3 (USM): the credential is a decrypted secret document.
struct V3<'a>(&'a SnmpV3Secret);

impl SnmpJobSource for V2c<'_> {
    fn scalar_and_table(
        &self,
        items: &[CollectionItem],
        timeout_ms: u32,
    ) -> (Option<LabelledSpec>, Option<LabelledSpec>) {
        let (scalar, table) = build_snmp_checks(self.0, items, timeout_ms);
        (
            scalar.map(|c| (CheckSpec::Snmp(c), "snmp")),
            table.map(|c| (CheckSpec::SnmpTable(c), "snmp_table")),
        )
    }
    fn optical(&self, items: &[CollectionItem], timeout_ms: u32) -> Option<LabelledSpec> {
        build_snmp_optical_check(self.0, items, timeout_ms)
            .map(|c| (CheckSpec::SnmpOptical(c), "snmp_optical"))
    }
    fn neighbors(&self, timeout_ms: u32) -> LabelledSpec {
        (
            CheckSpec::SnmpNeighbors(build_snmp_neighbor_check(self.0, timeout_ms)),
            "snmp_neighbors",
        )
    }
    fn media(&self, timeout_ms: u32) -> LabelledSpec {
        (
            CheckSpec::SnmpMau(build_snmp_mau_check(self.0, timeout_ms)),
            "snmp_mau",
        )
    }
    fn l3(&self, timeout_ms: u32) -> LabelledSpec {
        (
            CheckSpec::SnmpL3(build_snmp_l3_check(self.0, timeout_ms)),
            "snmp_l3",
        )
    }
    fn arp(&self, timeout_ms: u32) -> LabelledSpec {
        (
            CheckSpec::SnmpArp(build_snmp_arp_check(self.0, timeout_ms)),
            "snmp_arp",
        )
    }
    fn routing(&self, targets: &[std::net::IpAddr], timeout_ms: u32) -> LabelledSpec {
        (
            CheckSpec::SnmpRouting(build_snmp_routing_check(self.0, targets, timeout_ms)),
            "snmp_routing",
        )
    }
}

impl SnmpJobSource for V3<'_> {
    fn scalar_and_table(
        &self,
        items: &[CollectionItem],
        timeout_ms: u32,
    ) -> (Option<LabelledSpec>, Option<LabelledSpec>) {
        (
            build_snmp_v3_check(self.0, items, timeout_ms)
                .map(|c| (CheckSpec::SnmpV3(c), "snmp_v3")),
            build_snmp_v3_table_check(self.0, items, timeout_ms)
                .map(|c| (CheckSpec::SnmpV3Table(c), "snmp_v3_table")),
        )
    }
    fn optical(&self, items: &[CollectionItem], timeout_ms: u32) -> Option<LabelledSpec> {
        build_snmp_v3_optical_check(self.0, items, timeout_ms)
            .map(|c| (CheckSpec::SnmpV3Optical(c), "snmp_v3_optical"))
    }
    fn neighbors(&self, timeout_ms: u32) -> LabelledSpec {
        (
            CheckSpec::SnmpV3Neighbors(build_snmp_v3_neighbor_check(self.0, timeout_ms)),
            "snmp_v3_neighbors",
        )
    }
    fn media(&self, timeout_ms: u32) -> LabelledSpec {
        (
            CheckSpec::SnmpV3Mau(build_snmp_v3_mau_check(self.0, timeout_ms)),
            "snmp_v3_mau",
        )
    }
    fn l3(&self, timeout_ms: u32) -> LabelledSpec {
        (
            CheckSpec::SnmpV3L3(build_snmp_v3_l3_check(self.0, timeout_ms)),
            "snmp_v3_l3",
        )
    }
    fn arp(&self, timeout_ms: u32) -> LabelledSpec {
        (
            CheckSpec::SnmpV3Arp(build_snmp_v3_arp_check(self.0, timeout_ms)),
            "snmp_v3_arp",
        )
    }
    fn routing(&self, targets: &[std::net::IpAddr], timeout_ms: u32) -> LabelledSpec {
        (
            CheckSpec::SnmpV3Routing(build_snmp_v3_routing_check(self.0, targets, timeout_ms)),
            "snmp_v3_routing",
        )
    }
}

/// Append a node's SNMP jobs, whichever authentication scheme it uses.
///
/// Generic rather than `&dyn` so each arm monomorphises to what it used to be written as by
/// hand — this is a de-duplication, not a new indirection at dispatch time.
fn push_snmp_jobs<S: SnmpJobSource>(
    src: &S,
    node: &Node,
    items: &[CollectionItem],
    interval_secs: u32,
    neighbors: &AdjacencyPolicy,
    jobs: &mut Vec<(PollJob, &'static str)>,
) {
    let job = |(spec, kind): LabelledSpec, secs: u32| {
        (
            PollJob::for_spec(Uuid::new_v4(), node.id, node.address, spec, secs),
            kind,
        )
    };

    let (scalar, table) = src.scalar_and_table(items, SNMP_TIMEOUT_MS);
    if let Some(spec) = scalar {
        // Identity probing rides the scalar job only: it is the one that already does a GET, so
        // asking for sysDescr.0 alongside costs no extra round trip.
        let (mut j, kind) = job(spec, interval_secs);
        j.probe_identity = node.vendor.is_none();
        jobs.push((j, kind));
    }
    if let Some(spec) = table {
        jobs.push(job(spec, interval_secs));
    }
    // Optical rides `interval_secs`, not a slow adjacency cadence: optical power drifts
    // continuously with temperature and age, and it shares a time axis with throughput in the
    // interface dock (ADR-062).
    if let Some(spec) = src.optical(items, SNMP_TIMEOUT_MS) {
        jobs.push(job(spec, interval_secs));
    }
    if neighbors.neighbors_enabled {
        jobs.push(job(
            src.neighbors(SNMP_TIMEOUT_MS),
            neighbors.neighbors_interval_secs,
        ));
    }
    // Media rides the slow cadence, unlike optical above: a port's medium changes when someone
    // swaps a module, not continuously (ADR-063 Inc.2).
    if neighbors.media_enabled {
        jobs.push(job(
            src.media(SNMP_TIMEOUT_MS),
            neighbors.media_interval_secs,
        ));
    }
    if neighbors.l3_enabled {
        jobs.push(job(src.l3(SNMP_TIMEOUT_MS), neighbors.l3_interval_secs));
    }
    if neighbors.arp_enabled {
        jobs.push(job(src.arp(SNMP_TIMEOUT_MS), neighbors.arp_interval_secs));
    }
    if neighbors.routing_enabled {
        jobs.push(job(
            src.routing(neighbors.routing_plan.targets_for(node.id), SNMP_TIMEOUT_MS),
            neighbors.routing_interval_secs,
        ));
    }
}

/// Assemble every poll job for one node from its already-resolved SNMP auth + collection set:
/// an ICMP liveness job always, plus the SNMP scalar/table (v2c) or scalar (v3) jobs the set
/// calls for. Each job is tagged with a short kind label for logging. Pure (no I/O) — the async
/// credential/collection resolution lives in [`PollDispatcher::build_node_jobs`] — so the
/// job-shape logic is unit-testable without a database or bus.
///
/// `probe_identity` is set on the SNMP job only while the node's maker is unknown, so once a node
/// is classified we stop re-fetching sysDescr every poll (same rule as the periodic scheduler).
///
/// The neighbour job (ADR-038) is the one job here that does **not** run at `interval_secs`: it
/// carries `neighbors.interval_secs` instead, which the poller's per-spec scheduler honours
/// directly and the legacy publish path gates on separately.
#[must_use]
pub fn assemble_node_jobs(
    node: &Node,
    auth: Option<&SnmpAuth>,
    items: &[CollectionItem],
    monitor: Option<SpecialMonitor<'_>>,
    interval_secs: u32,
    neighbors: &AdjacencyPolicy,
) -> Vec<(PollJob, &'static str)> {
    // A URL or DNS monitor is its own node kind: dispatch that one job and nothing else. ICMP is
    // *not* added — a URL target may be non-pingable (e.g. behind a CDN), and a name has no
    // address of its own — and SNMP doesn't apply to either.
    match monitor {
        Some(SpecialMonitor::Url { cfg, auth }) => {
            let job = build_http_job(
                node,
                build_http_check(cfg, auth),
                interval_secs,
                Uuid::new_v4(),
            );
            return vec![(job, "http")];
        }
        Some(SpecialMonitor::Dns(cfg)) => {
            let job = build_dns_job(node, build_dns_check(cfg), interval_secs, Uuid::new_v4());
            return vec![(job, "dns")];
        }
        None => {}
    }
    let mut jobs = Vec::new();
    match auth {
        Some(SnmpAuth::V2c(community)) => {
            push_snmp_jobs(
                &V2c(community),
                node,
                items,
                interval_secs,
                neighbors,
                &mut jobs,
            );
        }
        Some(SnmpAuth::V3(secret)) => {
            push_snmp_jobs(
                &V3(secret),
                node,
                items,
                interval_secs,
                neighbors,
                &mut jobs,
            );
        }
        None => {}
    }
    // ICMP liveness is always polled, regardless of SNMP configuration.
    let icmp = build_icmp_job(node, IcmpCheck::default(), interval_secs, Uuid::new_v4());
    jobs.push((icmp, "icmp"));
    jobs
}

#[cfg(test)]
mod tests {
    use super::super::testkit::{item, node, optical_item, v3_secret};
    use super::*;
    use yagra_common::{
        builtin_arp_columns, builtin_routing_columns, CollectionKind, OpticalFlavor,
        METRIC_IF_RX_POWER_DBM,
    };

    /// The kinds present in an assembled job list (order-independent assertions).
    /// Collect the job-kind labels — and, while walking them, pin each one to the label the spec
    /// itself reports (ADR-110).
    ///
    /// 🚨 **The two vocabularies are deliberately separate and must not diverge.** These labels are
    /// written out beside the builder that chose the variant (see [`SnmpJobSource`]) so a spelling
    /// cannot drift from its spec; `CheckSpec::kind_label` is what the *poller* stamps on
    /// `yagra_poll_phase_seconds{kind=…}`, where it has a job and no dispatch label. An operator
    /// reading that histogram is reading the word they saw in the `dispatch.poll_job` span, so a
    /// disagreement would be two different names for one check with nothing to say so.
    ///
    /// Asserting it here rather than in a test of its own means **every** test that inspects job
    /// kinds enforces it, including the ones added later for reasons unrelated to labels.
    fn kinds(jobs: &[(PollJob, &'static str)]) -> Vec<&'static str> {
        for (job, label) in jobs {
            assert_eq!(
                job.check.kind_label(),
                *label,
                "job-kind label disagrees with CheckSpec::kind_label"
            );
        }
        jobs.iter().map(|(_, k)| *k).collect()
    }

    /// 🚨 The sixteen job-kind labels, pinned verbatim, for both authentication schemes.
    ///
    /// ADR-084 Inc.3 replaced sixteen hand-written blocks with one shared body and a per-scheme
    /// [`SnmpJobSource`]. The labels moved with them, and they are **operator-visible**:
    /// [`publish`] writes each one into the `dispatch.poll_job` tracing span and into the warn
    /// line a failed publish emits. Nothing else in the suite would notice a respelling —
    /// `assemble_v3_node_yields_the_v3_job_set` and its v2c twin check the `CheckSpec` variants,
    /// and the cadence test branches on `kind.contains("mau")`, which matches **both** spellings
    /// and so cannot tell them apart.
    ///
    /// Also pins the two sets to each other: every v2c label has a v3 counterpart spelled by
    /// inserting `_v3` after `snmp`. That is the relation, not a licence to compute one from the
    /// other — the strings stay written out, here and at the impls.
    #[test]
    fn every_snmp_job_kind_keeps_its_exact_label_under_both_auth_schemes() {
        let items = [
            item(
                "snmp_sys_uptime_ticks",
                "1.3.6.1.2.1.1.3.0",
                CollectionKind::Scalar,
            ),
            item(
                "if_hc_in_octets",
                "1.3.6.1.2.1.31.1.1.1.6",
                CollectionKind::Table,
            ),
            optical_item(METRIC_IF_RX_POWER_DBM, OpticalFlavor::Huawei),
        ];
        // Everything on, so every one of the eight kinds is issued.
        let policy = AdjacencyPolicy {
            neighbors_enabled: true,
            l3_enabled: true,
            arp_enabled: true,
            routing_enabled: true,
            media_enabled: true,
            ..AdjacencyPolicy::default()
        };
        let labels = |auth: &SnmpAuth| {
            let mut k = kinds(&assemble_node_jobs(
                &node("sw"),
                Some(auth),
                &items,
                None,
                30,
                &policy,
            ));
            k.sort_unstable();
            k
        };

        assert_eq!(
            labels(&SnmpAuth::V2c("public".to_owned())),
            vec![
                "icmp",
                "snmp",
                "snmp_arp",
                "snmp_l3",
                "snmp_mau",
                "snmp_neighbors",
                "snmp_optical",
                "snmp_routing",
                "snmp_table",
            ]
        );
        assert_eq!(
            labels(&SnmpAuth::V3(v3_secret())),
            vec![
                "icmp",
                "snmp_v3",
                "snmp_v3_arp",
                "snmp_v3_l3",
                "snmp_v3_mau",
                "snmp_v3_neighbors",
                "snmp_v3_optical",
                "snmp_v3_routing",
                "snmp_v3_table",
            ]
        );

        // The two sets are the same eight kinds under two spellings — a v3 label that lost its
        // marker, or a v2c label that grew one, breaks this without breaking either list above.
        let v2c: Vec<String> = labels(&SnmpAuth::V2c("public".to_owned()))
            .into_iter()
            .filter(|k| *k != "icmp")
            .map(|k| k.replacen("snmp", "snmp_v3", 1))
            .collect();
        let mut v3: Vec<String> = labels(&SnmpAuth::V3(v3_secret()))
            .into_iter()
            .filter(|k| *k != "icmp")
            .map(str::to_owned)
            .collect();
        v3.sort();
        let mut v2c = v2c;
        v2c.sort();
        assert_eq!(v2c, v3, "each v2c kind must have the `_v3` counterpart");
    }

    #[test]
    fn assemble_without_auth_yields_icmp_only() {
        // No SNMP credential resolved ⇒ liveness only, regardless of the collection set.
        let jobs = assemble_node_jobs(
            &node("ping-only"),
            None,
            &[],
            None,
            30,
            &AdjacencyPolicy::default(),
        );
        assert_eq!(kinds(&jobs), vec!["icmp"]);
        assert!(matches!(jobs[0].0.check, CheckSpec::Icmp(_)));
    }

    #[test]
    fn assemble_url_monitor_yields_http_only_no_icmp() {
        // A URL monitor is HTTP-only: no ICMP (target may be non-pingable) and no SNMP.
        let cfg = UrlCheckConfig::new("https://api.example.com/health");
        let monitor = SpecialMonitor::resolve(Some(&cfg), None);
        let jobs = assemble_node_jobs(
            &node("url-mon"),
            None,
            &[],
            monitor,
            30,
            &AdjacencyPolicy::default(),
        );
        assert_eq!(kinds(&jobs), vec!["http"]);
        assert!(matches!(jobs[0].0.check, CheckSpec::Http(_)));
    }

    #[test]
    fn assemble_dns_monitor_yields_dns_only_no_icmp() {
        // A DNS monitor is DNS-only: no ICMP (a name has no address of its own, and the resolver
        // need not be pingable) and no SNMP.
        let cfg = DnsCheckConfig::new("horryworks.net");
        let monitor = SpecialMonitor::resolve(None, Some(&cfg));
        let jobs = assemble_node_jobs(
            &node("dns-mon"),
            None,
            &[],
            monitor,
            30,
            &AdjacencyPolicy::default(),
        );
        assert_eq!(kinds(&jobs), vec!["dns"]);
        assert!(matches!(jobs[0].0.check, CheckSpec::Dns(_)));
    }

    #[test]
    fn resolve_prefers_url_when_a_node_somehow_has_both_kinds() {
        // The API edge refuses to create the second row (`api::reject_conflicting_monitor`, both
        // directions), but a row can predate that guard, so dispatch must still be deterministic
        // rather than depending on which side-table lookup ran first.
        let url = UrlCheckConfig::new("https://api.example.com/health");
        let dns = DnsCheckConfig::new("horryworks.net");
        let monitor = SpecialMonitor::resolve(Some(&url), Some(&dns));
        assert!(matches!(monitor, Some(SpecialMonitor::Url { .. })));
        let jobs = assemble_node_jobs(
            &node("both"),
            None,
            &[],
            monitor,
            30,
            &AdjacencyPolicy::default(),
        );
        assert_eq!(kinds(&jobs), vec!["http"]);
    }

    #[test]
    fn resolve_maps_each_single_row_and_no_rows() {
        // Pins the truth table for the job builder's view of it. The rule itself lives in
        // `NodeKind::resolve`; the dispatcher's load order defers to this, which defers to that.
        let url = UrlCheckConfig::new("https://api.example.com/health");
        let dns = DnsCheckConfig::new("horryworks.net");
        assert!(matches!(
            SpecialMonitor::resolve(Some(&url), None),
            Some(SpecialMonitor::Url { .. })
        ));
        assert!(matches!(
            SpecialMonitor::resolve(None, Some(&dns)),
            Some(SpecialMonitor::Dns(_))
        ));
        assert!(SpecialMonitor::resolve(None, None).is_none());
    }

    #[test]
    fn a_missing_hint_admits_every_node_but_an_empty_hint_admits_none() {
        // The only way to get `hint_admits` wrong, and both directions are silent in production:
        // read `None` as "nothing to poll" and the on-demand "poll now" path stops finding any
        // URL/DNS monitor; read an empty set as "no hint" and every node queries every side table
        // again, which is the 50k-round-trips-a-sweep debt this exists to close.
        let id = Uuid::new_v4();
        let other = Uuid::new_v4();

        assert!(hint_admits(None, id), "no hint must fall back to querying");

        let empty: HashSet<Uuid> = HashSet::new();
        assert!(
            !hint_admits(Some(&empty), id),
            "an empty preload means there are no monitors of this kind, not that we don't know"
        );

        let one: HashSet<Uuid> = [id].into_iter().collect();
        assert!(hint_admits(Some(&one), id));
        assert!(!hint_admits(Some(&one), other));
    }

    #[test]
    fn the_default_hints_query_every_side_table() {
        // `build_node_jobs` (the operator's "poll now", one node) passes the default. If a kind
        // ever defaulted to `Some(∅)` that path would silently stop polling monitors of that kind
        // — and it is the path an operator uses precisely when they suspect something is wrong.
        let hints = MonitorHints::default();
        assert!(hints.url.is_none());
        assert!(hints.dns.is_none());
        let id = Uuid::new_v4();
        assert!(hint_admits(hints.url, id));
        assert!(hint_admits(hints.dns, id));
    }

    #[test]
    fn the_job_builder_and_the_api_resolve_a_node_to_the_same_kind() {
        // `SpecialMonitor::resolve` is a view of `NodeKind::resolve`, not a second copy of the
        // precedence — the node-detail API answers the same question from the same function, and
        // the two disagreeing is exactly the bug this arrangement removes (a node polled as one
        // kind while the operator is shown another). Delegation is cheap to "optimize" back into a
        // local match, so pin it.
        let url = UrlCheckConfig::new("https://api.example.com/health");
        let dns = DnsCheckConfig::new("horryworks.net");
        for (u, d) in [
            (None, None),
            (Some(&url), None),
            (None, Some(&dns)),
            (Some(&url), Some(&dns)),
        ] {
            let rows = NodeRows {
                meraki: false,
                url: u.is_some(),
                dns: d.is_some(),
            };
            let expected = match NodeKind::resolve(rows) {
                NodeKind::Url => Some("url"),
                NodeKind::Dns => Some("dns"),
                NodeKind::Meraki | NodeKind::Device => None,
            };
            let actual = SpecialMonitor::resolve(u, d).map(|m| match m {
                SpecialMonitor::Url { .. } => "url",
                SpecialMonitor::Dns(_) => "dns",
            });
            assert_eq!(actual, expected, "rows {rows:?} resolved differently");
        }
    }

    #[test]
    fn assemble_v2c_builds_scalar_table_and_icmp_and_probes_identity() {
        let items = [
            item(
                "snmp_sys_uptime_ticks",
                "1.3.6.1.2.1.1.3.0",
                CollectionKind::Scalar,
            ),
            item(
                "if_oper_status",
                "1.3.6.1.2.1.2.2.1.8",
                CollectionKind::Table,
            ),
        ];
        let auth = SnmpAuth::V2c("public".to_owned());
        let jobs = assemble_node_jobs(
            &node("sw"),
            Some(&auth),
            &items,
            None,
            30,
            &AdjacencyPolicy::default(),
        );
        assert_eq!(
            kinds(&jobs),
            vec![
                "snmp",
                "snmp_table",
                "snmp_neighbors",
                "snmp_mau",
                "snmp_l3",
                "snmp_routing",
                "icmp"
            ]
        );
        // The maker is unknown (vendor None), so the scalar SNMP job carries the identity probe.
        let snmp = jobs.iter().find(|(_, k)| *k == "snmp").unwrap();
        assert!(snmp.0.probe_identity, "unknown-maker node probes sysDescr");
    }

    #[test]
    fn assemble_skips_identity_probe_once_maker_is_known() {
        let mut n = node("classified");
        n.vendor = Some("Cisco".to_owned());
        let items = [item(
            "snmp_sys_uptime_ticks",
            "1.3.6.1.2.1.1.3.0",
            CollectionKind::Scalar,
        )];
        let auth = SnmpAuth::V2c("public".to_owned());
        let jobs = assemble_node_jobs(
            &n,
            Some(&auth),
            &items,
            None,
            30,
            &AdjacencyPolicy::default(),
        );
        let snmp = jobs.iter().find(|(_, k)| *k == "snmp").unwrap();
        assert!(
            !snmp.0.probe_identity,
            "known-maker node stops probing sysDescr"
        );
    }

    #[test]
    fn assemble_v3_builds_scalar_table_and_icmp() {
        // v3 now walks the table set too (the GETBULK v3 walk): a v3 node with both a scalar and a
        // table item produces the scalar job, the table-walk job, and the always-on ICMP.
        let items = [
            item(
                "snmp_sys_uptime_ticks",
                "1.3.6.1.2.1.1.3.0",
                CollectionKind::Scalar,
            ),
            item(
                "if_hc_in_octets",
                "1.3.6.1.2.1.31.1.1.1.6",
                CollectionKind::Table,
            ),
        ];
        let auth = SnmpAuth::V3(v3_secret());
        let jobs = assemble_node_jobs(
            &node("fw"),
            Some(&auth),
            &items,
            None,
            30,
            &AdjacencyPolicy::default(),
        );
        assert_eq!(
            kinds(&jobs),
            vec![
                "snmp_v3",
                "snmp_v3_table",
                "snmp_v3_neighbors",
                "snmp_v3_mau",
                "snmp_v3_l3",
                "snmp_v3_routing",
                "icmp"
            ]
        );
        assert!(matches!(
            jobs.iter().find(|(_, k)| *k == "snmp_v3").unwrap().0.check,
            CheckSpec::SnmpV3(_)
        ));
        assert!(matches!(
            jobs.iter()
                .find(|(_, k)| *k == "snmp_v3_table")
                .unwrap()
                .0
                .check,
            CheckSpec::SnmpV3Table(_)
        ));
    }

    #[test]
    fn assemble_v3_scalar_only_omits_table_job() {
        // A v3 node with no table items produces just the scalar + ICMP jobs (no empty walk job).
        let items = [item(
            "snmp_sys_uptime_ticks",
            "1.3.6.1.2.1.1.3.0",
            CollectionKind::Scalar,
        )];
        let auth = SnmpAuth::V3(v3_secret());
        let jobs = assemble_node_jobs(
            &node("fw"),
            Some(&auth),
            &items,
            None,
            30,
            &AdjacencyPolicy::default(),
        );
        assert_eq!(
            kinds(&jobs),
            vec![
                "snmp_v3",
                "snmp_v3_neighbors",
                "snmp_v3_mau",
                "snmp_v3_l3",
                "snmp_v3_routing",
                "icmp"
            ]
        );
    }

    /// The neighbour job is the only one that does **not** run at the node's interval. Adjacency
    /// changes on the order of months, so riding the metric cadence would walk two extra tables on
    /// every SNMP node every minute — device load spent re-reading a constant.
    #[test]
    fn the_neighbor_job_carries_its_own_slow_cadence_not_the_nodes() {
        let items = [item(
            "if_hc_in_octets",
            "1.3.6.1.2.1.31.1.1.1.6",
            CollectionKind::Table,
        )];
        let auth = SnmpAuth::V2c("public".to_owned());
        let policy = AdjacencyPolicy::default();
        let jobs = assemble_node_jobs(&node("sw"), Some(&auth), &items, None, 30, &policy);
        for (job, kind) in &jobs {
            let expected = if kind.contains("neighbors")
                || kind.contains("l3")
                || kind.contains("routing")
                || kind.contains("mau")
            {
                3600
            } else {
                30
            };
            assert_eq!(job.interval_secs, expected, "{kind} cadence");
        }
        // The legacy publish path keys its extra due-check off exactly this inequality.
        let neighbor = jobs.iter().find(|(_, k)| *k == "snmp_neighbors").unwrap();
        assert!(neighbor.0.interval_secs > 30);
        assert!(matches!(neighbor.0.check, CheckSpec::SnmpNeighbors(_)));
        // The interface-address walk rides the same slow tier, for the same reason (ADR-043).
        let l3 = jobs.iter().find(|(_, k)| *k == "snmp_l3").unwrap();
        assert!(l3.0.interval_secs > 30);
        assert!(matches!(l3.0.check, CheckSpec::SnmpL3(_)));
    }

    /// The toggle is the whole safety valve: off must mean no job is built at all, on either
    /// protocol — not a job the poller then declines to run.
    #[test]
    fn disabling_collection_emits_no_neighbor_job_for_either_protocol() {
        let off = AdjacencyPolicy {
            neighbors_enabled: false,
            l3_enabled: false,
            media_enabled: false,
            ..AdjacencyPolicy::default()
        };
        let items = [item(
            "if_hc_in_octets",
            "1.3.6.1.2.1.31.1.1.1.6",
            CollectionKind::Table,
        )];
        let v2c = assemble_node_jobs(
            &node("sw"),
            Some(&SnmpAuth::V2c("public".to_owned())),
            &items,
            None,
            30,
            &off,
        );
        assert_eq!(kinds(&v2c), vec!["snmp_table", "snmp_routing", "icmp"]);
        let v3 = assemble_node_jobs(
            &node("fw"),
            Some(&SnmpAuth::V3(v3_secret())),
            &items,
            None,
            30,
            &off,
        );
        assert_eq!(kinds(&v3), vec!["snmp_v3_table", "snmp_v3_routing", "icmp"]);
    }

    /// The two walks share a settings struct but not a switch. A fleet may want L2 adjacency and
    /// not L3 addressing (or the reverse), and folding them onto one toggle would take that away —
    /// this pins that the sharing is about *when settings are resolved*, not about what they mean.
    #[test]
    fn the_two_discovery_walks_toggle_independently() {
        let items = [item(
            "if_hc_in_octets",
            "1.3.6.1.2.1.31.1.1.1.6",
            CollectionKind::Table,
        )];
        let auth = SnmpAuth::V2c("public".to_owned());

        let l3_only = AdjacencyPolicy {
            neighbors_enabled: false,
            media_enabled: false,
            ..AdjacencyPolicy::default()
        };
        let jobs = assemble_node_jobs(&node("sw"), Some(&auth), &items, None, 30, &l3_only);
        assert_eq!(
            kinds(&jobs),
            vec!["snmp_table", "snmp_l3", "snmp_routing", "icmp"]
        );

        let neighbors_only = AdjacencyPolicy {
            l3_enabled: false,
            media_enabled: false,
            ..AdjacencyPolicy::default()
        };
        let jobs = assemble_node_jobs(&node("sw"), Some(&auth), &items, None, 30, &neighbors_only);
        assert_eq!(
            kinds(&jobs),
            vec!["snmp_table", "snmp_neighbors", "snmp_routing", "icmp"]
        );
    }

    /// The media walk is on by default, has its own switch, and rides the slow tier (ADR-063 Inc.2).
    ///
    /// Three properties in one test because they are the three ways this increment could be wrong in
    /// production and in no local test: **shipped off** would leave the Media column empty on every
    /// fleet with nobody knowing there was a switch; **not independently switchable** would make an
    /// operator disable neighbour discovery to stop it; and **riding `interval_secs`** would walk
    /// `ifMauTable` on every 48-port switch every 30 seconds to learn a fact that changes when
    /// someone unplugs a cable.
    #[test]
    fn the_media_walk_is_on_by_default_switchable_and_slow() {
        let items = [item(
            "if_hc_in_octets",
            "1.3.6.1.2.1.31.1.1.1.6",
            CollectionKind::Table,
        )];
        assert!(
            AdjacencyPolicy::default().media_enabled,
            "shipped on by default (ADR-063 decision 6)"
        );

        for (auth, kind) in [
            (SnmpAuth::V2c("public".to_owned()), "snmp_mau"),
            (SnmpAuth::V3(v3_secret()), "snmp_v3_mau"),
        ] {
            let on = assemble_node_jobs(
                &node("sw"),
                Some(&auth),
                &items,
                None,
                30,
                &AdjacencyPolicy::default(),
            );
            let job = on
                .iter()
                .find(|(_, k)| *k == kind)
                .unwrap_or_else(|| panic!("{kind} must be built by default"));
            assert!(
                matches!(job.0.check, CheckSpec::SnmpMau(_) | CheckSpec::SnmpV3Mau(_)),
                "{kind} carries a media spec"
            );
            assert_eq!(job.0.interval_secs, 3600, "{kind} rides the slow tier");
            assert!(job.0.interval_secs > 30, "{kind} is slower than the node");

            // Off must mean no job at all, not a job the poller declines — the same safety valve
            // the neighbour toggle test pins.
            let off = AdjacencyPolicy {
                media_enabled: false,
                ..AdjacencyPolicy::default()
            };
            let jobs = assemble_node_jobs(&node("sw"), Some(&auth), &items, None, 30, &off);
            assert!(
                !kinds(&jobs).contains(&kind),
                "{kind} must not be built when the toggle is off"
            );
            // …and turning it off leaves the neighbour walk alone: separate switches, one struct.
            assert!(kinds(&jobs).iter().any(|k| k.contains("neighbors")));
        }
    }

    /// ARP discovery is off in the default policy, and that default is what every node gets until an
    /// operator says otherwise. A regression here would start walking `ipNetToPhysicalTable` on
    /// every SNMP node in a fleet on the strength of an upgrade.
    #[test]
    fn the_arp_walk_is_absent_from_the_default_policy_on_both_protocols() {
        let items = [item(
            "if_hc_in_octets",
            "1.3.6.1.2.1.31.1.1.1.6",
            CollectionKind::Table,
        )];
        assert!(!AdjacencyPolicy::default().arp_enabled);
        for (auth, kind) in [
            (SnmpAuth::V2c("public".to_owned()), "snmp_arp"),
            (SnmpAuth::V3(v3_secret()), "snmp_v3_arp"),
        ] {
            let jobs = assemble_node_jobs(
                &node("sw"),
                Some(&auth),
                &items,
                None,
                30,
                &AdjacencyPolicy::default(),
            );
            assert!(
                !kinds(&jobs).contains(&kind),
                "{kind} must not be issued unless an operator enabled it"
            );
        }
    }

    /// Turned on, it rides its own cadence and carries the columns and the row budget core decides.
    #[test]
    fn an_enabled_arp_walk_carries_its_own_cadence_columns_and_row_budget() {
        let items = [item(
            "if_hc_in_octets",
            "1.3.6.1.2.1.31.1.1.1.6",
            CollectionKind::Table,
        )];
        let policy = AdjacencyPolicy {
            arp_enabled: true,
            arp_interval_secs: 21_600,
            ..AdjacencyPolicy::default()
        };
        let jobs = assemble_node_jobs(
            &node("sw"),
            Some(&SnmpAuth::V2c("public".to_owned())),
            &items,
            None,
            30,
            &policy,
        );
        let arp = jobs.iter().find(|(_, k)| *k == "snmp_arp").expect("issued");
        assert_eq!(arp.0.interval_secs, 21_600);
        let CheckSpec::SnmpArp(check) = &arp.0.check else {
            panic!("wrong variant");
        };
        // The budget must reach the wire: a poller that received 0 would collect nothing, silently.
        assert_eq!(
            usize::try_from(check.max_rows).unwrap(),
            yagra_common::MAX_ARP_WALK_ROWS
        );
        let sent: Vec<&str> = check.columns.iter().map(|c| c.oid.as_str()).collect();
        let declared: Vec<&str> = builtin_arp_columns().iter().map(|(_, o)| *o).collect();
        assert_eq!(
            sent, declared,
            "core is the one place the OID set is decided"
        );
    }

    /// The routing walk is the third to ride the slow tier, and unlike ARP it ships on. Its cost
    /// argument is the tables it reads, so pin that it is actually issued by default.
    #[test]
    fn the_routing_walk_is_issued_by_default_on_both_protocols() {
        let items = [item(
            "if_hc_in_octets",
            "1.3.6.1.2.1.31.1.1.1.6",
            CollectionKind::Table,
        )];
        assert!(AdjacencyPolicy::default().routing_enabled);
        for (auth, kind) in [
            (SnmpAuth::V2c("public".to_owned()), "snmp_routing"),
            (SnmpAuth::V3(v3_secret()), "snmp_v3_routing"),
        ] {
            let jobs = assemble_node_jobs(
                &node("rtr"),
                Some(&auth),
                &items,
                None,
                30,
                &AdjacencyPolicy::default(),
            );
            let job = jobs.iter().find(|(_, k)| *k == kind).expect("issued");
            assert_eq!(job.0.interval_secs, 3600, "{kind} rides the slow tier");
        }
    }

    /// The toggle is the safety valve here too: off means no job is built at all, on either
    /// protocol — not a job the poller then declines to run.
    #[test]
    fn disabling_routing_collection_emits_no_job_for_either_protocol() {
        let off = AdjacencyPolicy {
            routing_enabled: false,
            ..AdjacencyPolicy::default()
        };
        for auth in [
            SnmpAuth::V2c("public".to_owned()),
            SnmpAuth::V3(v3_secret()),
        ] {
            let jobs = assemble_node_jobs(&node("rtr"), Some(&auth), &[], None, 30, &off);
            assert!(kinds(&jobs).iter().all(|k| !k.contains("routing")));
        }
    }

    /// A node with no host address of its own gets the adjacency walk and **no probes** — the rule
    /// that keeps `inetCidrRouteTable` off 50,000 nodes. Nearly every node is in this state.
    #[test]
    fn a_node_with_no_planned_targets_walks_adjacency_but_probes_nothing() {
        let jobs = assemble_node_jobs(
            &node("rtr"),
            Some(&SnmpAuth::V2c("public".to_owned())),
            &[],
            None,
            30,
            &AdjacencyPolicy::default(),
        );
        let job = jobs
            .iter()
            .find(|(_, k)| *k == "snmp_routing")
            .expect("issued");
        let CheckSpec::SnmpRouting(check) = &job.0.check else {
            panic!("wrong variant");
        };
        assert!(check.route_probes.is_empty());
        let sent: Vec<&str> = check.columns.iter().map(|c| c.oid.as_str()).collect();
        let declared: Vec<&str> = builtin_routing_columns().iter().map(|(_, o)| *o).collect();
        assert_eq!(
            sent, declared,
            "core is the one place the OID set is decided"
        );
    }

    /// A node with no SNMP credential gets no neighbour job even with collection enabled: there is
    /// nothing to authenticate the walk with, and an ICMP-only node has no LLDP/CDP tables anyway.
    #[test]
    fn a_node_without_snmp_gets_no_neighbor_job() {
        let jobs = assemble_node_jobs(
            &node("ping-only"),
            None,
            &[],
            None,
            30,
            &AdjacencyPolicy::default(),
        );
        assert_eq!(kinds(&jobs), vec!["icmp"]);
    }

    /// Single-purpose monitors return before the SNMP arm, so they must not gain a neighbour job —
    /// a URL or DNS monitor has no device to walk.
    #[test]
    fn single_purpose_monitors_get_no_neighbor_job() {
        let url = UrlCheckConfig::new("https://api.example.com/health");
        let jobs = assemble_node_jobs(
            &node("url-mon"),
            None,
            &[],
            SpecialMonitor::resolve(Some(&url), None),
            30,
            &AdjacencyPolicy::default(),
        );
        assert_eq!(kinds(&jobs), vec!["http"]);
    }
}
