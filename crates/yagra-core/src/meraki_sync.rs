// SPDX-License-Identifier: AGPL-3.0-only
//! The Meraki inventory sync (ADR-164): ask the Dashboard API what an organization holds, and keep
//! the answer in `meraki_inventory`.
//!
//! **Why this runs in core and not on a poller.** The inventory tier of the collect job was the
//! obvious home and is deliberately not used: a poller is stateless and reports *samples*, and what
//! a sync produces is a diff against a table only core can read. Shipping the listing back over the
//! bus would mean a new message, both allow-lists, and a second place that decides what "missing"
//! means — for three GETs every five minutes.
//!
//! **What it shares with the collector is one of the organization's two lanes**
//! ([`MerakiInflight`], ADR-169): the slow one, for the periodic sync and for "Sync now" alike
//! (ADR-164 決定 32). The Dashboard API's rate limit is per organization, so a sync never runs
//! beside a collect in its own lane — whichever asks second waits a tick — and it paces at one
//! lane's share of the budget (`MerakiOrg::lane_rps`), because the other lane can be busy; the one
//! exception is an organization with no node yet, whose fast lane has nothing to do. The lane is
//! released by a drop guard, because a sync that panicked or was cancelled while holding it would
//! otherwise stop that lane's collects until the lease ran out.
//!
//! **"Sync now" is a request, not a call** (決定 32). It re-reads the whole organization, which takes
//! minutes, so the endpoint writes the request on the organization's row and [`run_sync_loop`] runs
//! it when the slow lane is free; the row carries its progress while it runs.
//!
//! **What it must never do is conclude from a short answer.** A device the listing does not contain
//! is marked missing, so the listing has to be complete: [`MerakiDirectory::inventory`] is backed by
//! `yagra_transport::fetch_inventory`, which returns an error rather than a partial result, and a
//! sync whose listing failed writes its reason and nothing else — not one row of
//! `meraki_inventory`, and not `last_sync_at`.
//!
//! **Between the listing and the writes it reads MX networks' LAN sides** (ADR-164 決定 28,
//! [`MerakiSync::lan_stage`]). An MX reports no `lanIp`, so its address is one of its own VLAN
//! addresses — the lowest-numbered inside a folder's IP range, else the lowest-numbered, after
//! the addresses other networks reuse have been set aside. Those can only be read one network at a
//! time, and `meraki_org_networks` remembers them. A sync reads **every network never read, all at
//! once** (決定 30) — an organization's first sync reads its whole LAN side before anything is
//! imported — plus a few that are a day old; "Sync now" reads them all. It is a read that can fail
//! without failing the sync: a network it could not read keeps what it said last time, an MX in a
//! network never read is not imported until it has been, and a sync whose reads were cut short
//! imports no MX at all (決定 31), since an address is only known to be reused once every network
//! holding it has been read.
//!
//! **Then it imports** (ADR-164 Inc.4), when the organization says so: a device in a watched
//! network that Meraki has reported online, and that has never been a node here, becomes one. The
//! pick is [`crate::meraki_import::pick_automatic`] (pure); everything after the pick is the path a
//! manual import takes — [`ImportResolver`], then the one writer, `MerakiOrgRepo::import_devices`.
//! ⚠️ The import runs **after** the inventory is written and can fail on its own (a database read).
//! That sync is recorded as failed (`internal`) and retried an interval later; the inventory rows
//! it wrote stay, because they came from a complete listing and are true either way.
//! An import that created nodes bumps the configuration generation, for the reason the wireless
//! importer does: the scheduler's cached round holds only the nodes it was built from, and until it
//! is rebuilt it does not know the new ones are Meraki's.

use std::collections::{HashMap, HashSet};
use std::net::IpAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use uuid::Uuid;
use yagra_common::MerakiHaRole;
use yagra_transport::{
    MerakiFetchError, MerakiInventory, MerakiNetworkLan, MerakiOrgInfo, MerakiWireOrigin,
    TransportError,
};

use crate::meraki::{resolve_meraki_key, MerakiInflight, MerakiLane, MerakiOrg, MerakiOrgRepo};
use crate::meraki_import::{pick_automatic, ImportResolver};
use crate::meraki_inventory::{
    lan_addresses, lan_order, lan_reads_due, lan_rereads_per_sync, networks_with_an_mx, plan_sync,
    seen_devices, LanAddresses, LanReads, MerakiInventoryRepo, NetworkLan,
};
use crate::repo::NodeRepo;
use crate::secrets::CredentialStore;

/// How often the loop looks for an organization that is due.
const TICK: Duration = Duration::from_secs(15);
/// One Dashboard request's timeout.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
/// How long the three inventory listings may take. At a request a second they are a few seconds for
/// any organization under a few thousand devices; this is the ceiling for one that is not.
const INVENTORY_TIMEOUT: Duration = Duration::from_secs(120);
/// How long the warm-spare role read may take (ADR-164 決定 26) — one paged listing.
const ROLES_TIMEOUT: Duration = Duration::from_secs(60);
/// The flight's lease as a sync takes it — the backstop if the drop guard never runs (the process is
/// killed). Longer than [`INVENTORY_TIMEOUT`], the one read before the LAN stage; each later stage
/// extends it to cover its own reads ([`MerakiInflight::extend`]), so a sync still reading is never
/// taken for abandoned and a collect never starts beside it.
const LEASE: Duration = Duration::from_secs(150);
/// What an extended lease keeps beyond the reads it covers: a request still in flight when the
/// reads' own deadline passed, and the writes after them.
const LEASE_MARGIN: Duration = Duration::from_secs(60);
/// The most a periodic sync spends **re-reading** networks' LAN sides it has read before (ADR-164
/// 決定 29) — a few a sync, since `lan_rereads_per_sync` caps them. Networks never read are not held
/// to it: they are all read in the sync that finds them ([`lan_budget`], 決定 30).
const LAN_READ_BUDGET: Duration = Duration::from_secs(60);
/// The longest a whole-organization read may take (決定 30・32): about 1,700 networks at a request a
/// second. One that needs longer stops there — the MX wait (決定 31) and the next sync reads on from
/// the networks it did not reach.
const FULL_READ_CEILING: Duration = Duration::from_secs(30 * 60);
/// How many networks the LAN stage reads between two progress writes and two `record_network_lans`.
const LAN_READ_CHUNK: usize = 25;
/// The shortest interval the loop honours, whatever a row says. Mirrors the column's CHECK.
const MIN_INTERVAL_SECS: u32 = crate::config::MERAKI_INVENTORY_MIN_SECS.unsigned_abs();

/// Why a sync failed. Stored on the organization's row as its token and shown to an operator, so
/// every variant is a closed fact and none carries upstream text — a Dashboard error body can quote
/// the request, and the request carries the key.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize, utoipa::ToSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum MerakiSyncFailure {
    /// The organization's stored API key could not be opened (deleted, wrong kind, unreadable).
    Credential,
    /// The stored base URL or key cannot be used to build a request.
    Config,
    /// 401/403 — the key was refused, or has no access to this organization.
    Auth,
    /// The Dashboard API kept answering 429.
    RateLimited,
    /// The Dashboard API answered an error status.
    Upstream,
    /// The Dashboard API could not be reached.
    Unreachable,
    /// The answer could not be read.
    Malformed,
    /// A listing did not run to its end.
    Truncated,
    /// The sync did not finish inside its time limit.
    Timeout,
    /// Yagra could not read or write its own database.
    Internal,
    /// A **collect** was sent and nothing came back before its lease ran out (ADR-164 決定 18): a
    /// poller from before the collect report existed failed silently, the Meraki pool has no live
    /// poller, or a poller died mid-collect. The inventory sync never produces this one — it runs
    /// in this process and always has an answer of its own.
    NoAnswer,
}

impl MerakiSyncFailure {
    /// Every reason, for the tests that pin the token, the serde tag and the locale keys together.
    pub const ALL: [Self; 11] = [
        Self::Credential,
        Self::Config,
        Self::Auth,
        Self::RateLimited,
        Self::Upstream,
        Self::Unreachable,
        Self::Malformed,
        Self::Truncated,
        Self::Timeout,
        Self::Internal,
        Self::NoAnswer,
    ];

    /// The stored token (matches the serde tag — pinned by a test).
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Credential => "credential",
            Self::Config => "config",
            Self::Auth => "auth",
            Self::RateLimited => "rate_limited",
            Self::Upstream => "upstream",
            Self::Unreachable => "unreachable",
            Self::Malformed => "malformed",
            Self::Truncated => "truncated",
            Self::Timeout => "timeout",
            Self::Internal => "internal",
            Self::NoAnswer => "no_answer",
        }
    }

    /// Read a stored token. A token this build does not know — written by a newer core — reads as
    /// [`Self::Internal`] rather than as "no failure": the row still says the sync failed.
    #[must_use]
    pub fn from_token(token: &str) -> Self {
        Self::ALL
            .into_iter()
            .find(|f| f.as_str() == token)
            .unwrap_or(Self::Internal)
    }
}

impl From<MerakiFetchError> for MerakiSyncFailure {
    fn from(e: MerakiFetchError) -> Self {
        match e {
            MerakiFetchError::Config => Self::Config,
            MerakiFetchError::Auth(_) => Self::Auth,
            MerakiFetchError::RateLimited => Self::RateLimited,
            MerakiFetchError::Status(_) => Self::Upstream,
            MerakiFetchError::Network => Self::Unreachable,
            // A next link that left the allow-list is an answer we refuse to follow.
            MerakiFetchError::Host | MerakiFetchError::Malformed => Self::Malformed,
            MerakiFetchError::Truncated => Self::Truncated,
        }
    }
}

/// Everything core asks the Dashboard API itself. The seam a test replaces; production is
/// [`DashboardApi`].
///
/// 🚨 [`inventory`](Self::inventory)'s contract is the transport's: **a complete listing or an
/// error.** A fake that returns a short `Ok` is modelling a bug the real one cannot have.
///
/// ⚠️ [`organizations`](Self::organizations) joined it in ADR-164 Inc.6. Until then the onboarding
/// endpoints called the transport directly, so a test that sent either one a well-formed body made
/// a request to `api.meraki.com` from whatever machine ran the suite — which is why neither had a
/// test in which it was accepted.
#[async_trait]
pub trait MerakiDirectory: Send + Sync {
    /// The organizations `api_key` can see at `base_url`. The caller has already run `base_url`
    /// through the host allow-list; the transport checks it again.
    async fn organizations(
        &self,
        base_url: &str,
        api_key: &str,
    ) -> Result<Vec<MerakiOrgInfo>, TransportError>;

    /// Read `org`'s networks, devices and availabilities, to the end.
    async fn inventory(
        &self,
        org: &MerakiOrg,
        api_key: &str,
    ) -> Result<MerakiInventory, MerakiFetchError>;

    /// Read every MX's warm-spare role, to the end (ADR-164 決定 26): `(serial, role)`, `None` for
    /// an MX whose warm spare is not enabled. No default body on purpose — a fake that forgot this
    /// would answer "no pairs" and hide every role the sync should write.
    async fn ha_roles(
        &self,
        org: &MerakiOrg,
        api_key: &str,
    ) -> Result<Vec<(String, Option<MerakiHaRole>)>, MerakiFetchError>;

    /// Read the LAN addresses the MX holds in each of `network_ids`, one network at a time at `rps`,
    /// until `budget` is spent (ADR-164 決定 28): the networks reached, in order, each with its
    /// addresses or its own failure. `Err` only when nothing could be sent. No default body, for the
    /// reason [`ha_roles`](Self::ha_roles) has none — a fake that answered "nothing reached" would
    /// leave every MX waiting for an address, and so never imported.
    ///
    /// The rate is the caller's because it is not always the lane's: an organization with no node
    /// yet reads at its whole `target_rps` (決定 30).
    async fn network_lans(
        &self,
        org: &MerakiOrg,
        api_key: &str,
        network_ids: &[String],
        budget: Duration,
        rps: f64,
    ) -> Result<Vec<(String, MerakiNetworkLan)>, MerakiFetchError>;
}

/// The real Dashboard API, through `yagra-transport` (GET only, host allow-listed, paced).
///
/// `wire` is where the requests physically go when that is not the host each URL names. Only a lab
/// build can set it (ADR-166); `run_live` passes `None` everywhere else. This one value is what
/// onboarding, "Sync now" and the periodic sync all read through.
pub struct DashboardApi {
    wire: Option<MerakiWireOrigin>,
}

impl DashboardApi {
    pub fn new(wire: Option<MerakiWireOrigin>) -> Self {
        Self { wire }
    }
}

#[async_trait]
impl MerakiDirectory for DashboardApi {
    async fn organizations(
        &self,
        base_url: &str,
        api_key: &str,
    ) -> Result<Vec<MerakiOrgInfo>, TransportError> {
        yagra_transport::list_organizations(base_url, api_key, REQUEST_TIMEOUT, self.wire.as_ref())
            .await
    }

    async fn inventory(
        &self,
        org: &MerakiOrg,
        api_key: &str,
    ) -> Result<MerakiInventory, MerakiFetchError> {
        yagra_transport::fetch_inventory(
            &org.base_url,
            api_key,
            &org.org_id,
            org.lane_rps(),
            REQUEST_TIMEOUT,
            self.wire.as_ref(),
        )
        .await
    }

    async fn ha_roles(
        &self,
        org: &MerakiOrg,
        api_key: &str,
    ) -> Result<Vec<(String, Option<MerakiHaRole>)>, MerakiFetchError> {
        yagra_transport::fetch_ha_roles(
            &org.base_url,
            api_key,
            &org.org_id,
            org.lane_rps(),
            REQUEST_TIMEOUT,
            self.wire.as_ref(),
        )
        .await
    }

    async fn network_lans(
        &self,
        org: &MerakiOrg,
        api_key: &str,
        network_ids: &[String],
        budget: Duration,
        rps: f64,
    ) -> Result<Vec<(String, MerakiNetworkLan)>, MerakiFetchError> {
        yagra_transport::fetch_network_lans(
            &org.base_url,
            api_key,
            network_ids,
            rps,
            REQUEST_TIMEOUT,
            budget,
            self.wire.as_ref(),
        )
        .await
    }
}

/// What one successful sync found and did.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct MerakiSyncReport {
    /// Devices the Dashboard lists.
    pub devices: u32,
    /// Networks the Dashboard lists.
    pub networks: u32,
    /// Inventory and network rows written. Zero is the ordinary answer.
    pub written: u32,
    /// Devices that were listed last time and are not now.
    pub newly_missing: u32,
    /// Devices this sync turned into nodes. Always zero for an organization whose automatic import
    /// is off.
    pub imported: u32,
    /// Devices that qualified for import and were left out by the organization's `max_devices`.
    pub over_cap: u32,
    /// Nodes that took a new address, a new name or a new network from the Dashboard in this sync
    /// (ADR-164 決定 14). A node is renamed only while it still carries the name Meraki gave it,
    /// and is never moved to another folder. Zero is the ordinary answer.
    pub followed: u32,
}

