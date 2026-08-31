// SPDX-License-Identifier: AGPL-3.0-only
//! What the poll dispatcher is allowed to read, expressed as four traits (ADR-111).
//!
//! Before this file [`PollDispatcher`](super::PollDispatcher) held eight concrete repositories,
//! seven of which need a live PostgreSQL. That is why **473 production lines shipped with no
//! tests**: not because nobody wrote them, but because no test could construct the value they are
//! methods on. ADR-096 決定 3 and ADR-098 決定 4 each measured that and deliberately stopped; this
//! is the rest of it, and the same shape [`crate::analysis::seams`] took.
//!
//! The traits are cut by **what the caller needs**, never per repository (ADR-092 決定 1) — nine
//! methods against seven concrete types with well over a hundred between them. The doc this
//! replaces said the seam count here was "roughly double" the analysis case; that was counting
//! *repositories* (8 against 5). Counted by what the callers need it is **smaller** — four traits,
//! nine methods, against ADR-098's four and twelve.
//!
//! 🎯 [`MonitorBindings::meraki_bound`] returning `bool` is what "not per repository" means. The
//! caller loads a whole `MerakiDeviceConfig` and reads `.is_some()` off it; what it needs is one
//! boolean, so that is what the seam offers.
//!
//! 🚨 **[`CredentialSource`] deliberately stops at the sealed bytes.** A seam returning a resolved
//! `SnmpAuth` would take the interesting half with it — the v3-document parse, the fall-through to
//! a v2c community, the static-reason warning that must never echo the secret — and leave nothing
//! to test. Same reasoning for [`CollectionSource`], which hands back the raw scoped rows so that
//! "an empty resolution falls back to the built-in catalogue" stays inside the tested code.
//!
//! ⚠️ What this does **not** buy: the production implementations below are one line each, so a
//! fake proves the dispatcher asked the right question, never that the SQL answers it correctly.
//! That needs a database, exactly as `repo/` does.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::net::IpAddr;
use std::sync::Arc;

use async_trait::async_trait;
use uuid::Uuid;
use yagra_common::{DnsCheckConfig, NodeId, ScopedCollectionItem, UrlCheckConfig};

use crate::collection::CollectionRepo;
use crate::dns_check::DnsCheckRepo;
use crate::l3::L3Repo;
use crate::meraki::MerakiDeviceRepo;
use crate::neighbors::AdjacencySettings;
use crate::repo::NodeRepo;
use crate::secrets::CredentialStore;
use crate::url_check::UrlCheckRepo;

// ── Single-purpose monitor bindings ──────────────────────────────────────────────────────────

/// Which single-purpose monitor a node is, if any — one trait rather than three, because that is
/// the single question the dispatcher asks (the inputs to `NodeKind::resolve`).
///
/// The two `*_node_ids` reads belong here for the same reason: they are the sweep's once-per-round
/// preload of exactly these bindings, and separating them from the per-node lookups they replace
/// would put the two halves of one decision behind two seams.
#[async_trait]
pub(super) trait MonitorBindings: Send + Sync {
    /// Whether the node is polled by the Meraki org collector. `bool` on purpose — see the module
    /// doc; the caller has never needed the configuration itself.
    async fn meraki_bound(&self, node: Uuid) -> anyhow::Result<bool>;
    async fn url_config(&self, node: Uuid) -> anyhow::Result<Option<UrlCheckConfig>>;
    async fn dns_config(&self, node: Uuid) -> anyhow::Result<Option<DnsCheckConfig>>;
    /// Every node carrying a `url_checks` row, for the sweep's
    /// [`MonitorHints`](super::MonitorHints).
    async fn url_node_ids(&self) -> anyhow::Result<HashSet<Uuid>>;
    /// Every node carrying a `dns_checks` row. The [`Self::url_node_ids`] twin.
    async fn dns_node_ids(&self) -> anyhow::Result<HashSet<Uuid>>;
}

/// The production implementation: the three 1:1 side-table repositories.
pub(super) struct RepoBindings {
    meraki: Arc<MerakiDeviceRepo>,
    url: Arc<UrlCheckRepo>,
    dns: Arc<DnsCheckRepo>,
}

impl RepoBindings {
    pub(super) const fn new(
        meraki: Arc<MerakiDeviceRepo>,
        url: Arc<UrlCheckRepo>,
        dns: Arc<DnsCheckRepo>,
    ) -> Self {
        Self { meraki, url, dns }
    }
}

