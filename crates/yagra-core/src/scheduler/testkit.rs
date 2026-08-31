// SPDX-License-Identifier: AGPL-3.0-only
//! Fixtures more than one of the split files needs.
//!
//! `item`, `optical_item`, `v3_secret` and `node` are used by both the [`checks`](super::checks)
//! and [`assemble`](super::assemble) tests. Same shape as `events/testkit.rs` and
//! `mcp/tools/testkit.rs`.
//!
//! The rest is what ADR-111 added: one fake per seam in [`super::seams`], plus a [`Harness`] that
//! assembles a [`PollDispatcher`](super::PollDispatcher) out of them.
//!
//! 🚨 **Every fake counts its calls, and that is the point, not a convenience.** Roughly half of
//! what `dispatch.rs` decides is *not to ask* — the sweep's preloaded hints exist to remove one
//! round trip per node per side table per round, and getting the gate backwards is silent in both
//! directions (either a monitor is never polled, or 50k nodes query every table again). An
//! assertion on the returned jobs cannot see that; an assertion on the counter can.
//!
//! ⚠️ **A fake answering "nothing" looks exactly like a healthy store with no rows.** Most of
//! these default to empty, so a test asserting only that a lookup did *not* happen proves very
//! little on its own — pair it with one that asserts the lookup *did*
//! (`rejection-only-tests-pass-when-everything-rejects`).

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::net::{IpAddr, Ipv4Addr};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use uuid::Uuid;
use yagra_bus::InMemoryBus;
use yagra_common::{
    CollectionItem, CollectionKind, DnsCheckConfig, Node, NodeId, OpticalFlavor, ScopeLevel,
    ScopedCollectionItem, UrlCheckConfig,
};

use crate::neighbors::AdjacencySettings;
use crate::secrets::SnmpV3Secret;

use super::seams::{AdjacencySource, CollectionSource, CredentialSource, MonitorBindings};
use super::PollDispatcher;

pub(super) fn item(metric: &str, oid: &str, kind: CollectionKind) -> CollectionItem {
    CollectionItem {
        metric_name: metric.to_owned(),
        oid: oid.to_owned(),
        kind,
        metric_kind: yagra_common::MetricKind::Gauge,
    }
}

/// An optical item for `flavor`, publishing under `metric`.
pub(super) fn optical_item(metric: &str, flavor: OpticalFlavor) -> CollectionItem {
    item(metric, flavor.root_oid(), CollectionKind::Optical)
}

pub(super) fn v3_secret() -> SnmpV3Secret {
    SnmpV3Secret::parse(
        br#"{"user":"monitor","security_level":"authpriv","auth_protocol":"sha256",
             "auth_key":"a-pass","priv_protocol":"aes128","priv_key":"p-pass"}"#,
    )
    .expect("valid v3 secret")
}

pub(super) fn node(name: &str) -> Node {
    Node::new(NodeId::new(), name, IpAddr::V4(Ipv4Addr::new(10, 0, 0, 20)))
}

// ── Fixtures for the dispatcher's seams (ADR-111) ────────────────────────────────────────────

/// The bytes a `snmp_v3` credential holds. The same document [`v3_secret`] parses, so a test that
/// stores this and one that constructs the secret directly are talking about the same credential.
pub(super) const V3_DOC: &[u8] = br#"{"user":"monitor","security_level":"authpriv",
    "auth_protocol":"sha256","auth_key":"a-pass","priv_protocol":"aes128","priv_key":"p-pass"}"#;