/// Why a sync did not produce a report.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncError {
    /// Every lane this sync may take is held by a collect or another sync (ADR-169). Nothing was
    /// attempted and nothing was recorded.
    Busy,
    /// The sync ran and failed; the reason is on the organization's row.
    Failed(MerakiSyncFailure),
}

/// Releases the lane the sync took when it ends — however it ends.
struct Flight<'a> {
    inflight: &'a MerakiInflight,
    job: Uuid,
}

impl Drop for Flight<'_> {
    fn drop(&mut self) {
        self.inflight.complete(self.job);
    }
}

/// The sync, with everything it reaches. One per process, shared by the periodic loop and the
/// "Sync now" endpoint, so both go through the same flight.
pub struct MerakiSync {
    orgs: Arc<MerakiOrgRepo>,
    inventory: Arc<MerakiInventoryRepo>,
    creds: Arc<CredentialStore>,
    directory: Arc<dyn MerakiDirectory>,
    inflight: Arc<MerakiInflight>,
    resolver: Arc<ImportResolver>,
}

impl MerakiSync {
    #[must_use]
    pub fn new(
        orgs: Arc<MerakiOrgRepo>,
        inventory: Arc<MerakiInventoryRepo>,
        creds: Arc<CredentialStore>,
        directory: Arc<dyn MerakiDirectory>,
        inflight: Arc<MerakiInflight>,
        resolver: Arc<ImportResolver>,
    ) -> Self {
        Self {
            orgs,
            inventory,
            creds,
            directory,
            inflight,
            resolver,
        }
    }

    /// This process's one handle to the Dashboard API.
    ///
    /// The onboarding endpoints read it from here rather than holding their own, so a fixture that
    /// replaces the directory replaces it for everything that would otherwise leave the machine.
    #[must_use]
    pub fn directory(&self) -> &dyn MerakiDirectory {
        self.directory.as_ref()
    }

    /// The periodic sync (ADR-169 決定 1): the **slow lane only** — nothing waits on it, and in the
    /// fast lane it delayed availability by one tick every time it ran; its 315 s cycle and the shift
    /// that delay gave availability stayed in step, measured on the lab deployment. It reads every
    /// MX network never read, all in this one sync (ADR-164 決定 30), and its share of the stale ones.
    pub async fn sync_org_scheduled(&self, org: &MerakiOrg) -> Result<MerakiSyncReport, SyncError> {
        self.sync_in(org, SyncKind::Scheduled).await
    }

    /// The whole-organization read "Sync now" asked for (ADR-164 決定 32): every MX network's LAN
    /// side, read or not, then the import — in the slow lane only, like the periodic sync. It takes
    /// minutes, so nobody waits on it: the endpoint only records the request
    /// ([`MerakiOrgRepo::request_full_sync`]) and [`run_sync_loop`] calls this. The request is
    /// cleared when the read ends, however it ends.
    pub async fn sync_org_requested(&self, org: &MerakiOrg) -> Result<MerakiSyncReport, SyncError> {
        self.sync_in(org, SyncKind::Requested).await
    }

    async fn sync_in(
        &self,
        org: &MerakiOrg,
        kind: SyncKind,
    ) -> Result<MerakiSyncReport, SyncError> {
        let job = Uuid::new_v4();
        if !self
            .inflight
            .acquire_sync(org.id, MerakiLane::Slow, job, LEASE, Instant::now())
        {
            metrics::counter!("yagra_meraki_syncs_total", "outcome" => "busy").increment(1);
            return Err(SyncError::Busy);
        }
        let _flight = Flight {
            inflight: &self.inflight,
            job,
        };

        let started = Instant::now();
        let mut reading = LanReading::default();
        let result = self.attempt(org, kind, job, &mut reading).await;
        if reading.whole || kind == SyncKind::Requested {
            self.end_full_read(org, kind, &reading, result.is_ok(), started.elapsed())
                .await;
        }

        match result {
            Ok(report) => {
                if let Err(e) = self.orgs.record_sync_success(org.id).await {
                    // The rows are written and correct; only the stamp is missing, so the loop
                    // will simply sync again next tick and find nothing to write.
                    tracing::warn!(org = %org.org_id, error = %e, "meraki sync: recording success failed");
                }
                metrics::counter!("yagra_meraki_syncs_total", "outcome" => "ok").increment(1);
                tracing::debug!(
                    org = %org.org_id,
                    devices = report.devices,
                    written = report.written,
                    newly_missing = report.newly_missing,
                    "meraki sync completed"
                );
                if report.imported > 0 {
                    // No audit row (ADR-164 決定 11): nobody did this. Who switched automatic import
                    // on is what the audit log holds, from `PUT …/import-settings`.
                    tracing::info!(
                        org = %org.org_id,
                        imported = report.imported,
                        over_cap = report.over_cap,
                        "imported meraki devices as nodes"
                    );
                    metrics::counter!("yagra_meraki_devices_imported_total")
                        .increment(u64::from(report.imported));
                }
                if report.followed > 0 {
                    // No audit row either, for the same reason: nobody did this.
                    tracing::info!(
                        org = %org.org_id,
                        followed = report.followed,
                        "imported meraki nodes followed the dashboard"
                    );
                    metrics::counter!("yagra_meraki_nodes_followed_total")
                        .increment(u64::from(report.followed));
                }
                // One bump for the sync, and only when a node row really changed. It is the address
                // that needs it: the connectivity graph is re-derived when the generation or an
                // observation watermark moves (`run_topology_derivation`), and a re-addressed node
                // moves neither watermark. A rename alone would not need one — a notification reads
                // names on a 60-second TTL — but it is rare enough not to be worth telling apart.
                if report.imported > 0 || report.followed > 0 {
                    crate::config_gen::bump();
                }
                Ok(report)
            }
            Err(failure) => {
                if let Err(e) = self
                    .orgs
                    .record_sync_failure(org.id, failure.as_str())
                    .await
                {
                    tracing::warn!(org = %org.org_id, error = %e, "meraki sync: recording failure failed");
                }
                metrics::counter!("yagra_meraki_syncs_total", "outcome" => "failed").increment(1);
                tracing::warn!(org = %org.org_id, reason = failure.as_str(), "meraki sync failed");
                Err(SyncError::Failed(failure))
            }
        }
    }

    /// A whole-organization read ended (決定 30・32): clear its progress — and the request, when it
    /// was the read "Sync now" asked for — count it, and say in one line how it went.
    async fn end_full_read(
        &self,
        org: &MerakiOrg,
        kind: SyncKind,
        reading: &LanReading,
        succeeded: bool,
        took: Duration,
    ) {
        if let Err(e) = self
            .orgs
            .finish_full_sync(org.id, kind == SyncKind::Requested)
            .await
        {
            // The page goes on saying "reading" until the next sync of this organization, or the
            // next leader, clears it. Nothing else reads these columns.
            tracing::warn!(org = %org.org_id, error = %e, "meraki sync: clearing the full read failed");
        }
        let trigger = match kind {
            SyncKind::Requested => "requested",
            SyncKind::Scheduled => "unread",
        };
        let outcome = match (succeeded, reading.complete) {
            (false, _) => "failed",
            (true, true) => "complete",
            (true, false) => "cut_short",
        };
        metrics::counter!("yagra_meraki_full_syncs_total", "trigger" => trigger, "outcome" => outcome)
            .increment(1);
        tracing::info!(
            org = %org.org_id,
            trigger,
            outcome,
            networks = reading.due,
            tried = reading.tried,
            seconds = took.as_secs(),
            "meraki sync: whole-organization read ended"
        );
    }

    /// The sync itself. What it learns from a complete answer it writes as it goes — the network
    /// list, then each network's LAN side as it is read — because each is true on its own. What
    /// needs the whole picture waits for it: the inventory rows (a device the listing lacks is marked
    /// missing), what imported nodes follow, and the import, which reads the rows written before it.
    async fn attempt(
        &self,
        org: &MerakiOrg,
        kind: SyncKind,
        job: Uuid,
        reading: &mut LanReading,
    ) -> Result<MerakiSyncReport, MerakiSyncFailure> {
        let api_key = resolve_meraki_key(&self.creds, org.credential_id)
            .await
            .ok_or(MerakiSyncFailure::Credential)?;
        let listing =
            tokio::time::timeout(INVENTORY_TIMEOUT, self.directory.inventory(org, &api_key))
                .await
                .map_err(|_| MerakiSyncFailure::Timeout)??;

        let internal = |what: &'static str| {
            move |e: anyhow::Error| {
                tracing::warn!(error = %e, "meraki sync: {what}");
                MerakiSyncFailure::Internal
            }
        };
        let networks: Vec<(String, String)> = listing
            .networks
            .iter()
            .map(|n| (n.id.clone(), n.name.clone()))
            .collect();
        let network_rows = self
            .orgs
            // With automatic import on, a network seen for the first time is watched from this
            // sync on. One already stored keeps its flag either way (migration 0125). Before the
            // LAN reads: they record what they learn onto these rows.
            .record_networks(org.id, &networks, org.import_devices)
            .await
            .map_err(internal("recording the networks failed"))?;
        let bound = self
            .inventory
            .bound(org.id)
            .await
            .map_err(internal("reading the device bindings failed"))?;
        let (lans, read) = self
            .lan_stage(org, &api_key, &listing, kind, bound.is_empty(), job)
            .await;
        *reading = read;
        let lans = lans?;
        let seen = seen_devices(&listing, &lans.chosen);
        let stored = self
            .inventory
            .stored(org.id)
            .await
            .map_err(internal("reading the inventory failed"))?;
        let plan = plan_sync(&stored, &seen, &bound);
        // The inventory rows and what imported nodes follow, in one transaction (決定 14).
        let applied = self
            .inventory
            .apply(org.id, &plan)
            .await
            .map_err(internal("writing the inventory failed"))?;
        // After `apply`, which created the rows of devices seen for the first time.
        self.inflight
            .extend(job, ROLES_TIMEOUT + LEASE_MARGIN, Instant::now());
        let roles = self.ha_roles(org, &api_key, ROLES_TIMEOUT).await;
        let (imported, over_cap) = self.import(org, lans.complete).await?;