#[async_trait]
impl MonitorBindings for RepoBindings {
    async fn meraki_bound(&self, node: Uuid) -> anyhow::Result<bool> {
        Ok(self.meraki.get(node).await?.is_some())
    }

    async fn url_config(&self, node: Uuid) -> anyhow::Result<Option<UrlCheckConfig>> {
        self.url.get(node).await
    }

    async fn dns_config(&self, node: Uuid) -> anyhow::Result<Option<DnsCheckConfig>> {
        self.dns.get(node).await
    }

    async fn url_node_ids(&self) -> anyhow::Result<HashSet<Uuid>> {
        self.url.node_ids().await
    }

    async fn dns_node_ids(&self) -> anyhow::Result<HashSet<Uuid>> {
        self.dns.node_ids().await
    }
}

// ── Credentials ──────────────────────────────────────────────────────────────────────────────

/// The one credential read the dispatcher makes: open a sealed secret in memory (ADR-018).
///
/// 🚨 Deliberately the *sealed* form. See the module doc — resolving to an `SnmpAuth` here would
/// move the protocol decision behind the seam, and that decision is what a test needs to reach.
#[async_trait]
pub(super) trait CredentialSource: Send + Sync {
    /// The credential's `kind` and its decrypted bytes, or `None` if the row is gone.
    async fn open(&self, id: Uuid) -> anyhow::Result<Option<(String, Vec<u8>)>>;
}

#[async_trait]
impl CredentialSource for CredentialStore {
    async fn open(&self, id: Uuid) -> anyhow::Result<Option<(String, Vec<u8>)>> {
        Self::open(self, id).await
    }
}

// ── Collection set ───────────────────────────────────────────────────────────────────────────

/// What a node is configured to collect, before resolution.
///
/// Raw scoped rows rather than a resolved `Vec<CollectionItem>`: "node scope overrides profile
/// scope, and an empty result means the built-in catalogue" is the behaviour under test.
#[async_trait]
pub(super) trait CollectionSource: Send + Sync {
    async fn items_for_node(
        &self,
        node: Uuid,
        profile: Option<Uuid>,
    ) -> anyhow::Result<Vec<ScopedCollectionItem>>;
}

#[async_trait]
impl CollectionSource for CollectionRepo {
    async fn items_for_node(
        &self,
        node: Uuid,
        profile: Option<Uuid>,
    ) -> anyhow::Result<Vec<ScopedCollectionItem>> {
        self.list_items_for_node(node, profile).await
    }
}

// ── Adjacency policy ─────────────────────────────────────────────────────────────────────────

/// What the neighbour/L3/ARP/routing walks need in order to be issued (ADR-043).
///
/// Two reads, one question — the deployment's toggles and cadences, plus the host addressing the
/// route-probe plan is derived from. They are one trait because the dispatcher only ever reads the
/// second in order to finish answering the first.
///
/// ⚠️ [`Self::settings`] returns a value, not a `Result`: the repository already degrades a failed
/// read to the compiled defaults, because the sweep calls it every round and a database blip must
/// not change the cadence. That choice lives there and is not re-litigated here.
#[async_trait]
pub(super) trait AdjacencySource: Send + Sync {
    async fn settings(&self) -> AdjacencySettings;
    /// Every node's `/32` and `/128` interface addresses, keyed by node.
    async fn host_addresses(&self) -> anyhow::Result<BTreeMap<NodeId, BTreeSet<IpAddr>>>;
}

/// The production implementation: deployment settings from `app_settings`, addressing from
/// `node_l3`. Two repositories behind one seam, which is the point of cutting by caller need.
pub(super) struct RepoAdjacency {
    settings: Arc<NodeRepo>,
    l3: Arc<L3Repo>,
}

impl RepoAdjacency {
    pub(super) const fn new(settings: Arc<NodeRepo>, l3: Arc<L3Repo>) -> Self {
        Self { settings, l3 }
    }
}

#[async_trait]
impl AdjacencySource for RepoAdjacency {
    async fn settings(&self) -> AdjacencySettings {
        self.settings.get_adjacency_settings().await
    }

    async fn host_addresses(&self) -> anyhow::Result<BTreeMap<NodeId, BTreeSet<IpAddr>>> {
        self.l3.host_addresses().await
    }
}