/// A URL-monitor config, built through serde because [`UrlCheckConfig`] has no `Default` — every
/// field but `url` carries a serde default, so this is the shipped shape rather than an invented
/// one.
pub(super) fn url_cfg(url: &str, credential: Option<Uuid>) -> UrlCheckConfig {
    let cred = credential.map_or_else(|| "null".to_owned(), |c| format!("\"{c}\""));
    serde_json::from_str(&format!(r#"{{"url":"{url}","credential":{cred}}}"#))
        .expect("a url with defaults is a valid UrlCheckConfig")
}

/// A DNS-monitor config. Same reasoning as [`url_cfg`].
pub(super) fn dns_cfg(name: &str) -> DnsCheckConfig {
    serde_json::from_str(&format!(r#"{{"name":"{name}"}}"#))
        .expect("a name with defaults is a valid DnsCheckConfig")
}

/// How many times each [`MonitorBindings`] method was called.
///
/// Public fields rather than accessors: a test reads them by the trait method's own name, so the
/// assertion says which lookup it is talking about without a second vocabulary to learn.
#[derive(Default)]
pub(super) struct BindingCalls {
    pub(super) meraki_bound: AtomicUsize,
    pub(super) url_config: AtomicUsize,
    pub(super) dns_config: AtomicUsize,
    pub(super) url_node_ids: AtomicUsize,
    pub(super) dns_node_ids: AtomicUsize,
}

impl BindingCalls {
    fn bump(counter: &AtomicUsize) {
        counter.fetch_add(1, Ordering::Relaxed);
    }
}

/// A [`MonitorBindings`] a test seeds by hand, counting every call.
#[derive(Default)]
pub(super) struct FakeBindings {
    url: HashMap<Uuid, UrlCheckConfig>,
    dns: HashMap<Uuid, DnsCheckConfig>,
    meraki: HashSet<Uuid>,
    /// Which reads answer with `Err` instead of a value — the degradation paths this file's
    /// production code all has an opinion about.
    fail_meraki: bool,
    fail_url: bool,
    fail_dns: bool,
    fail_url_ids: bool,
    fail_dns_ids: bool,
    pub(super) calls: BindingCalls,
}

impl FakeBindings {
    pub(super) fn new() -> Self {
        Self::default()
    }

    pub(super) fn with_url(mut self, node: Uuid, cfg: UrlCheckConfig) -> Self {
        self.url.insert(node, cfg);
        self
    }

    pub(super) fn with_dns(mut self, node: Uuid, cfg: DnsCheckConfig) -> Self {
        self.dns.insert(node, cfg);
        self
    }

    pub(super) fn with_meraki(mut self, node: Uuid) -> Self {
        self.meraki.insert(node);
        self
    }

    pub(super) const fn failing_meraki(mut self) -> Self {
        self.fail_meraki = true;
        self
    }

    pub(super) const fn failing_url(mut self) -> Self {
        self.fail_url = true;
        self
    }

    pub(super) const fn failing_dns(mut self) -> Self {
        self.fail_dns = true;
        self
    }

    pub(super) const fn failing_id_preloads(mut self) -> Self {
        self.fail_url_ids = true;
        self.fail_dns_ids = true;
        self
    }
}

#[async_trait]
impl MonitorBindings for FakeBindings {
    async fn meraki_bound(&self, node: Uuid) -> anyhow::Result<bool> {
        BindingCalls::bump(&self.calls.meraki_bound);
        if self.fail_meraki {
            anyhow::bail!("meraki read failed");
        }
        Ok(self.meraki.contains(&node))
    }

    async fn url_config(&self, node: Uuid) -> anyhow::Result<Option<UrlCheckConfig>> {
        BindingCalls::bump(&self.calls.url_config);
        if self.fail_url {
            anyhow::bail!("url_checks read failed");
        }
        Ok(self.url.get(&node).cloned())
    }

    async fn dns_config(&self, node: Uuid) -> anyhow::Result<Option<DnsCheckConfig>> {
        BindingCalls::bump(&self.calls.dns_config);
        if self.fail_dns {
            anyhow::bail!("dns_checks read failed");
        }
        Ok(self.dns.get(&node).cloned())
    }

    async fn url_node_ids(&self) -> anyhow::Result<HashSet<Uuid>> {
        BindingCalls::bump(&self.calls.url_node_ids);
        if self.fail_url_ids {
            anyhow::bail!("url id preload failed");
        }
        Ok(self.url.keys().copied().collect())
    }

    async fn dns_node_ids(&self) -> anyhow::Result<HashSet<Uuid>> {
        BindingCalls::bump(&self.calls.dns_node_ids);
        if self.fail_dns_ids {
            anyhow::bail!("dns id preload failed");
        }
        Ok(self.dns.keys().copied().collect())
    }
}

/// A [`CredentialSource`] holding sealed secrets by id, counting every open.
#[derive(Default)]
pub(super) struct FakeCreds {
    secrets: HashMap<Uuid, (String, Vec<u8>)>,
    fail: bool,
    pub(super) opens: AtomicUsize,
}

impl FakeCreds {
    pub(super) fn new() -> Self {
        Self::default()
    }

    /// Store `bytes` under `id` with credential kind `kind` — the kind is what picks the protocol
    /// path, so a test names it explicitly rather than inferring it from the payload.
    pub(super) fn with(mut self, id: Uuid, kind: &str, bytes: &[u8]) -> Self {
        self.secrets.insert(id, (kind.to_owned(), bytes.to_vec()));
        self
    }

    pub(super) const fn failing(mut self) -> Self {
        self.fail = true;
        self
    }
}

#[async_trait]
impl CredentialSource for FakeCreds {
    async fn open(&self, id: Uuid) -> anyhow::Result<Option<(String, Vec<u8>)>> {
        self.opens.fetch_add(1, Ordering::Relaxed);
        if self.fail {
            anyhow::bail!("credential decrypt failed");
        }
        Ok(self.secrets.get(&id).cloned())
    }
}

/// A [`CollectionSource`] returning a fixed row set, counting every read.
#[derive(Default)]
pub(super) struct FakeCollection {
    rows: Vec<ScopedCollectionItem>,
    fail: bool,
    pub(super) reads: AtomicUsize,
}

impl FakeCollection {
    pub(super) fn new() -> Self {
        Self::default()
    }

    pub(super) fn with(mut self, level: ScopeLevel, item: CollectionItem) -> Self {
        self.rows.push(ScopedCollectionItem::new(level, item));
        self
    }

    pub(super) const fn failing(mut self) -> Self {
        self.fail = true;
        self
    }
}

#[async_trait]
impl CollectionSource for FakeCollection {
    async fn items_for_node(
        &self,
        _node: Uuid,
        _profile: Option<Uuid>,
    ) -> anyhow::Result<Vec<ScopedCollectionItem>> {
        self.reads.fetch_add(1, Ordering::Relaxed);
        if self.fail {
            anyhow::bail!("collection read failed");
        }
        Ok(self.rows.clone())
    }
}

/// An [`AdjacencySource`] whose addressing read can be made to fail on a chosen attempt.
///
/// 🚨 The failure has to be schedulable rather than permanent: the invariant under test is that a
/// *transient* failure keeps the previous plan and does not advance the cache's clock, which needs
/// one successful build followed by a failure.
#[derive(Default)]
pub(super) struct FakeAdjacency {
    settings: AdjacencySettings,
    hosts: BTreeMap<NodeId, BTreeSet<IpAddr>>,
    /// 1-based call numbers on which `host_addresses` returns `Err`.
    fail_on: Mutex<Vec<usize>>,
    pub(super) settings_reads: AtomicUsize,
    pub(super) host_reads: AtomicUsize,
}

impl FakeAdjacency {
    pub(super) fn new() -> Self {
        Self::default()
    }

    pub(super) fn with_settings(mut self, settings: AdjacencySettings) -> Self {
        self.settings = settings;
        self
    }

    /// Give one node a host route, so the built plan is non-empty and a test can tell "kept the
    /// previous plan" apart from "fell back to an empty one".
    pub(super) fn with_host(mut self, node: NodeId, ip: IpAddr) -> Self {
        self.hosts.entry(node).or_default().insert(ip);
        self
    }

    pub(super) fn failing_host_read_on(self, attempt: usize) -> Self {
        self.fail_on.lock().expect("fail_on poisoned").push(attempt);
        self
    }
}

#[async_trait]
impl AdjacencySource for FakeAdjacency {
    async fn settings(&self) -> AdjacencySettings {
        self.settings_reads.fetch_add(1, Ordering::Relaxed);
        self.settings
    }

    async fn host_addresses(&self) -> anyhow::Result<BTreeMap<NodeId, BTreeSet<IpAddr>>> {
        let attempt = self.host_reads.fetch_add(1, Ordering::Relaxed) + 1;
        if self
            .fail_on
            .lock()
            .expect("fail_on poisoned")
            .contains(&attempt)
        {
            anyhow::bail!("node_l3 read failed");
        }
        Ok(self.hosts.clone())
    }
}

/// A [`PollDispatcher`] built from fakes, with each fake kept so a test can read its counters.
///
/// ⚠️ [`Self::jobs`] must be subscribed **before** a publish: `InMemoryBus` routes pool jobs over a
/// `broadcast` channel, which drops anything sent with no receiver attached. Subscribing in
/// [`Self::build`] rather than at the assertion is what makes that impossible to get wrong.
pub(super) struct Harness {
    pub(super) dispatcher: PollDispatcher,
    pub(super) bindings: Arc<FakeBindings>,
    pub(super) creds: Arc<FakeCreds>,
    pub(super) collection: Arc<FakeCollection>,
    pub(super) adjacency: Arc<FakeAdjacency>,
    jobs: tokio::sync::broadcast::Receiver<(String, yagra_bus::PollJob)>,
}

/// Assembles a [`Harness`], defaulting every seam to an empty fake.
pub(super) struct HarnessBuilder {
    bindings: FakeBindings,
    creds: FakeCreds,
    collection: FakeCollection,
    adjacency: FakeAdjacency,
    env_community: Option<String>,
    interval_secs: u32,
}

impl HarnessBuilder {
    pub(super) fn bindings(mut self, b: FakeBindings) -> Self {
        self.bindings = b;
        self
    }

    pub(super) fn creds(mut self, c: FakeCreds) -> Self {
        self.creds = c;
        self
    }

    pub(super) fn collection(mut self, c: FakeCollection) -> Self {
        self.collection = c;
        self
    }

    pub(super) fn adjacency(mut self, a: FakeAdjacency) -> Self {
        self.adjacency = a;
        self
    }

    /// The `YAGRA_SNMP_COMMUNITY` fallback for nodes with no bound credential.
    pub(super) fn env_community(mut self, community: &str) -> Self {
        self.env_community = Some(community.to_owned());
        self
    }

    pub(super) const fn interval_secs(mut self, secs: u32) -> Self {
        self.interval_secs = secs;
        self
    }

    pub(super) fn build(self) -> Harness {
        let bus = Arc::new(InMemoryBus::default());
        // Subscribe now: the channel is a broadcast, so a publish with no receiver is discarded
        // and the assertion would read as "nothing was published".
        let jobs = bus.subscribe_pool_jobs();
        let bindings = Arc::new(self.bindings);
        let creds = Arc::new(self.creds);
        let collection = Arc::new(self.collection);
        let adjacency = Arc::new(self.adjacency);
        Harness {
            dispatcher: PollDispatcher::from_seams(
                bus,
                creds.clone(),
                collection.clone(),
                bindings.clone(),
                adjacency.clone(),
                self.env_community,
                self.interval_secs,
            ),
            bindings,
            creds,
            collection,
            adjacency,
            jobs,
        }
    }
}

impl Harness {
    /// A builder with every seam empty and no environment community — the "nothing is configured"
    /// deployment, which is the state most of the degradation paths are about.
    pub(super) fn builder() -> HarnessBuilder {
        HarnessBuilder {
            bindings: FakeBindings::new(),
            creds: FakeCreds::new(),
            collection: FakeCollection::new(),
            adjacency: FakeAdjacency::new(),
            env_community: None,
            interval_secs: 60,
        }
    }

    /// Every `(pool, job)` published so far, drained from the subscription.
    pub(super) fn published(&mut self) -> Vec<(String, yagra_bus::PollJob)> {
        let mut out = Vec::new();
        while let Ok(msg) = self.jobs.try_recv() {
            out.push(msg);
        }
        out
    }
}