        Ok(MerakiSyncReport {
            devices: count(seen.len()),
            networks: count(listing.networks.len()),
            // Not the LAN reads: those are the same network rows, updated again.
            written: u32::try_from(network_rows + applied.rows + roles).unwrap_or(u32::MAX),
            newly_missing: count(plan.newly_missing.len()),
            imported,
            over_cap,
            followed: applied.followed,
        })
    }

    /// The LAN side of every network holding an MX (ADR-164 決定 28): read what is due, then choose
    /// each read network's address from everything now known.
    ///
    /// **Networks never read are all read here, in this one sync** (決定 30), and "Sync now" reads
    /// every network (決定 32). A read of either kind is a *whole-organization read*: it is held to
    /// what its networks need ([`lan_budget`]) rather than to a minute, its progress goes on the
    /// organization's row for the page, and it reads at the organization's whole `target_rps` while
    /// the organization has no node — nothing is collected for it then, so the fast lane is idle.
    /// Reading the round a minute a sync is what let an MX be filed by an address it shared with a
    /// network not yet read (2026-09-24, a lab deployment): "shared" can only be decided over what
    /// has been read.
    ///
    /// **A failed read is never a reason to fail the sync, and never forgets anything.** A network
    /// whose read failed keeps what it said last time, and one never read stays unread (its MX waits
    /// to be imported); the next sync asks again. What *does* fail the sync is this core's own
    /// database — the stored addresses, the folders' ranges, the write of what was read — because
    /// reading either as empty would move hundreds of node addresses and move them back a sync later.
    ///
    /// Whether the reads ran to the end is [`LanStage::complete`], and it decides whether any MX may
    /// be imported by this sync (決定 31). What was read comes back beside the result rather than in
    /// it, so a sync that fails after reading still says what it read.
    async fn lan_stage(
        &self,
        org: &MerakiOrg,
        api_key: &str,
        listing: &MerakiInventory,
        kind: SyncKind,
        no_nodes: bool,
        job: Uuid,
    ) -> (Result<LanStage, MerakiSyncFailure>, LanReading) {
        let mut reading = LanReading::default();
        let stage = self
            .read_lans(org, api_key, listing, kind, no_nodes, job, &mut reading)
            .await;
        (stage, reading)
    }

    #[allow(clippy::too_many_arguments)] // `lan_stage`'s body; `reading` is filled as it goes
    async fn read_lans(
        &self,
        org: &MerakiOrg,
        api_key: &str,
        listing: &MerakiInventory,
        kind: SyncKind,
        no_nodes: bool,
        job: Uuid,
        reading: &mut LanReading,
    ) -> Result<LanStage, MerakiSyncFailure> {
        let internal = |what: &'static str| {
            move |e: anyhow::Error| {
                tracing::warn!(error = %e, "meraki sync: {what}");
                MerakiSyncFailure::Internal
            }
        };
        let mx = networks_with_an_mx(listing);
        if mx.is_empty() {
            reading.complete = true;
            return Ok(LanStage {
                complete: true,
                ..LanStage::default()
            });
        }
        let mut known: HashMap<String, NetworkLan> = self
            .orgs
            .network_lans(org.id)
            .await
            .map_err(internal("reading the networks' LAN addresses failed"))?;
        known.retain(|network, _| mx.contains(network));

        let whole = kind == SyncKind::Requested || mx.iter().any(|n| !known.contains_key(n));
        let reads = match kind {
            SyncKind::Requested => LanReads::Every,
            SyncKind::Scheduled => LanReads::Due {
                rereads: lan_rereads_per_sync(mx.len(), org.inventory_secs),
            },
        };
        let due = lan_reads_due(&mx, &known, Utc::now(), reads);
        let rps = if whole && no_nodes {
            org.target_rps.max(yagra_transport::MERAKI_MIN_RPS)
        } else {
            org.lane_rps()
        };
        let budget = lan_budget(due.len(), rps, whole);
        *reading = LanReading {
            whole,
            due: due.len(),
            tried: 0,
            complete: true,
        };

        if !due.is_empty() {
            // The lease covers these reads, the role read after them and the writes.
            self.inflight
                .extend(job, budget + ROLES_TIMEOUT + LEASE_MARGIN, Instant::now());
            if whole {
                if let Err(e) = self.orgs.start_full_sync(org.id, count(due.len())).await {
                    tracing::warn!(org = %org.org_id, error = %e, "meraki sync: recording the full read failed");
                }
            }
            let deadline = Instant::now() + budget;
            let pace = Duration::from_secs_f64(1.0 / rps);
            let mut fresh = 0usize;
            for (i, chunk) in due.chunks(LAN_READ_CHUNK).enumerate() {
                let left = deadline.saturating_duration_since(Instant::now());
                if left.is_zero() {
                    reading.complete = false;
                    break;
                }
                if i > 0 {
                    // Each chunk is a session of its own, whose first request is not paced against
                    // the last one of the chunk before.
                    tokio::time::sleep(pace).await;
                }
                // The transport stops sending at `left`; this bounds the one request still in flight.
                let reached = match tokio::time::timeout(
                    left + REQUEST_TIMEOUT,
                    self.directory.network_lans(org, api_key, chunk, left, rps),
                )
                .await
                {
                    Ok(Ok(reached)) => reached,
                    Ok(Err(why)) => {
                        tracing::warn!(org = %org.org_id, reason = why.token(), "meraki sync: the LAN reads could not start");
                        reading.complete = false;
                        break;
                    }
                    Err(_) => {
                        tracing::warn!(org = %org.org_id, "meraki sync: the LAN reads ran out of time");
                        reading.complete = false;
                        break;
                    }
                };
                // Short of the chunk means the transport stopped: the deadline, or an answer that
                // would be the same for every network. The last case can also fill the chunk exactly
                // — then the next chunk would only be refused the same way.
                let stopped = reached.len() < chunk.len()
                    || reached.iter().any(|(_, answer)| {
                        matches!(
                            answer,
                            Err(MerakiFetchError::Auth(_)
                                | MerakiFetchError::Host
                                | MerakiFetchError::RateLimited)
                        )
                    });
                let now = Utc::now();
                let mut read: Vec<(String, Vec<IpAddr>)> = Vec::new();
                for (network, answer) in reached {
                    reading.tried += 1;
                    let outcome = match answer {
                        Ok(addresses) => {
                            let ips = lan_order(&addresses);
                            let outcome = if ips.is_empty() { "no_lan" } else { "ok" };
                            read.push((network.clone(), ips.clone()));
                            known.insert(network, NetworkLan { ips, read_at: now });
                            outcome
                        }
                        Err(why) => {
                            tracing::debug!(org = %org.org_id, reason = why.token(), "meraki sync: a network's LAN read failed");
                            "failed"
                        }
                    };
                    metrics::counter!("yagra_meraki_lan_reads_total", "outcome" => outcome)
                        .increment(1);
                }
                // Written as they arrive: each is a complete answer about one network and true on its
                // own, so a read that stops half-way keeps what it learned and the next sync starts
                // after it rather than from the beginning.
                fresh += read.len();
                self.orgs
                    .record_network_lans(org.id, &read)
                    .await
                    .map_err(internal("recording the networks' LAN addresses failed"))?;
                if whole {
                    if let Err(e) = self
                        .orgs
                        .record_full_sync_read(org.id, count(reading.tried))
                        .await
                    {
                        tracing::debug!(org = %org.org_id, error = %e, "meraki sync: recording the read's progress failed");
                    }
                }
                if stopped {
                    reading.complete = false;
                    break;
                }
            }
            let unread = mx.iter().filter(|n| !known.contains_key(*n)).count();
            tracing::info!(
                org = %org.org_id,
                read = fresh,
                tried = reading.tried,
                due = due.len(),
                unread,
                complete = reading.complete,
                "meraki sync: read networks' LAN addresses"
            );
        }

        let candidates: Vec<IpAddr> = known
            .values()
            .flat_map(|lan| lan.ips.iter().copied())
            .collect::<HashSet<_>>()
            .into_iter()
            .collect();
        let in_a_range = self
            .resolver
            .in_a_range(&candidates)
            .await
            .map_err(internal(
                "matching LAN addresses against the folders' ranges failed",
            ))?;
        Ok(LanStage {
            chosen: lan_addresses(&known, &in_a_range),
            complete: reading.complete,
        })
    }

    /// Read and record every MX's warm-spare role (ADR-164 決定 26): the rows written.
    ///
    /// **Best effort, and never a reason to fail the sync.** The roles only label the pair on a
    /// node's card; the sync's job is the inventory, and a licence or a read that fails here must
    /// not stop it or mark a single device missing. A failure keeps the roles already stored.
    /// Outside `apply`'s one transaction on purpose: that exists because a rename is planned from a
    /// difference the same write erases, and a role has no such follow-up.
    async fn ha_roles(&self, org: &MerakiOrg, api_key: &str, left: Duration) -> u64 {
        let outcome = match tokio::time::timeout(left, self.directory.ha_roles(org, api_key)).await
        {
            Ok(Ok(roles)) => match self.inventory.record_ha_roles(org.id, &roles).await {
                Ok(written) => {
                    metrics::counter!("yagra_meraki_ha_role_reads_total", "outcome" => "ok")
                        .increment(1);
                    return written;
                }
                Err(e) => {
                    tracing::warn!(org = %org.org_id, error = %e, "meraki sync: recording ha roles failed");
                    "write_failed"
                }
            },
            Ok(Err(why)) => {
                tracing::warn!(org = %org.org_id, reason = why.token(), "meraki sync: reading ha roles failed");
                "read_failed"
            }
            Err(_) => {
                tracing::warn!(org = %org.org_id, "meraki sync: reading ha roles ran out of time");
                "timeout"
            }
        };
        metrics::counter!("yagra_meraki_ha_role_reads_total", "outcome" => outcome).increment(1);
        0
    }

    /// The import stage: `(imported, over_cap)`. Runs on every successful listing, and for an
    /// organization whose automatic import is off it does one thing — make sure the row does not
    /// go on claiming a cap is leaving devices out.
    ///
    /// Read from the table rather than from the listing in hand: the three conditions are facts the
    /// inventory holds (`first_online_at`, `imported_at`, the network's watch flag), and reading
    /// them where [`crate::meraki_inventory::classify`] reads them is what keeps "New" on the page
    /// and "imported by the sync" the same set.
    ///
    /// `mx_ready` is whether this sync's LAN reads ran to the end (決定 31); without it no MX is
    /// picked, and each keeps its place under the cap.
    async fn import(
        &self,
        org: &MerakiOrg,
        mx_ready: bool,
    ) -> Result<(u32, u32), MerakiSyncFailure> {
        let internal = |what: &'static str| {
            move |e: anyhow::Error| {
                tracing::warn!(error = %e, "meraki sync: {what}");
                MerakiSyncFailure::Internal
            }
        };
        if !org.import_devices {
            self.orgs
                .record_over_cap(org.id, 0)
                .await
                .map_err(internal("clearing the import cap count failed"))?;
            return Ok((0, 0));
        }
        let devices = self
            .inventory
            .devices(org.id)
            .await
            .map_err(internal("reading the devices to import failed"))?;
        let pick = pick_automatic(&devices, org.max_devices, mx_ready);
        let mut imported = 0;
        if !pick.chosen.is_empty() {
            let resolved = self
                .resolver
                .resolve(pick.chosen, org.file_by_prefix)
                .await
                .map_err(internal("resolving where imported devices go failed"))?;
            imported = self
                .orgs
                .import_devices(org, &resolved.devices)
                .await
                .map_err(internal("importing devices failed"))?
                .imported;
        }
        self.orgs
            .record_over_cap(org.id, pick.over_cap)
            .await
            .map_err(internal("recording the import cap count failed"))?;
        Ok((imported, pick.over_cap))
    }
}

/// Which sync this is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SyncKind {
    /// The periodic one: every network never read, and its share of the stale ones.
    Scheduled,
    /// The one "Sync now" asked for: every network (ADR-164 決定 32).
    Requested,
}

/// How long a sync may spend reading `due` networks' LAN sides at `rps` (ADR-164 決定 29・30).
///
/// A whole-organization read gets what its networks need: two requests each at most (a network with
/// VLANs off answers `vlans` with 400 and is read again at `singleLan`), each at the pace `rps` sets
/// but never under half a second — the answer itself takes a median 220 ms and a p90 of 271 ms on a
/// real organization — plus a minute, up to [`FULL_READ_CEILING`]. Re-reads alone get
/// [`LAN_READ_BUDGET`]: how many there are is capped already.
fn lan_budget(due: usize, rps: f64, whole: bool) -> Duration {
    if !whole {
        return LAN_READ_BUDGET;
    }
    let per_request = Duration::from_secs_f64(1.0 / rps.max(yagra_transport::MERAKI_MIN_RPS))
        .max(Duration::from_millis(500));
    let requests = u32::try_from(due.saturating_mul(2)).unwrap_or(u32::MAX);
    per_request
        .saturating_mul(requests)
        .saturating_add(Duration::from_secs(60))
        .min(FULL_READ_CEILING)
}

fn count(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

/// What a sync's LAN reads were, for the line and the counter a whole-organization read ends with.
/// Filled in by [`MerakiSync::lan_stage`] as it goes, so a sync that fails after it still reports
/// what it read.
#[derive(Debug, Default)]
struct LanReading {
    /// A whole-organization read: "Sync now", or a network never read (決定 30・32).
    whole: bool,
    /// The networks it set out to read.
    due: usize,
    /// How many of them it asked — a network whose own read failed counts.
    tried: usize,
    /// Whether it asked every one of them (決定 31).
    complete: bool,
}

/// What [`MerakiSync::lan_stage`] hands the rest of the sync.
#[derive(Debug, Default)]
struct LanStage {
    /// The address each read network's MX takes — what [`seen_devices`] reads.
    chosen: LanAddresses,
    /// Whether every network this sync meant to read was asked (ADR-164 決定 31). A network whose
    /// own read failed was asked; one the time limit, a refused key or 429s kept it from was not.
    /// Without it no MX is imported by this sync.
    complete: bool,
}

/// When each organization is next due. Success is read from the row (`last_sync_at`), so a manual
/// sync and a failover are both honoured; a **failure** is remembered here, because a failed sync
/// deliberately leaves `last_sync_at` alone — and without this the loop would retry a refused key
/// or a rate-limited organization every tick, which is the opposite of backing off.
#[derive(Default)]
pub struct SyncSchedule {
    failed_at: HashMap<Uuid, Instant>,
}

impl SyncSchedule {
    /// Whether `org` should be synced now.
    #[must_use]
    pub fn is_due(&self, org: &MerakiOrg, now_utc: DateTime<Utc>, now: Instant) -> bool {
        let every = Duration::from_secs(u64::from(org.inventory_secs.max(MIN_INTERVAL_SECS)));
        if self
            .failed_at
            .get(&org.id)
            .is_some_and(|&failed| now.saturating_duration_since(failed) < every)
        {
            return false;
        }
        org.last_sync_at.is_none_or(|last| {
            // A `last_sync_at` ahead of this clock reads as "just now", not as overdue.
            now_utc
                .signed_duration_since(last)
                .to_std()
                .unwrap_or_default()
                >= every
        })
    }

    /// A sync of `org` failed at `now`: wait one full interval before trying again.
    pub fn failed(&mut self, org: Uuid, now: Instant) {
        self.failed_at.insert(org, now);
    }

    /// A sync of `org` succeeded: its row carries the time from here on.
    pub fn succeeded(&mut self, org: Uuid) {
        self.failed_at.remove(&org);
    }
}

/// The leader-only periodic sync, and the runner of what "Sync now" asked for.
///
/// ⚠️ **Spawned from `LeaderTasks`, never from `run_live`** (ADR-090). Leader-gated for a stronger
/// reason than NetBox's loop: the lanes live in this process, so two cores syncing would each
/// believe they held an organization's lane alone.
///
/// Honours the Meraki kill switch — that switch exists to give the Dashboard API budget back at
/// once, and a sync spends it like a collect does. An organization with a request standing is
/// synced at once, whatever its interval and however recently it failed (ADR-164 決定 32); the
/// others when they are due.
///
/// **Organizations sync side by side, one sync each** ([`RunningSyncs`]). They used to go one after
/// another, which was free while a sync took seconds; a whole-organization read takes minutes, and
/// it would have held every other organization's sync — and every "Sync now" — behind it. The
/// Dashboard's rate limit is per organization, so running two organizations at once costs neither
/// anything.
pub async fn run_sync_loop(sync: Arc<MerakiSync>, settings: Arc<NodeRepo>) {
    // Nothing is reading as this starts: a read runs inside the process that holds its lane, so a
    // row still saying "reading" was left by a process that stopped mid-way (決定 32).
    match sync.orgs.clear_full_sync_progress().await {
        Ok(0) => {}
        Ok(n) => {
            tracing::info!(
                organizations = n,
                "meraki sync: cleared reads a previous process left"
            );
        }
        Err(e) => tracing::warn!(error = %e, "meraki sync: clearing reads left behind failed"),
    }
    let mut tick = tokio::time::interval(TICK);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut schedule = SyncSchedule::default();
    let mut running = RunningSyncs::default();
    loop {
        tick.tick().await;
        running.reap(&mut schedule, Instant::now());
        if !settings.get_meraki_polling_enabled().await {
            continue;
        }
        let orgs = match sync.orgs.list_enabled().await {
            Ok(o) => o,
            Err(e) => {
                tracing::warn!(error = %e, "meraki sync: listing orgs failed");
                continue;
            }
        };
        for org in orgs {
            let requested = org.full_sync_requested_at.is_some();
            if running.contains(org.id)
                || !(requested || schedule.is_due(&org, Utc::now(), Instant::now()))
            {
                continue;
            }
            let sync = sync.clone();
            running.spawn(org.id, async move {
                if requested {
                    sync.sync_org_requested(&org).await
                } else {
                    sync.sync_org_scheduled(&org).await
                }
            });
        }
    }
}

/// The syncs [`run_sync_loop`] has in flight: at most one per organization. Dropping it aborts them
/// all, so the loop being cancelled leaves no sync running behind it — and the drop guard each one
/// holds gives its lane back.
#[derive(Default)]
pub struct RunningSyncs {
    set: tokio::task::JoinSet<Result<MerakiSyncReport, SyncError>>,
    orgs: HashMap<tokio::task::Id, Uuid>,
}

impl RunningSyncs {
    /// Start `sync` for `org`. The caller has checked [`Self::contains`].
    pub fn spawn<F>(&mut self, org: Uuid, sync: F)
    where
        F: std::future::Future<Output = Result<MerakiSyncReport, SyncError>> + Send + 'static,
    {
        let handle = self.set.spawn(sync);
        self.orgs.insert(handle.id(), org);
    }

    /// Whether `org` has a sync in flight.
    #[must_use]
    pub fn contains(&self, org: Uuid) -> bool {
        self.orgs.values().any(|o| *o == org)
    }

    /// Hand every sync that has ended to `schedule`: a success hands the next one back to the row,
    /// a failure waits an interval, a busy lane asks again next tick. A sync that panicked counts as
    /// failed, so the loop backs off rather than asking again every fifteen seconds.
    pub fn reap(&mut self, schedule: &mut SyncSchedule, now: Instant) {
        while let Some(ended) = self.set.try_join_next_with_id() {
            let (id, outcome) = match ended {
                Ok((id, outcome)) => (id, Some(outcome)),
                Err(e) => {
                    tracing::warn!(error = %e, "meraki sync: a sync ended without an answer");
                    (e.id(), None)
                }
            };
            let Some(org) = self.orgs.remove(&id) else {
                continue;
            };
            match outcome {
                Some(Ok(_)) => schedule.succeeded(org),
                // A slow collect holds the organization. Not a failure: ask again next tick.
                Some(Err(SyncError::Busy)) => {}
                Some(Err(SyncError::Failed(_))) | None => schedule.failed(org, now),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::meraki::MerakiLane;
    use crate::pgtest;
    use sqlx::Row;
    use std::sync::Mutex;
    use yagra_common::MerakiTier;
    use yagra_transport::{
        MerakiAvailability, MerakiDeviceInfo, MerakiInventoryDevice, MerakiNetworkInfo,
    };

    fn org_with(last_sync_at: Option<DateTime<Utc>>, inventory_secs: u32) -> MerakiOrg {
        MerakiOrg {
            id: Uuid::from_u128(7),
            org_id: "123456".into(),
            name: "Acme".into(),
            base_url: "https://api.meraki.com".into(),
            credential_id: Uuid::nil(),
            availability_secs: 300,
            uplink_secs: 300,
            traffic_secs: 1800,
            inventory_secs,
            switch_ports_secs: 300,
            wireless_secs: 300,
            enabled_tiers: Vec::new(),
            target_rps: 2.0,
            group_id: None,
            enabled: true,
            last_sync_at,
            last_sync_ok: None,
            last_sync_error: None,
            import_devices: true,
            file_by_prefix: true,
            max_devices: 1000,
            devices_over_cap: 0,
            collect_failures: Vec::new(),
            full_sync_requested_at: None,
            full_sync: None,
        }
    }

    #[test]
    fn every_failure_round_trips_through_its_token_and_through_serde() {
        for f in MerakiSyncFailure::ALL {
            let json = serde_json::to_string(&f).expect("serialize");
            assert_eq!(json, format!("\"{}\"", f.as_str()), "{f:?}");
            assert_eq!(MerakiSyncFailure::from_token(f.as_str()), f);
        }
        // A token from a newer core still reads as a failure, never as none.
        assert_eq!(
            MerakiSyncFailure::from_token("quota_exhausted"),
            MerakiSyncFailure::Internal
        );
    }

    /// A poller reports a failed collect as `MerakiFetchError::token()` and core reads it back with
    /// `from_token` (ADR-164 決定 18). The transport crate cannot see this enum, so its spelling is a
    /// second copy — and a token core does not know reads as `internal`, which would put "Yagra
    /// could not read or write its own database" on screen for a revoked key. This is what holds
    /// the two together: for every fetch error, the token IS the token of the failure it maps to.
    #[test]
    fn a_collect_failure_token_is_the_one_the_sync_stores() {
        for e in MerakiFetchError::ALL {
            let mapped = MerakiSyncFailure::from(e);
            assert_eq!(
                e.token(),
                mapped.as_str(),
                "{e:?} travels as a token core reads back as something else"
            );
            assert_eq!(MerakiSyncFailure::from_token(e.token()), mapped, "{e:?}");
        }
    }

    #[test]
    fn every_fetch_error_has_a_reason_an_operator_can_act_on() {
        use MerakiSyncFailure as F;
        for (e, want) in [
            (MerakiFetchError::Config, F::Config),
            (MerakiFetchError::Host, F::Malformed),
            (MerakiFetchError::Auth(401), F::Auth),
            (MerakiFetchError::Auth(403), F::Auth),
            (MerakiFetchError::RateLimited, F::RateLimited),
            (MerakiFetchError::Status(503), F::Upstream),
            (MerakiFetchError::Network, F::Unreachable),
            (MerakiFetchError::Malformed, F::Malformed),
            (MerakiFetchError::Truncated, F::Truncated),
        ] {
            assert_eq!(F::from(e), want, "{e:?}");
        }
    }

    /// Both halves of "due": the row decides after a success, this struct after a failure.
    #[test]
    fn an_organization_is_due_by_its_row_and_backs_off_after_a_failure() {
        let now = Instant::now();
        let t0 = DateTime::from_timestamp(1_800_000_000, 0).expect("in range");
        let after = |secs: i64| t0 + chrono::Duration::seconds(secs);
        let mut s = SyncSchedule::default();

        // Never synced: due at once.
        assert!(s.is_due(&org_with(None, 300), t0, now));
        // Synced: not due until the interval has passed, then due.
        assert!(!s.is_due(&org_with(Some(t0), 300), after(299), now));
        assert!(s.is_due(&org_with(Some(t0), 300), after(300), now));
        // A stamp ahead of this clock is "just now", not overdue.
        assert!(!s.is_due(&org_with(Some(after(60)), 300), t0, now));
        // A row below the floor is held to the floor.
        assert!(!s.is_due(&org_with(Some(t0), 5), after(30), now));

        // A failure leaves `last_sync_at` alone, so the row still says "due". The schedule is what
        // stops the loop from asking a refused key again fifteen seconds later.
        let never = org_with(None, 300);
        s.failed(never.id, now);
        assert!(!s.is_due(&never, t0, now + Duration::from_secs(299)));
        assert!(s.is_due(&never, t0, now + Duration::from_secs(300)));
        // …and a success hands the decision back to the row.
        s.failed(never.id, now);
        s.succeeded(never.id);
        assert!(s.is_due(&never, t0, now));
    }

    // ── against a real database ─────────────────────────────────────────────────────────────

    /// A directory that answers what it is told to, and counts how often it was asked.
    struct FakeDirectory {
        answer: Mutex<Result<MerakiInventory, MerakiFetchError>>,
        asked: Mutex<u32>,
        roles: Mutex<RolesAnswer>,
        /// What every network's LAN read answers (ADR-164 決定 28). By default one VLAN at
        /// `10.0.0.1` — the address `listing` has always given its devices — so a test about
        /// something else sees an MX addressed and imported exactly as before 決定 28.
        lan: Mutex<MerakiNetworkLan>,
        /// Per-network answers that take the place of `lan` for the networks they name.
        lan_for: Mutex<HashMap<String, MerakiNetworkLan>>,
        /// Every network a LAN read was asked about, in order.
        lan_asked: Mutex<Vec<String>>,
        /// The rate each LAN read was asked to pace at, one entry per call.
        lan_rps: Mutex<Vec<f64>>,
        /// How many networks each call reaches before it stops as the transport does at its
        /// deadline — `None` reaches all of them.
        lan_reach: Mutex<Option<usize>>,
    }

    type RolesAnswer = Result<Vec<(String, Option<MerakiHaRole>)>, MerakiFetchError>;

    fn vlan(id: u32, ip: &str) -> yagra_transport::MerakiLanAddress {
        yagra_transport::MerakiLanAddress {
            vlan_id: Some(id),
            appliance_ip: ip.to_owned(),
        }
    }

    impl FakeDirectory {
        fn answering(answer: Result<MerakiInventory, MerakiFetchError>) -> Arc<Self> {
            Arc::new(Self {
                answer: Mutex::new(answer),
                asked: Mutex::new(0),
                roles: Mutex::new(Ok(Vec::new())),
                lan: Mutex::new(Ok(vec![vlan(1, "10.0.0.1")])),
                lan_for: Mutex::new(HashMap::new()),
                lan_asked: Mutex::new(Vec::new()),
                lan_rps: Mutex::new(Vec::new()),
                lan_reach: Mutex::new(None),
            })
        }

        fn lan_rps(&self) -> Vec<f64> {
            self.lan_rps.lock().expect("lan rps").clone()
        }

        /// Every later LAN read reaches only `n` networks, as one the deadline stopped does.
        fn lan_reaches(&self, n: Option<usize>) {
            *self.lan_reach.lock().expect("lan reach") = n;
        }

        fn lan_answer_for(&self, network: &str, lan: MerakiNetworkLan) {
            self.lan_for
                .lock()
                .expect("lan for")
                .insert(network.to_owned(), lan);
        }

        fn roles_answer(&self, roles: RolesAnswer) {
            *self.roles.lock().expect("roles") = roles;
        }

        fn lan_answer(&self, lan: MerakiNetworkLan) {
            *self.lan.lock().expect("lan") = lan;
        }

        fn lan_asked(&self) -> Vec<String> {
            self.lan_asked.lock().expect("lan asked").clone()
        }

        fn now_answers(&self, answer: Result<MerakiInventory, MerakiFetchError>) {
            *self.answer.lock().expect("answer") = answer;
        }

        fn asked(&self) -> u32 {
            *self.asked.lock().expect("asked")
        }
    }

    #[async_trait]
    impl MerakiDirectory for FakeDirectory {
        /// A sync never lists organizations; onboarding does, and `api/meraki.rs` tests that.
        async fn organizations(
            &self,
            _base_url: &str,
            _api_key: &str,
        ) -> Result<Vec<MerakiOrgInfo>, TransportError> {
            Ok(Vec::new())
        }

        async fn inventory(
            &self,
            _org: &MerakiOrg,
            _api_key: &str,
        ) -> Result<MerakiInventory, MerakiFetchError> {
            *self.asked.lock().expect("asked") += 1;
            self.answer.lock().expect("answer").clone()
        }

        async fn ha_roles(&self, _org: &MerakiOrg, _api_key: &str) -> RolesAnswer {
            self.roles.lock().expect("roles").clone()
        }

        async fn network_lans(
            &self,
            _org: &MerakiOrg,
            _api_key: &str,
            network_ids: &[String],
            _budget: Duration,
            rps: f64,
        ) -> Result<Vec<(String, MerakiNetworkLan)>, MerakiFetchError> {
            self.lan_rps.lock().expect("lan rps").push(rps);
            let reach = self
                .lan_reach
                .lock()
                .expect("lan reach")
                .unwrap_or(network_ids.len());
            let network_ids = &network_ids[..reach.min(network_ids.len())];
            self.lan_asked
                .lock()
                .expect("lan asked")
                .extend(network_ids.iter().cloned());
            let lan = self.lan.lock().expect("lan").clone();
            let lan_for = self.lan_for.lock().expect("lan for").clone();
            Ok(network_ids
                .iter()
                .map(|n| {
                    (
                        n.clone(),
                        lan_for.get(n).cloned().unwrap_or_else(|| lan.clone()),
                    )
                })
                .collect())
        }
    }

    fn listing(devices: &[(&str, Option<MerakiAvailability>)]) -> MerakiInventory {
        MerakiInventory {
            networks: vec![MerakiNetworkInfo {
                id: "N_1".into(),
                name: "HQ".into(),
            }],
            devices: devices
                .iter()
                .map(|(serial, availability)| MerakiInventoryDevice {
                    info: MerakiDeviceInfo {
                        serial: (*serial).into(),
                        name: format!("dev-{serial}"),
                        model: Some("MX67".into()),
                        product_type: "appliance".into(),
                        network_id: "N_1".into(),
                        lan_ip: Some("10.0.0.1".into()),
                    },
                    availability: *availability,
                })
                .collect(),
        }
    }

    struct Rig {
        sync: MerakiSync,
        orgs: Arc<MerakiOrgRepo>,
        inventory: Arc<MerakiInventoryRepo>,
        inflight: Arc<MerakiInflight>,
        directory: Arc<FakeDirectory>,
        org: Uuid,
    }

    impl Rig {
        async fn org(&self) -> MerakiOrg {
            self.orgs.get(self.org).await.expect("get").expect("org")
        }
    }

    /// One organization with a real sealed key, and a sync wired to a fake directory.
    async fn rig(pool: &sqlx::PgPool, first: Result<MerakiInventory, MerakiFetchError>) -> Rig {
        let creds = Arc::new(CredentialStore::new(pool.clone(), pgtest::kek()));
        let credential = creds
            .create(
                "Meraki API — Acme",
                crate::secrets::KIND_MERAKI_API,
                br#"{"api_key":"not-a-real-key"}"#,
            )
            .await
            .expect("seal key");
        let orgs = Arc::new(MerakiOrgRepo::new(pool.clone()));
        let org = orgs
            .create("123456", "Acme", "https://api.meraki.com", credential)
            .await
            .expect("create org");
        let inventory = Arc::new(MerakiInventoryRepo::new(pool.clone()));
        let inflight = Arc::new(MerakiInflight::new());
        let directory = FakeDirectory::answering(first);
        let resolver = Arc::new(ImportResolver::new(
            Arc::new(crate::groups::GroupRepo::new(pool.clone())),
            Arc::new(NodeRepo::from_pool(pool.clone())),
        ));
        let sync = MerakiSync::new(
            orgs.clone(),
            inventory.clone(),
            creds,
            directory.clone(),
            inflight.clone(),
            resolver,
        );
        Rig {
            sync,
            orgs,
            inventory,
            inflight,
            directory,
            org,
        }
    }

    const UP: Option<MerakiAvailability> = Some(MerakiAvailability::Online);
    const DOWN: Option<MerakiAvailability> = Some(MerakiAvailability::Dormant);

    /// The whole life of a row, against real SQL: found, unchanged, gone, back.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn a_sync_records_what_changed_and_only_that(pool: sqlx::PgPool) {
        let r = rig(&pool, Ok(listing(&[("Q2-A", UP), ("Q2-B", DOWN)]))).await;

        let first = r
            .sync
            .sync_org_scheduled(&r.org().await)
            .await
            .expect("first sync");
        assert_eq!(
            (first.devices, first.networks, first.newly_missing),
            (2, 1, 0)
        );
        assert_eq!(first.written, 3, "two devices and one network are new");
        let after = r.org().await;
        assert_eq!(
            (after.last_sync_ok, after.last_sync_error.clone()),
            (Some(true), None)
        );
        assert!(after.last_sync_at.is_some());

        let stored = r.inventory.stored(r.org).await.expect("stored");
        let online: Vec<(&str, bool)> = stored
            .iter()
            .map(|d| (d.serial.as_str(), d.first_online_at.is_some()))
            .collect();
        assert!(online.contains(&("Q2-A", true)));
        assert!(
            online.contains(&("Q2-B", false)),
            "a dormant device has never been online"
        );

        // The ordinary sync: nothing changed, nothing written.
        let again = r
            .sync
            .sync_org_scheduled(&r.org().await)
            .await
            .expect("second sync");
        assert_eq!(again.written, 0, "an unchanged listing rewrote rows");

        // Q2-A leaves the listing; Q2-B comes online for the first time.
        r.directory.now_answers(Ok(listing(&[("Q2-B", UP)])));
        let third = r
            .sync
            .sync_org_scheduled(&r.org().await)
            .await
            .expect("third sync");
        assert_eq!(third.newly_missing, 1);
        let stored = r.inventory.stored(r.org).await.expect("stored");
        let a = stored
            .iter()
            .find(|d| d.serial == "Q2-A")
            .expect("Q2-A kept");
        let b = stored.iter().find(|d| d.serial == "Q2-B").expect("Q2-B");
        assert!(a.missing_since.is_some());
        assert!(b.first_online_at.is_some());
        let first_online = a.first_online_at;
        let marked = a.missing_since;

        // Still gone: the mark must not move.
        r.sync
            .sync_org_scheduled(&r.org().await)
            .await
            .expect("fourth sync");
        let stored = r.inventory.stored(r.org).await.expect("stored");
        let a = stored.iter().find(|d| d.serial == "Q2-A").expect("Q2-A");
        assert_eq!(a.missing_since, marked, "missing_since was restamped");

        // Back again, offline: the mark clears and first_online_at is not restamped.
        r.directory
            .now_answers(Ok(listing(&[("Q2-A", DOWN), ("Q2-B", UP)])));
        r.sync
            .sync_org_scheduled(&r.org().await)
            .await
            .expect("fifth sync");
        let stored = r.inventory.stored(r.org).await.expect("stored");
        let a = stored.iter().find(|d| d.serial == "Q2-A").expect("Q2-A");
        assert_eq!(a.missing_since, None);
        assert_eq!(a.first_online_at, first_online, "first_online_at moved");
    }

    /// ADR-164 決定 26: the sync records each MX's warm-spare role — once; the next sync writes no
    /// role — and a roles read that fails costs nothing: the roles stay, the sync is still a
    /// success, and no device is marked missing. A pair is the other MX of the same network.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn the_sync_records_warm_spare_roles_and_a_failed_roles_read_costs_nothing(
        pool: sqlx::PgPool,
    ) {
        use crate::meraki_inventory::HaPair;
        let device = |serial: &str, product: &str, net: &str| MerakiInventoryDevice {
            info: MerakiDeviceInfo {
                serial: serial.into(),
                name: format!("dev-{serial}"),
                model: Some("MX85".into()),
                product_type: product.into(),
                network_id: net.into(),
                lan_ip: Some("10.0.0.1".into()),
            },
            availability: UP,
        };
        // N_1: a pair, and a cellular gateway beside it. N_2: one MX alone. N_3: three MX.
        let inventory = MerakiInventory {
            networks: ["N_1", "N_2", "N_3"]
                .iter()
                .map(|n| MerakiNetworkInfo {
                    id: (*n).into(),
                    name: format!("site-{n}"),
                })
                .collect(),
            devices: vec![
                device("Q2-P", "appliance", "N_1"),
                device("Q2-S", "appliance", "N_1"),
                device("Q2-G", "cellularGateway", "N_1"),
                device("Q2-1", "appliance", "N_2"),
                device("Q2-X", "appliance", "N_3"),
                device("Q2-Y", "appliance", "N_3"),
                device("Q2-Z", "appliance", "N_3"),
            ],
        };
        let r = rig(&pool, Ok(inventory)).await;
        let roles = |p: MerakiHaRole, s: MerakiHaRole| -> RolesAnswer {
            Ok(vec![
                ("Q2-P".into(), Some(p)),
                ("Q2-S".into(), Some(s)),
                ("Q2-1".into(), None),
                ("Q2-X".into(), Some(MerakiHaRole::Primary)),
                ("Q2-Y".into(), Some(MerakiHaRole::Spare)),
                ("Q2-Z".into(), Some(MerakiHaRole::Spare)),
            ])
        };
        r.directory
            .roles_answer(roles(MerakiHaRole::Primary, MerakiHaRole::Spare));

        let first = r
            .sync
            .sync_org_scheduled(&r.org().await)
            .await
            .expect("first sync");
        assert_eq!(
            first.written,
            7 + 3 + 5,
            "seven devices, three networks, five roles"
        );
        let pair = |serial: &'static str| {
            let inv = r.inventory.clone();
            let org = r.org;
            async move { inv.ha_pair(org, serial).await.expect("ha_pair") }
        };
        let p = pair("Q2-P").await.expect("Q2-P is in a pair");
        assert_eq!(p.role, MerakiHaRole::Primary);
        let partner = p.partner.expect("the other MX of N_1 — not the gateway");
        assert_eq!(
            (partner.serial.as_str(), partner.role),
            ("Q2-S", Some(MerakiHaRole::Spare))
        );
        assert_eq!(pair("Q2-1").await, None, "a single MX holds no role");
        assert_eq!(pair("Q2-G").await, None, "a gateway is not an MX");
        assert_eq!(
            pair("Q2-X").await,
            Some(HaPair {
                role: MerakiHaRole::Primary,
                partner: None
            }),
            "three MX in one network: no telling which is the pair"
        );

        // Unchanged roles: nothing written.
        let again = r
            .sync
            .sync_org_scheduled(&r.org().await)
            .await
            .expect("second sync");
        assert_eq!(again.written, 0);

        // The roles read fails: the sync still succeeds, the roles stay, nothing goes missing.
        r.directory.roles_answer(Err(MerakiFetchError::Auth(403)));
        r.sync
            .sync_org_scheduled(&r.org().await)
            .await
            .expect("a failed roles read does not fail the sync");
        let org = r.org().await;
        assert_eq!((org.last_sync_ok, org.last_sync_error), (Some(true), None));
        assert_eq!(
            pair("Q2-P").await.map(|p| p.role),
            Some(MerakiHaRole::Primary)
        );
        let stored = r.inventory.stored(r.org).await.expect("stored");
        assert!(stored.iter().all(|d| d.missing_since.is_none()));

        // The roles swap (someone reconfigured the pair): two rows written, read back.
        r.directory
            .roles_answer(roles(MerakiHaRole::Spare, MerakiHaRole::Primary));
        let swapped = r
            .sync
            .sync_org_scheduled(&r.org().await)
            .await
            .expect("third sync");
        assert_eq!(swapped.written, 2);
        assert_eq!(
            pair("Q2-P").await.map(|p| p.role),
            Some(MerakiHaRole::Spare)
        );
    }

    /// ADR-164 決定 3, the one that matters most: a sync that fails changes **nothing** it could be
    /// wrong about. Not a row of the inventory, and not the stamp the next sync is timed from.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn a_failed_sync_marks_nothing_missing_and_does_not_move_the_stamp(pool: sqlx::PgPool) {
        let r = rig(&pool, Ok(listing(&[("Q2-A", UP), ("Q2-B", UP)]))).await;
        r.sync
            .sync_org_scheduled(&r.org().await)
            .await
            .expect("first sync");
        let synced = r.org().await.last_sync_at.expect("stamped");
        let before = r.inventory.stored(r.org).await.expect("stored");

        for (answer, want) in [
            (MerakiFetchError::Truncated, MerakiSyncFailure::Truncated),
            (
                MerakiFetchError::RateLimited,
                MerakiSyncFailure::RateLimited,
            ),
            (MerakiFetchError::Auth(401), MerakiSyncFailure::Auth),
        ] {
            r.directory.now_answers(Err(answer));
            assert_eq!(
                r.sync.sync_org_scheduled(&r.org().await).await,
                Err(SyncError::Failed(want))
            );
            let org = r.org().await;
            assert_eq!(
                org.last_sync_at,
                Some(synced),
                "{want:?} moved last_sync_at"
            );
            assert_eq!(org.last_sync_ok, Some(false));
            assert_eq!(org.last_sync_error.as_deref(), Some(want.as_str()));
            let mut now = r.inventory.stored(r.org).await.expect("stored");
            let mut then = before.clone();
            now.sort_by(|x, y| x.serial.cmp(&y.serial));
            then.sort_by(|x, y| x.serial.cmp(&y.serial));
            assert_eq!(now, then, "{want:?} changed the inventory");
        }

        // And the next success clears the reason.
        r.directory
            .now_answers(Ok(listing(&[("Q2-A", UP), ("Q2-B", UP)])));
        r.sync
            .sync_org_scheduled(&r.org().await)
            .await
            .expect("recovered");
        let org = r.org().await;
        assert_eq!((org.last_sync_ok, org.last_sync_error), (Some(true), None));
    }

    /// ADR-169 決定 1, the periodic sync: the slow lane only. A slow collect in flight refuses it
    /// without touching the Dashboard or the row — and it never takes the fast lane instead, which
    /// is where it used to delay availability by a tick every time it ran. A finished sync, and a
    /// failed one, leave the lane free for the collector.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn the_scheduled_sync_waits_for_the_slow_lane_and_leaves_the_fast_one_alone(
        pool: sqlx::PgPool,
    ) {
        let r = rig(&pool, Ok(listing(&[("Q2-A", UP)]))).await;
        let ports = Uuid::new_v4();
        assert!(r.inflight.acquire_collect(
            r.org,
            MerakiLane::Slow,
            ports,
            MerakiTier::SwitchPorts,
            Duration::from_secs(300),
            Instant::now()
        ));

        assert_eq!(
            r.sync.sync_org_scheduled(&r.org().await).await,
            Err(SyncError::Busy)
        );
        assert!(
            !r.inflight
                .is_inflight(r.org, MerakiLane::Fast, Instant::now()),
            "the scheduled sync took the fast lane, where it delays availability"
        );
        assert_eq!(
            r.directory.asked(),
            0,
            "a busy organization was asked anyway"
        );
        let org = r.org().await;
        assert_eq!(
            (org.last_sync_ok, org.last_sync_at),
            (None, None),
            "being busy is not a failed sync"
        );

        r.inflight.complete(ports);
        r.sync
            .sync_org_scheduled(&r.org().await)
            .await
            .expect("sync");
        assert!(
            !r.inflight
                .is_inflight(r.org, MerakiLane::Slow, Instant::now()),
            "a finished sync kept the organization's slow lane"
        );

        // …and a failed one releases it too.
        r.directory.now_answers(Err(MerakiFetchError::Network));
        let _ = r.sync.sync_org_scheduled(&r.org().await).await;
        assert!(!r
            .inflight
            .is_inflight(r.org, MerakiLane::Slow, Instant::now()));
    }

    /// ADR-164 決定 32, "Sync now": the read it asks for runs in the **slow lane only** — it takes
    /// minutes, and the fast lane is availability's. While a collect holds the slow lane it waits
    /// (busy, the request standing, nothing asked); once it runs it reads every network — the one
    /// read a sync ago too — and clears the request and its progress. It releases its own lane only.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn a_requested_read_waits_for_the_slow_lane_and_reads_every_network(pool: sqlx::PgPool) {
        let r = rig(&pool, Ok(listing(&[("Q2-A", UP)]))).await;
        r.sync
            .sync_org_scheduled(&r.org().await)
            .await
            .expect("first sync");
        assert_eq!(r.directory.lan_asked(), ["N_1"]);
        assert!(r.orgs.request_full_sync(r.org).await.expect("request"));

        let ports = Uuid::new_v4();
        assert!(r.inflight.acquire_collect(
            r.org,
            MerakiLane::Slow,
            ports,
            MerakiTier::SwitchPorts,
            Duration::from_secs(300),
            Instant::now()
        ));
        assert_eq!(
            r.sync.sync_org_requested(&r.org().await).await,
            Err(SyncError::Busy)
        );
        assert!(
            !r.inflight
                .is_inflight(r.org, MerakiLane::Fast, Instant::now()),
            "the requested read took the fast lane"
        );
        assert_eq!(
            r.directory.asked(),
            1,
            "a busy organization was asked anyway"
        );
        assert!(
            r.org().await.full_sync_requested_at.is_some(),
            "a read that never ran answered the request"
        );

        r.inflight.complete(ports);
        r.sync
            .sync_org_requested(&r.org().await)
            .await
            .expect("the requested read");
        assert_eq!(
            r.directory.lan_asked(),
            ["N_1", "N_1"],
            "a network read a sync ago was not read again"
        );
        let org = r.org().await;
        assert_eq!(
            (org.full_sync_requested_at, org.full_sync),
            (None, None),
            "the request or its progress outlived the read"
        );
        assert!(!r
            .inflight
            .is_inflight(r.org, MerakiLane::Slow, Instant::now()));

        // A request answered by a read that failed is answered too: otherwise one refused key would
        // be asked again every tick.
        assert!(r.orgs.request_full_sync(r.org).await.expect("request"));
        r.directory.now_answers(Err(MerakiFetchError::Auth(401)));
        assert_eq!(
            r.sync.sync_org_requested(&r.org().await).await,
            Err(SyncError::Failed(MerakiSyncFailure::Auth))
        );
        assert_eq!(r.org().await.full_sync_requested_at, None);
    }

    /// A key that cannot be opened is a recorded failure, and the Dashboard is never asked.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn an_unusable_key_fails_the_sync_before_anything_is_sent(pool: sqlx::PgPool) {
        let r = rig(&pool, Ok(listing(&[("Q2-A", UP)]))).await;
        // Same table, a kind the resolver refuses.
        let wrong = pgtest::credential(&pool, "not-a-meraki-key", "snmp_v2c").await;
        sqlx::query("UPDATE meraki_orgs SET credential_id = $2 WHERE id = $1")
            .bind(r.org)
            .bind(wrong)
            .execute(&pool)
            .await
            .expect("repoint");

        assert_eq!(
            r.sync.sync_org_scheduled(&r.org().await).await,
            Err(SyncError::Failed(MerakiSyncFailure::Credential))
        );
        assert_eq!(r.directory.asked(), 0);
        assert_eq!(r.org().await.last_sync_error.as_deref(), Some("credential"));
    }

    // ── the import stage (ADR-164 Inc.4) ─────────────────────────────────────────────────────

    /// The serials that are nodes of `org` right now, sorted.
    async fn nodes_of(pool: &sqlx::PgPool, org: Uuid) -> Vec<String> {
        sqlx::query_scalar("SELECT serial FROM meraki_devices WHERE org_id = $1 ORDER BY serial")
            .bind(org)
            .fetch_all(pool)
            .await
            .expect("bound serials")
    }

    /// The folder a device's node is filed in.
    async fn folder_of(pool: &sqlx::PgPool, serial: &str) -> Option<Uuid> {
        sqlx::query_scalar(
            "SELECT n.group_id FROM nodes n JOIN meraki_devices d ON d.node_id = n.id \
             WHERE d.serial = $1",
        )
        .bind(serial)
        .fetch_one(pool)
        .await
        .expect("the node's folder")
    }

    /// ADR-164 決定 5: a device becomes a node when it is in a watched network and Meraki has
    /// reported it online — and a new organization watches a network from the sync that finds it.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn a_sync_imports_what_has_been_online_and_only_that(pool: sqlx::PgPool) {
        let r = rig(&pool, Ok(listing(&[("Q2-A", UP), ("Q2-B", DOWN)]))).await;
        assert!(
            r.org().await.import_devices,
            "migration 0125: an organization added from here on imports automatically"
        );
        let generation = crate::config_gen::current();

        let first = r
            .sync
            .sync_org_scheduled(&r.org().await)
            .await
            .expect("first sync");
        assert_eq!((first.imported, first.over_cap), (1, 0));
        assert_eq!(
            nodes_of(&pool, r.org).await,
            ["Q2-A"],
            "a device Meraki has never reported online was imported, or the online one was not"
        );
        assert_eq!(
            r.orgs.monitored_network_ids(r.org).await.expect("scope"),
            ["N_1"],
            "the network the sync found was not watched"
        );
        assert_eq!(
            folder_of(&pool, "Q2-A").await,
            Some(crate::meraki::network_group_id(r.org, "N_1")),
            "with no IP range anywhere the device goes under Organization ▸ Network"
        );
        assert!(
            crate::config_gen::current() > generation,
            "an import that created a node must make the scheduler rebuild its round"
        );

        // The ordinary sync: nothing new to import, and nothing written.
        let again = r
            .sync
            .sync_org_scheduled(&r.org().await)
            .await
            .expect("second sync");
        assert_eq!((again.imported, again.written), (0, 0));

        // Q2-B is plugged in. The sync that first sees it online imports it.
        r.directory
            .now_answers(Ok(listing(&[("Q2-A", UP), ("Q2-B", UP)])));
        let third = r
            .sync
            .sync_org_scheduled(&r.org().await)
            .await
            .expect("third sync");
        assert_eq!(third.imported, 1);
        assert_eq!(nodes_of(&pool, r.org).await, ["Q2-A", "Q2-B"]);
    }

    /// A device an operator deleted stays deleted. `imported_at` outlives the node, and that is the
    /// whole mechanism — without it the next sync, five minutes later, would put the node back.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn a_device_an_operator_deleted_is_not_imported_again(pool: sqlx::PgPool) {
        let r = rig(&pool, Ok(listing(&[("Q2-A", UP)]))).await;
        r.sync
            .sync_org_scheduled(&r.org().await)
            .await
            .expect("first sync");
        assert_eq!(nodes_of(&pool, r.org).await, ["Q2-A"]);

        sqlx::query("DELETE FROM nodes WHERE id = $1")
            .bind(crate::meraki::device_node_id("Q2-A"))
            .execute(&pool)
            .await
            .expect("the operator deletes the node");

        let after = r
            .sync
            .sync_org_scheduled(&r.org().await)
            .await
            .expect("second sync");
        assert_eq!(
            after.imported, 0,
            "the sync put back a node somebody deleted"
        );
        assert!(nodes_of(&pool, r.org).await.is_empty());
        let devices = r.inventory.devices(r.org).await.expect("devices");
        assert_eq!(
            devices[0].state,
            crate::meraki_inventory::MerakiDeviceState::Deleted
        );
    }

    /// Both halves of "off", and the trap behind switching it on later: an organization that was
    /// imported by hand has networks nobody watched, and turning the switch on watches none of them.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn with_automatic_import_off_nothing_is_imported_and_no_network_is_watched(
        pool: sqlx::PgPool,
    ) {
        let r = rig(&pool, Ok(listing(&[("Q2-A", UP)]))).await;
        assert!(r
            .orgs
            .set_import_settings(r.org, false, true, 1000)
            .await
            .expect("switch off"));

        let off = r
            .sync
            .sync_org_scheduled(&r.org().await)
            .await
            .expect("sync, off");
        assert_eq!(off.imported, 0);
        assert!(nodes_of(&pool, r.org).await.is_empty());
        assert!(
            r.orgs
                .monitored_network_ids(r.org)
                .await
                .expect("scope")
                .is_empty(),
            "a network was watched on behalf of an organization that imports nothing"
        );

        // Switched on afterwards: N_1 is already known, so it keeps the flag it has, and a device
        // in a network nobody watches is not imported.
        r.orgs
            .set_import_settings(r.org, true, true, 1000)
            .await
            .expect("switch on");
        let on = r
            .sync
            .sync_org_scheduled(&r.org().await)
            .await
            .expect("sync, on");
        assert_eq!(
            on.imported, 0,
            "a device in an unwatched network was imported"
        );

        // Watching the network is the operator's step — "Watch all" on the organization's page.
        r.orgs
            .set_networks_monitored(r.org, &["N_1".to_owned()], true)
            .await
            .expect("watch");
        let watched = r
            .sync
            .sync_org_scheduled(&r.org().await)
            .await
            .expect("sync, watched");
        assert_eq!(watched.imported, 1);
        assert_eq!(nodes_of(&pool, r.org).await, ["Q2-A"]);
    }

    /// The cap stops the import and is never silent about it: what it left out is on the row, goes
    /// back to zero when the cap is raised, and does not outlive the switch.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn the_cap_stops_the_import_and_the_row_says_what_it_left_out(pool: sqlx::PgPool) {
        let r = rig(
            &pool,
            Ok(listing(&[("Q2-A", UP), ("Q2-B", UP), ("Q2-C", UP)])),
        )
        .await;
        assert_eq!(
            r.org().await.max_devices,
            1000,
            "migration 0125's column default"
        );
        r.orgs
            .set_import_settings(r.org, true, true, 1)
            .await
            .expect("cap at one");

        let capped = r
            .sync
            .sync_org_scheduled(&r.org().await)
            .await
            .expect("capped sync");
        assert_eq!((capped.imported, capped.over_cap), (1, 2));
        assert_eq!(nodes_of(&pool, r.org).await.len(), 1);
        assert_eq!(r.org().await.devices_over_cap, 2);
        assert_eq!(
            r.orgs.record_over_cap(r.org, 2).await.expect("rewrite"),
            0,
            "an unchanged count must write nothing: this runs every five minutes"
        );

        // At the cap: nothing imported, the same two reported.
        let full = r
            .sync
            .sync_org_scheduled(&r.org().await)
            .await
            .expect("full sync");
        assert_eq!((full.imported, full.over_cap), (0, 2));

        // Switching the import off clears a number that would otherwise describe a cap nothing is
        // applying — and so does the next sync of an organization that is off.
        r.orgs
            .set_import_settings(r.org, false, true, 1)
            .await
            .expect("switch off");
        assert_eq!(r.org().await.devices_over_cap, 0);

        r.orgs
            .set_import_settings(r.org, true, true, 50)
            .await
            .expect("raise the cap");
        let raised = r
            .sync
            .sync_org_scheduled(&r.org().await)
            .await
            .expect("raised sync");
        assert_eq!((raised.imported, raised.over_cap), (2, 0));
        assert_eq!(r.org().await.devices_over_cap, 0);
        assert_eq!(nodes_of(&pool, r.org).await, ["Q2-A", "Q2-B", "Q2-C"]);

        // The CHECK is the last line of defence behind the API's own bounds.
        assert!(r
            .orgs
            .set_import_settings(r.org, true, true, 0)
            .await
            .is_err());
        assert!(r
            .orgs
            .set_import_settings(r.org, true, true, 50_001)
            .await
            .is_err());
    }

    /// The sync files a device exactly as a manual import does — the folder whose IP range holds
    /// its address — unless the organization says not to.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn the_sync_files_by_ip_range_unless_the_organization_says_not_to(pool: sqlx::PgPool) {
        let site = pgtest::group(&pool, "Matsuyama").await;
        pgtest::prefix(&pool, site, "10.0.0.0/24").await;
        // `listing` gives every device 10.0.0.1.
        let r = rig(&pool, Ok(listing(&[("Q2-A", UP)]))).await;

        r.sync
            .sync_org_scheduled(&r.org().await)
            .await
            .expect("first sync");
        assert_eq!(folder_of(&pool, "Q2-A").await, Some(site));
        assert_eq!(
            pgtest::rows(&pool, "node_groups").await,
            2,
            "the site and the organization's own folder; a matched device needs no network folder"
        );

        r.orgs
            .set_import_settings(r.org, true, false, 1000)
            .await
            .expect("stop filing by range");
        r.directory
            .now_answers(Ok(listing(&[("Q2-A", UP), ("Q2-B", UP)])));
        r.sync
            .sync_org_scheduled(&r.org().await)
            .await
            .expect("second sync");
        assert_eq!(
            folder_of(&pool, "Q2-B").await,
            Some(crate::meraki::network_group_id(r.org, "N_1"))
        );
        assert_eq!(
            folder_of(&pool, "Q2-A").await,
            Some(site),
            "a node that is already filed is never moved"
        );
    }

    /// Migration 0124, as a behaviour: the floor is one minute, and the default five.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn the_sync_interval_defaults_to_five_minutes_and_may_go_down_to_one(pool: sqlx::PgPool) {
        let r = rig(&pool, Ok(MerakiInventory::default())).await;
        assert_eq!(r.org().await.inventory_secs, 300);

        let set = |secs: i32| {
            let orgs = r.orgs.clone();
            let id = r.org;
            async move {
                orgs.update_cadence(
                    id,
                    &crate::meraki::MerakiCadence {
                        availability_secs: 300,
                        uplink_secs: 300,
                        traffic_secs: 1800,
                        inventory_secs: secs,
                        switch_ports_secs: None,
                        wireless_secs: None,
                        enabled_tiers: Vec::new(),
                        target_rps: 2.0,
                    },
                )
                .await
            }
        };
        assert!(set(60).await.expect("one minute is allowed"));
        assert!(set(59).await.is_err(), "the CHECK let 59 seconds through");
    }

    // ── what an imported node follows (ADR-164 Inc.8, 決定 14) ───────────────────────────────

    /// One device, described freely. Both networks are always listed, so a move between them is a
    /// move and not a disappearance.
    fn listing_of(
        serial: &str,
        name: &str,
        network: &str,
        lan_ip: Option<&str>,
    ) -> MerakiInventory {
        MerakiInventory {
            networks: ["N_1", "N_2"]
                .iter()
                .map(|id| MerakiNetworkInfo {
                    id: (*id).into(),
                    name: format!("net {id}"),
                })
                .collect(),
            devices: vec![MerakiInventoryDevice {
                info: MerakiDeviceInfo {
                    serial: serial.into(),
                    name: name.into(),
                    model: Some("MR46".into()),
                    product_type: "wireless".into(),
                    network_id: network.into(),
                    lan_ip: lan_ip.map(str::to_owned),
                },
                availability: UP,
            }],
        }
    }

    /// A device's node as it stands: `(name, address, the binding's network)`.
    async fn node_as_it_stands(pool: &sqlx::PgPool, serial: &str) -> (String, String, String) {
        let row = sqlx::query(
            "SELECT n.name, host(n.address) AS address, d.network_id \
             FROM nodes n JOIN meraki_devices d ON d.node_id = n.id WHERE d.serial = $1",
        )
        .bind(serial)
        .fetch_one(pool)
        .await
        .expect("the device's node");
        (
            row.try_get("name").expect("name"),
            row.try_get("address").expect("address"),
            row.try_get("network_id").expect("network"),
        )
    }

    /// The three things a node takes from the Dashboard, against real SQL — and the one it never
    /// does: it stays in the folder it was filed in.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn an_imported_node_follows_the_dashboard_and_stays_where_it_was_filed(
        pool: sqlx::PgPool,
    ) {
        let r = rig(
            &pool,
            Ok(listing_of("Q2-A", "ap-1", "N_1", Some("10.0.0.1"))),
        )
        .await;
        let first = r
            .sync
            .sync_org_scheduled(&r.org().await)
            .await
            .expect("first sync");
        assert_eq!((first.imported, first.followed), (1, 0));
        let filed = folder_of(&pool, "Q2-A").await;
        let generation = crate::config_gen::current();

        r.directory
            .now_answers(Ok(listing_of("Q2-A", "lobby-ap", "N_2", Some("10.0.0.7"))));
        let second = r
            .sync
            .sync_org_scheduled(&r.org().await)
            .await
            .expect("second sync");
        assert_eq!(second.followed, 1, "one node, counted once: {second:?}");
        assert_eq!(
            node_as_it_stands(&pool, "Q2-A").await,
            ("lobby-ap".into(), "10.0.0.7".into(), "N_2".into())
        );
        // Same transaction: the row that made the rename visible carries the new name too.
        let stored = r.inventory.stored(r.org).await.expect("stored");
        assert_eq!(stored[0].name, "lobby-ap");
        assert_eq!(
            folder_of(&pool, "Q2-A").await,
            filed,
            "a device that changed network was moved to another folder (決定 6)"
        );
        assert!(
            crate::config_gen::current() > generation,
            "a re-addressed node is not re-derived into the map until the generation moves"
        );

        // The ordinary sync again: everything agrees, nothing is written, nobody follows.
        let third = r
            .sync
            .sync_org_scheduled(&r.org().await)
            .await
            .expect("third sync");
        assert_eq!((third.written, third.followed), (0, 0), "{third:?}");
    }

    /// A name an operator chose survives Meraki's rename — decided by the plan when the sync reads
    /// the node after the rename, and by the statement when the rename lands in between.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn a_name_an_operator_chose_survives_a_rename_in_meraki(pool: sqlx::PgPool) {
        let r = rig(
            &pool,
            Ok(listing_of("Q2-A", "ap-1", "N_1", Some("10.0.0.1"))),
        )
        .await;
        r.sync
            .sync_org_scheduled(&r.org().await)
            .await
            .expect("first sync");
        sqlx::query("UPDATE nodes SET name = 'Reception (do not touch)' WHERE id = $1")
            .bind(crate::meraki::device_node_id("Q2-A"))
            .execute(&pool)
            .await
            .expect("the operator renames the node");

        // The statement's own guard, alone: a plan made before the operator's rename still names
        // the old name, which is what a sync that read a moment earlier would hold.
        let stale_plan = crate::meraki_inventory::SyncPlan {
            follows: vec![crate::meraki_inventory::NodeFollow {
                serial: "Q2-A".into(),
                rename: Some(("ap-1".into(), "lobby-ap".into())),
                address: None,
                network_id: None,
            }],
            ..Default::default()
        };
        let applied = r
            .inventory
            .apply(r.org, &stale_plan)
            .await
            .expect("apply the stale plan");
        assert_eq!(
            applied.followed, 0,
            "the rename overwrote an operator's name"
        );
        assert_eq!(
            node_as_it_stands(&pool, "Q2-A").await.0,
            "Reception (do not touch)"
        );

        // And through the sync: the inventory takes Meraki's new name, the node keeps its own.
        r.directory
            .now_answers(Ok(listing_of("Q2-A", "lobby-ap", "N_1", Some("10.0.0.1"))));
        let second = r
            .sync
            .sync_org_scheduled(&r.org().await)
            .await
            .expect("second sync");
        assert_eq!(second.followed, 0, "{second:?}");
        assert_eq!(second.written, 1, "the inventory row still follows Meraki");
        assert_eq!(
            node_as_it_stands(&pool, "Q2-A").await.0,
            "Reception (do not touch)"
        );
    }

    /// ADR-164 Inc.11. The inventory rows go out as one statement per chunk, eight arrays side by
    /// side. Against real SQL: every row lands with **its own** columns (NULLs included — an array
    /// that slipped by one would still insert the right number of rows), a second write keeps the
    /// two timestamps the first one stamped and clears `missing_since`, and a serial planned twice
    /// costs that row rather than the statement.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn a_plan_of_many_rows_lands_whole_and_a_second_write_keeps_what_the_first_stamped(
        pool: sqlx::PgPool,
    ) {
        use crate::meraki_inventory::{DeviceWrite, SeenDevice, SyncPlan};
        let r = rig(&pool, Ok(MerakiInventory::default())).await;
        let device =
            |serial: &str, name: &str, model: Option<&str>, lan_ip: Option<&str>| SeenDevice {
                serial: serial.into(),
                name: name.into(),
                model: model.map(str::to_owned),
                product_type: format!("type-of-{serial}"),
                network_id: format!("N_{serial}"),
                lan_ip: lan_ip.map(|ip| ip.parse().expect("ip")),
                online: true,
            };
        let bound_at = chrono::DateTime::from_timestamp(1_800_000_000, 0).expect("in range");
        let first = SyncPlan {
            writes: vec![
                DeviceWrite {
                    device: device("Q2-A", "alpha", Some("MX67"), Some("10.0.0.1")),
                    first_online: true,
                    imported_at: None,
                },
                DeviceWrite {
                    device: device("Q2-B", "", None, None),
                    first_online: false,
                    imported_at: Some(bound_at),
                },
                DeviceWrite {
                    device: device("Q2-C", "gamma", Some("MR46"), Some("2001:db8::7")),
                    first_online: false,
                    imported_at: None,
                },
                // The same serial again: PostgreSQL refuses an upsert naming a row twice.
                DeviceWrite {
                    device: device("Q2-A", "a second alpha", None, None),
                    first_online: false,
                    imported_at: None,
                },
            ],
            ..Default::default()
        };
        let applied = r.inventory.apply(r.org, &first).await.expect("first write");
        assert_eq!(applied.rows, 3, "one row per serial: {applied:?}");

        let stored = |rows: Vec<crate::meraki_inventory::StoredDevice>| {
            let mut rows = rows;
            rows.sort_by(|a, b| a.serial.cmp(&b.serial));
            rows
        };
        let rows = stored(r.inventory.stored(r.org).await.expect("stored"));
        let described: Vec<_> = rows
            .iter()
            .map(|d| {
                (
                    d.serial.as_str(),
                    d.name.as_str(),
                    d.model.as_deref(),
                    d.product_type.as_str(),
                    d.network_id.as_str(),
                    d.lan_ip.map(|ip| ip.to_string()),
                )
            })
            .collect();
        assert_eq!(
            described,
            [
                (
                    "Q2-A",
                    "alpha",
                    Some("MX67"),
                    "type-of-Q2-A",
                    "N_Q2-A",
                    Some("10.0.0.1".to_owned())
                ),
                ("Q2-B", "", None, "type-of-Q2-B", "N_Q2-B", None),
                (
                    "Q2-C",
                    "gamma",
                    Some("MR46"),
                    "type-of-Q2-C",
                    "N_Q2-C",
                    Some("2001:db8::7".to_owned())
                ),
            ]
        );
        // The two transitions landed on the rows that asked for them, and on no other.
        let stamps: Vec<_> = rows
            .iter()
            .map(|d| (d.first_online_at.is_some(), d.imported_at))
            .collect();
        assert_eq!(
            stamps,
            [(true, None), (false, Some(bound_at)), (false, None)]
        );
        let first_online_at = rows[0].first_online_at;

        // A later sync describes A anew, asks for both stamps again, and finds it back from missing.
        sqlx::query("UPDATE meraki_inventory SET missing_since = now() WHERE serial = 'Q2-A'")
            .execute(&pool)
            .await
            .expect("mark missing");
        let later = chrono::DateTime::from_timestamp(1_900_000_000, 0).expect("in range");
        let second = SyncPlan {
            writes: vec![
                DeviceWrite {
                    device: device("Q2-A", "alpha renamed", Some("MX68"), None),
                    first_online: true,
                    imported_at: Some(later),
                },
                DeviceWrite {
                    device: device("Q2-B", "beta", None, None),
                    first_online: true,
                    imported_at: Some(later),
                },
            ],
            ..Default::default()
        };
        let applied = r
            .inventory
            .apply(r.org, &second)
            .await
            .expect("second write");
        assert_eq!(applied.rows, 2);
        let rows = stored(r.inventory.stored(r.org).await.expect("stored"));
        assert_eq!(
            (
                rows[0].name.as_str(),
                rows[0].model.as_deref(),
                rows[0].lan_ip
            ),
            ("alpha renamed", Some("MX68"), None),
            "the description follows Meraki, an address it stopped reporting included"
        );
        assert_eq!(
            rows[0].first_online_at, first_online_at,
            "a second write moved the moment the device was first seen online"
        );
        assert_eq!(
            rows[0].imported_at,
            Some(later),
            "A had none, so it takes one"
        );
        assert_eq!(rows[0].missing_since, None, "a listed device is back");
        assert!(
            rows[1].first_online_at.is_some(),
            "B is online for the first time"
        );
        assert_eq!(
            rows[1].imported_at,
            Some(bound_at),
            "a second write moved the moment B's node was bound"
        );
        assert_eq!(
            rows[2].name, "gamma",
            "C was not in the plan and was written"
        );
    }

    /// A node imported while Meraki reported no address stands at `0.0.0.0`, and nothing but this
    /// could ever change that — no screen edits a node's address. It gets one from the first sync
    /// that has one, and a later sync that has none again leaves it alone.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn a_node_imported_without_an_address_gets_one_and_never_loses_it(pool: sqlx::PgPool) {
        let r = rig(&pool, Ok(listing_of("Q2-A", "ap-1", "N_1", None))).await;
        r.sync
            .sync_org_scheduled(&r.org().await)
            .await
            .expect("first sync");
        assert_eq!(node_as_it_stands(&pool, "Q2-A").await.1, "0.0.0.0");

        r.directory
            .now_answers(Ok(listing_of("Q2-A", "ap-1", "N_1", Some("10.0.0.9"))));
        let second = r
            .sync
            .sync_org_scheduled(&r.org().await)
            .await
            .expect("second sync");
        assert_eq!(second.followed, 1);
        assert_eq!(node_as_it_stands(&pool, "Q2-A").await.1, "10.0.0.9");

        r.directory
            .now_answers(Ok(listing_of("Q2-A", "ap-1", "N_1", None)));
        let third = r
            .sync
            .sync_org_scheduled(&r.org().await)
            .await
            .expect("third sync");
        assert_eq!(third.followed, 0, "{third:?}");
        assert_eq!(
            node_as_it_stands(&pool, "Q2-A").await.1,
            "10.0.0.9",
            "a listing with no address turned a good address into none"
        );
    }

    /// ADR-164 決定 15. Collection asks about watched networks only, so a node whose network is not
    /// watched goes quiet. The count on the organization's row has to be the number of rows the
    /// device list marks — they are two queries, and one number on two screens.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn the_count_of_nodes_nothing_is_collected_for_is_what_the_device_list_marks(
        pool: sqlx::PgPool,
    ) {
        use crate::meraki_inventory::MerakiDeviceState;
        // Two become nodes; the dormant one never does, and must not be counted wherever it sits.
        let r = rig(
            &pool,
            Ok(listing(&[("Q2-A", UP), ("Q2-B", UP), ("Q2-C", DOWN)])),
        )
        .await;
        r.sync
            .sync_org_scheduled(&r.org().await)
            .await
            .expect("first sync");
        assert_eq!(nodes_of(&pool, r.org).await, ["Q2-A", "Q2-B"]);

        let counted = |r: &Rig| {
            let inventory = r.inventory.clone();
            let org = r.org;
            async move {
                let counts = inventory.counts().await.expect("counts");
                let marked = inventory
                    .devices(org)
                    .await
                    .expect("devices")
                    .iter()
                    .filter(|d| d.state == MerakiDeviceState::Monitored && !d.network_monitored)
                    .count();
                (
                    counts
                        .get(&org)
                        .copied()
                        .unwrap_or_default()
                        .monitored_unwatched,
                    marked,
                )
            }
        };
        assert_eq!(counted(&r).await, (0, 0), "the network is watched");

        r.orgs
            .set_networks_monitored(r.org, &["N_1".to_owned()], false)
            .await
            .expect("stop watching the network");
        assert_eq!(
            counted(&r).await,
            (2, 2),
            "both nodes went quiet; the device that was never a node is not one of them"
        );

        r.orgs
            .set_networks_monitored(r.org, &["N_1".to_owned()], true)
            .await
            .expect("watch it again");
        assert_eq!(counted(&r).await, (0, 0));
    }

    // ── an MX's address from its network's LAN side (ADR-164 決定 28) ─────────────────────────

    /// The case the decision was made on, end to end: VLAN 1 left at a default subnet, the site's own
    /// VLAN next, a folder holding the site's range. The MX is addressed and filed by the site's VLAN
    /// — never by the address the listing carries, which for an MX is its WAN — and a network read
    /// once is not read again the next sync.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn an_mx_is_addressed_and_filed_by_the_vlan_inside_a_folders_range(pool: sqlx::PgPool) {
        let site = pgtest::group(&pool, "site-a").await;
        pgtest::prefix(&pool, site, "10.20.0.0/16").await;
        // `listing` carries 10.0.0.1 on the device, which is not in the site's range.
        let r = rig(&pool, Ok(listing(&[("Q2-A", UP)]))).await;
        r.directory
            .lan_answer(Ok(vec![vlan(10, "10.20.0.1"), vlan(1, "192.168.128.1")]));

        r.sync
            .sync_org_scheduled(&r.org().await)
            .await
            .expect("first sync");
        assert_eq!(node_as_it_stands(&pool, "Q2-A").await.1, "10.20.0.1");
        assert_eq!(folder_of(&pool, "Q2-A").await, Some(site));
        assert_eq!(r.directory.lan_asked(), ["N_1"]);
        let row = r.inventory.stored(r.org).await.expect("stored");
        assert_eq!(row[0].lan_ip, Some("10.20.0.1".parse().expect("ip")));

        let again = r
            .sync
            .sync_org_scheduled(&r.org().await)
            .await
            .expect("second sync");
        assert_eq!((again.written, again.followed), (0, 0), "{again:?}");
        assert_eq!(
            r.directory.lan_asked(),
            ["N_1"],
            "a network read a sync ago is not read again"
        );
    }

    /// An MX whose network could not be read has no address yet, and importing it would file it by
    /// none — for good, since a node is never moved (決定 6). It waits, the sync still succeeds, and
    /// the sync that reads its network imports it.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn an_mx_is_imported_only_once_its_network_has_been_read(pool: sqlx::PgPool) {
        let site = pgtest::group(&pool, "site-a").await;
        pgtest::prefix(&pool, site, "10.20.0.0/16").await;
        let r = rig(&pool, Ok(listing(&[("Q2-A", UP)]))).await;
        r.directory.lan_answer(Err(MerakiFetchError::Status(500)));

        let first = r
            .sync
            .sync_org_scheduled(&r.org().await)
            .await
            .expect("first sync");
        assert_eq!(first.imported, 0);
        assert!(nodes_of(&pool, r.org).await.is_empty());
        let listed = r.inventory.devices(r.org).await.expect("devices");
        assert!(listed[0].lan_pending);
        assert_eq!(listed[0].lan_ip, None, "no WAN address stands in for it");
        assert_eq!(r.org().await.last_sync_ok, Some(true));

        r.directory.lan_answer(Ok(vec![vlan(10, "10.20.0.1")]));
        let second = r
            .sync
            .sync_org_scheduled(&r.org().await)
            .await
            .expect("second sync");
        assert_eq!(second.imported, 1);
        assert_eq!(node_as_it_stands(&pool, "Q2-A").await.1, "10.20.0.1");
        assert_eq!(folder_of(&pool, "Q2-A").await, Some(site));
    }

    /// A node that carries the address it was given before 決定 28 — its WAN — follows to its LAN
    /// address; and a later read that fails, once the network is due again, takes nothing away.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn a_wan_address_follows_to_the_lan_and_a_failed_reread_keeps_it(pool: sqlx::PgPool) {
        let r = rig(&pool, Ok(listing(&[("Q2-A", UP)]))).await;
        r.directory.lan_answer(Ok(vec![vlan(10, "10.20.0.1")]));
        r.sync
            .sync_org_scheduled(&r.org().await)
            .await
            .expect("first sync");
        // What an upgrade finds: the node at the address the old core took from `wan1Ip`.
        sqlx::query("UPDATE nodes SET address = '198.51.100.20'::inet")
            .execute(&pool)
            .await
            .expect("the old address");

        let second = r
            .sync
            .sync_org_scheduled(&r.org().await)
            .await
            .expect("second sync");
        assert_eq!(second.followed, 1);
        assert_eq!(node_as_it_stands(&pool, "Q2-A").await.1, "10.20.0.1");

        sqlx::query("UPDATE meraki_org_networks SET lan_read_at = now() - interval '2 days'")
            .execute(&pool)
            .await
            .expect("make the network due");
        r.directory.lan_answer(Err(MerakiFetchError::Status(500)));
        let third = r
            .sync
            .sync_org_scheduled(&r.org().await)
            .await
            .expect("third sync");
        assert_eq!((third.written, third.followed), (0, 0), "{third:?}");
        assert_eq!(r.directory.lan_asked(), ["N_1", "N_1"]);
        assert_eq!(node_as_it_stands(&pool, "Q2-A").await.1, "10.20.0.1");
        let stale: bool = sqlx::query_scalar(
            "SELECT lan_read_at < now() - interval '1 day' FROM meraki_org_networks",
        )
        .fetch_one(&pool)
        .await
        .expect("read stamp");
        assert!(stale, "a failed read must not stamp the network as read");
    }

    /// What the lab found on 2026-09-23, end to end: two sites reuse one guest subnet, a folder's
    /// range happens to hold it, and neither site's own VLAN is in any range. Each MX takes its own
    /// VLAN address — never the reused one — and so neither is filed into that folder.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn a_guest_subnet_several_sites_reuse_is_never_an_mx_address(pool: sqlx::PgPool) {
        let home = pgtest::group(&pool, "home").await;
        pgtest::prefix(&pool, home, "192.168.50.0/24").await;
        let mx = |serial: &str, network: &str| MerakiInventoryDevice {
            info: MerakiDeviceInfo {
                serial: serial.into(),
                name: format!("mx-{serial}"),
                model: Some("MX67".into()),
                product_type: "appliance".into(),
                network_id: network.into(),
                lan_ip: None,
            },
            availability: UP,
        };
        let listing = MerakiInventory {
            networks: ["N_1", "N_2"]
                .iter()
                .map(|id| MerakiNetworkInfo {
                    id: (*id).into(),
                    name: format!("site {id}"),
                })
                .collect(),
            devices: vec![mx("Q2-A", "N_1"), mx("Q2-B", "N_2")],
        };
        let r = rig(&pool, Ok(listing)).await;
        r.directory.lan_answer_for(
            "N_1",
            Ok(vec![vlan(5, "192.168.50.1"), vlan(10, "10.1.0.1")]),
        );
        r.directory.lan_answer_for(
            "N_2",
            Ok(vec![vlan(5, "192.168.50.1"), vlan(10, "10.2.0.1")]),
        );

        r.sync
            .sync_org_scheduled(&r.org().await)
            .await
            .expect("first sync");
        assert_eq!(node_as_it_stands(&pool, "Q2-A").await.1, "10.1.0.1");
        assert_eq!(node_as_it_stands(&pool, "Q2-B").await.1, "10.2.0.1");
        assert_ne!(folder_of(&pool, "Q2-A").await, Some(home));
        assert_ne!(folder_of(&pool, "Q2-B").await, Some(home));
    }

    /// An MX waiting for its network to be read keeps its slot under the cap: the access point behind
    /// it in name order does not take it, so the MX goes in on the sync that reads its network.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn an_mx_waiting_for_its_network_keeps_its_slot_under_the_cap(pool: sqlx::PgPool) {
        let device =
            |serial: &str, name: &str, product_type: &str, network: &str| MerakiInventoryDevice {
                info: MerakiDeviceInfo {
                    serial: serial.into(),
                    name: name.into(),
                    model: None,
                    product_type: product_type.into(),
                    network_id: network.into(),
                    lan_ip: Some("10.9.0.5".into()),
                },
                availability: UP,
            };
        let listing = MerakiInventory {
            networks: ["N_1", "N_2"]
                .iter()
                .map(|id| MerakiNetworkInfo {
                    id: (*id).into(),
                    name: format!("site {id}"),
                })
                .collect(),
            // Name order puts the MX first.
            devices: vec![
                device("Q2-A", "a-mx", "appliance", "N_1"),
                device("Q2-B", "b-ap", "wireless", "N_2"),
            ],
        };
        let r = rig(&pool, Ok(listing)).await;
        r.orgs
            .set_import_settings(r.org, true, true, 1)
            .await
            .expect("a cap of one");
        r.directory
            .lan_answer_for("N_1", Err(MerakiFetchError::Status(500)));

        let first = r
            .sync
            .sync_org_scheduled(&r.org().await)
            .await
            .expect("first sync");
        assert_eq!((first.imported, first.over_cap), (0, 1), "{first:?}");
        assert!(nodes_of(&pool, r.org).await.is_empty());

        r.directory
            .lan_answer_for("N_1", Ok(vec![vlan(10, "10.1.0.1")]));
        let second = r
            .sync
            .sync_org_scheduled(&r.org().await)
            .await
            .expect("second sync");
        assert_eq!((second.imported, second.over_cap), (1, 1), "{second:?}");
        assert_eq!(nodes_of(&pool, r.org).await, ["Q2-A"]);
    }

    /// `n` networks, each with one MX whose own VLAN is `10.<i>.0.1`, and a folder-less lab.
    fn sites(n: usize) -> MerakiInventory {
        MerakiInventory {
            networks: (0..n)
                .map(|i| MerakiNetworkInfo {
                    id: format!("N_{i:03}"),
                    name: format!("site {i:03}"),
                })
                .collect(),
            devices: (0..n)
                .map(|i| MerakiInventoryDevice {
                    info: MerakiDeviceInfo {
                        serial: format!("Q2-{i:03}"),
                        name: format!("mx-{i:03}"),
                        model: Some("MX67".into()),
                        product_type: "appliance".into(),
                        network_id: format!("N_{i:03}"),
                        lan_ip: None,
                    },
                    availability: UP,
                })
                .collect(),
        }
    }

    /// ADR-164 決定 30: an organization's first sync reads **every** network's LAN side — more than
    /// one chunk of them — and imports every MX in that same sync, reading at the organization's
    /// whole rate while it has no node. The next sync reads nothing new; "Sync now" then reads them
    /// all again, at one lane's rate now that there are nodes to collect.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn an_organizations_first_sync_reads_every_network_and_imports_every_mx(
        pool: sqlx::PgPool,
    ) {
        let n = LAN_READ_CHUNK * 2 + 10;
        let r = rig(&pool, Ok(sites(n))).await;
        for i in 0..n {
            r.directory.lan_answer_for(
                &format!("N_{i:03}"),
                Ok(vec![vlan(10, &format!("10.{i}.0.1"))]),
            );
        }

        let first = r
            .sync
            .sync_org_scheduled(&r.org().await)
            .await
            .expect("first sync");
        assert_eq!(
            r.directory.lan_asked().len(),
            n,
            "not every network was read"
        );
        assert_eq!(first.imported, count(n), "{first:?}");
        assert_eq!(node_as_it_stands(&pool, "Q2-007").await.1, "10.7.0.1");
        let rates = r.directory.lan_rps();
        assert_eq!(rates.len(), 3, "read in chunks of {LAN_READ_CHUNK}");
        assert!(
            rates.iter().all(|&rps| (rps - 2.0).abs() < f64::EPSILON),
            "an organization with no node read at one lane's rate: {rates:?}"
        );
        let org = r.org().await;
        assert_eq!((org.full_sync_requested_at, org.full_sync), (None, None));

        r.sync
            .sync_org_scheduled(&r.org().await)
            .await
            .expect("second sync");
        assert_eq!(
            r.directory.lan_asked().len(),
            n,
            "a fresh network was read again"
        );

        assert!(r.orgs.request_full_sync(r.org).await.expect("request"));
        r.sync
            .sync_org_requested(&r.org().await)
            .await
            .expect("Sync now");
        assert_eq!(r.directory.lan_asked().len(), 2 * n);
        assert!(
            r.directory.lan_rps()[3..]
                .iter()
                .all(|&rps| (rps - 1.0).abs() < f64::EPSILON),
            "an organization with nodes read at the whole rate, beside its collects"
        );
    }

    /// The lab case (2026-09-24), with the reads cut short where the old minute-a-sync round cut
    /// them. Two sites reuse `192.168.0.1`, and the only folder range in the lab holds it. The first
    /// sync reaches only the first site: its MX must not go in — the reused address is not known to
    /// be reused yet, and it would be filed into that folder for good — while the access point,
    /// addressed by its own `lanIp`, does (決定 31). The next sync reads the second site, and the MX
    /// goes in by its own address, into its network's folder.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn a_read_cut_short_imports_no_mx_and_the_reused_address_never_files_one(
        pool: sqlx::PgPool,
    ) {
        let home = pgtest::group(&pool, "home").await;
        pgtest::prefix(&pool, home, "192.168.0.0/24").await;
        let mut listing = sites(2);
        listing.devices.push(MerakiInventoryDevice {
            info: MerakiDeviceInfo {
                serial: "Q2-AP".into(),
                name: "ap-1".into(),
                model: Some("MR46".into()),
                product_type: "wireless".into(),
                network_id: "N_000".into(),
                lan_ip: Some("10.0.9.9".into()),
            },
            availability: UP,
        });
        let r = rig(&pool, Ok(listing)).await;
        r.directory.lan_answer_for(
            "N_000",
            Ok(vec![vlan(1, "192.168.0.1"), vlan(10, "10.0.0.1")]),
        );
        r.directory.lan_answer_for(
            "N_001",
            Ok(vec![vlan(1, "192.168.0.1"), vlan(10, "10.1.0.1")]),
        );
        r.directory.lan_reaches(Some(1));

        let cut = r
            .sync
            .sync_org_scheduled(&r.org().await)
            .await
            .expect("a cut read is still a successful sync");
        assert_eq!(r.directory.lan_asked(), ["N_000"]);
        assert_eq!(
            nodes_of(&pool, r.org).await,
            ["Q2-AP"],
            "an MX went in on a read cut short: {cut:?}"
        );

        r.directory.lan_reaches(None);
        r.sync
            .sync_org_scheduled(&r.org().await)
            .await
            .expect("the read goes on");
        assert_eq!(r.directory.lan_asked(), ["N_000", "N_001"]);
        assert_eq!(nodes_of(&pool, r.org).await, ["Q2-000", "Q2-001", "Q2-AP"]);
        assert_eq!(node_as_it_stands(&pool, "Q2-000").await.1, "10.0.0.1");
        assert_ne!(folder_of(&pool, "Q2-000").await, Some(home));
        assert_ne!(folder_of(&pool, "Q2-001").await, Some(home));
    }

    /// 決定 31's other half, and the answer to why increment 15 would not wait: a network whose
    /// **own** read fails was asked, so it holds up only its own MX — every other MX goes in.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn a_network_that_cannot_be_read_holds_up_only_its_own_mx(pool: sqlx::PgPool) {
        let r = rig(&pool, Ok(sites(3))).await;
        r.directory
            .lan_answer_for("N_000", Ok(vec![vlan(10, "10.0.0.1")]));
        r.directory
            .lan_answer_for("N_001", Err(MerakiFetchError::Status(500)));
        r.directory
            .lan_answer_for("N_002", Ok(vec![vlan(10, "10.2.0.1")]));

        let first = r
            .sync
            .sync_org_scheduled(&r.org().await)
            .await
            .expect("first sync");
        assert_eq!(first.imported, 2, "{first:?}");
        assert_eq!(nodes_of(&pool, r.org).await, ["Q2-000", "Q2-002"]);
    }

    /// The row's side of a whole-organization read: a request keeps its first time however often it
    /// is pressed; progress is written and read back; ending a read the loop started on its own
    /// leaves the request; a new leader clears progress and keeps the request.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn a_full_reads_request_and_progress_live_on_the_row(pool: sqlx::PgPool) {
        let r = rig(&pool, Ok(listing(&[("Q2-A", UP)]))).await;
        assert!(r.orgs.request_full_sync(r.org).await.expect("request"));
        let asked = r.org().await.full_sync_requested_at.expect("requested");
        assert!(r.orgs.request_full_sync(r.org).await.expect("again"));
        assert_eq!(
            r.org().await.full_sync_requested_at,
            Some(asked),
            "a second press queued a second read"
        );
        assert!(!r
            .orgs
            .request_full_sync(Uuid::new_v4())
            .await
            .expect("no such org"));

        r.orgs.start_full_sync(r.org, 350).await.expect("start");
        r.orgs
            .record_full_sync_read(r.org, 120)
            .await
            .expect("progress");
        let progress = r.org().await.full_sync.expect("reading");
        assert_eq!((progress.networks, progress.read), (350, 120));

        r.orgs.finish_full_sync(r.org, false).await.expect("finish");
        let org = r.org().await;
        assert_eq!(org.full_sync, None);
        assert_eq!(org.full_sync_requested_at, Some(asked));

        r.orgs.start_full_sync(r.org, 350).await.expect("start");
        assert_eq!(r.orgs.clear_full_sync_progress().await.expect("clear"), 1);
        let org = r.org().await;
        assert_eq!(org.full_sync, None);
        assert_eq!(
            org.full_sync_requested_at,
            Some(asked),
            "a new leader dropped the request"
        );
        assert_eq!(r.orgs.clear_full_sync_progress().await.expect("clear"), 0);
    }

    /// The loop's side of "one sync per organization, side by side": both start without waiting for
    /// each other, a sync still running is not reaped, and each ending reaches the schedule — a
    /// failure backs off an interval, a busy lane is asked again next tick.
    #[tokio::test]
    async fn syncs_run_side_by_side_one_per_organization() {
        let mut failing = org_with(None, 300);
        failing.id = Uuid::from_u128(1);
        let mut busy = org_with(None, 300);
        busy.id = Uuid::from_u128(2);
        let gate = Arc::new(tokio::sync::Semaphore::new(0));
        let mut running = RunningSyncs::default();
        for (org, outcome) in [
            (
                failing.id,
                Err(SyncError::Failed(MerakiSyncFailure::Upstream)),
            ),
            (busy.id, Err(SyncError::Busy)),
        ] {
            let gate = gate.clone();
            running.spawn(org, async move {
                gate.acquire().await.expect("gate").forget();
                outcome
            });
        }
        assert!(running.contains(failing.id) && running.contains(busy.id));

        let mut schedule = SyncSchedule::default();
        let now = Instant::now();
        running.reap(&mut schedule, now);
        assert!(
            running.contains(failing.id),
            "a sync still running was reaped"
        );

        gate.add_permits(2);
        tokio::time::timeout(Duration::from_secs(5), async {
            while running.contains(failing.id) || running.contains(busy.id) {
                tokio::time::sleep(Duration::from_millis(10)).await;
                running.reap(&mut schedule, now);
            }
        })
        .await
        .expect("both syncs ended");
        let t0 = DateTime::from_timestamp(1_800_000_000, 0).expect("in range");
        assert!(
            !schedule.is_due(&failing, t0, now + Duration::from_secs(299)),
            "a failed sync did not back off"
        );
        assert!(
            schedule.is_due(&busy, t0, now),
            "a busy lane was taken for a failure"
        );
    }

    /// 決定 30: a whole-organization read is given what its networks need — two requests each, at
    /// its pace but never under half a second — up to the ceiling; re-reads alone get a minute.
    #[test]
    fn a_whole_read_is_given_what_its_networks_need_up_to_the_ceiling() {
        assert_eq!(lan_budget(3, 1.0, false), LAN_READ_BUDGET);
        assert_eq!(lan_budget(350, 1.0, true), Duration::from_secs(760));
        assert_eq!(lan_budget(350, 2.0, true), Duration::from_secs(410));
        assert_eq!(
            lan_budget(350, 10.0, true),
            Duration::from_secs(410),
            "a request was budgeted under half a second"
        );
        assert_eq!(lan_budget(5_000, 1.0, true), FULL_READ_CEILING);
    }
}
