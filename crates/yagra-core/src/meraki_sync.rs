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
//! **What it shares with the collector is the organization's single flight** ([`MerakiInflight`]).
//! The Dashboard API's rate limit is per organization, so a sync and a collect of the same
//! organization never run at once: whichever asks second waits a tick. The flight is released by a
//! drop guard, because a sync that panicked or was cancelled while holding it would otherwise stop
//! that organization's collection until the lease ran out.
//!
//! **What it must never do is conclude from a short answer.** A device the listing does not contain
//! is marked missing, so the listing has to be complete: [`MerakiDirectory::inventory`] is backed by
//! `yagra_transport::fetch_inventory`, which returns an error rather than a partial result, and a
//! failed sync writes its reason and nothing else — not one row of `meraki_inventory`, and not
//! `last_sync_at`.
//!
//! This increment observes only. No node is created here yet (ADR-164 Inc.4).

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use uuid::Uuid;
use yagra_transport::{MerakiFetchError, MerakiInventory};

use crate::meraki::{resolve_meraki_key, MerakiInflight, MerakiOrg, MerakiOrgRepo};
use crate::meraki_inventory::{plan_sync, seen_devices, MerakiInventoryRepo};
use crate::repo::NodeRepo;
use crate::secrets::CredentialStore;

/// How often the loop looks for an organization that is due.
const TICK: Duration = Duration::from_secs(15);
/// One Dashboard request's timeout.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
/// The whole sync's timeout. Three listings at two requests a second is a few seconds for any
/// organization under a few thousand devices; this is the ceiling for one that is not, and it is
/// what bounds how long a sync can keep the organization's collection waiting.
const SYNC_TIMEOUT: Duration = Duration::from_secs(120);
/// The flight's lease — the backstop if the drop guard never runs (the process is killed). Longer
/// than [`SYNC_TIMEOUT`] so a sync that is still running is never treated as abandoned.
const LEASE: Duration = Duration::from_secs(150);
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
}

impl MerakiSyncFailure {
    /// Every reason, for the tests that pin the token, the serde tag and the locale keys together.
    pub const ALL: [Self; 10] = [
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

/// Where a sync gets an organization's inventory. The seam a test replaces; production is
/// [`DashboardApi`].
///
/// 🚨 The contract is the transport's: **a complete listing or an error.** A fake that returns a
/// short `Ok` is modelling a bug the real one cannot have.
#[async_trait]
pub trait MerakiDirectory: Send + Sync {
    /// Read `org`'s networks, devices and availabilities, to the end.
    async fn inventory(
        &self,
        org: &MerakiOrg,
        api_key: &str,
    ) -> Result<MerakiInventory, MerakiFetchError>;
}

/// The real Dashboard API, through `yagra-transport` (GET only, host allow-listed, paced).
pub struct DashboardApi;

#[async_trait]
impl MerakiDirectory for DashboardApi {
    async fn inventory(
        &self,
        org: &MerakiOrg,
        api_key: &str,
    ) -> Result<MerakiInventory, MerakiFetchError> {
        yagra_transport::fetch_inventory(
            &org.base_url,
            api_key,
            &org.org_id,
            org.target_rps,
            REQUEST_TIMEOUT,
        )
        .await
    }
}

/// What one successful sync found and did.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, utoipa::ToSchema)]
pub struct MerakiSyncReport {
    /// Devices the Dashboard lists.
    pub devices: u32,
    /// Networks the Dashboard lists.
    pub networks: u32,
    /// Inventory and network rows written. Zero is the ordinary answer.
    pub written: u32,
    /// Devices that were listed last time and are not now.
    pub newly_missing: u32,
}

/// Why a sync did not produce a report.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncError {
    /// A collect, or another sync, holds the organization's flight. Nothing was attempted and
    /// nothing was recorded.
    Busy,
    /// The sync ran and failed; the reason is on the organization's row.
    Failed(MerakiSyncFailure),
}

/// Releases an organization's flight when the sync ends — however it ends.
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
}

impl MerakiSync {
    #[must_use]
    pub fn new(
        orgs: Arc<MerakiOrgRepo>,
        inventory: Arc<MerakiInventoryRepo>,
        creds: Arc<CredentialStore>,
        directory: Arc<dyn MerakiDirectory>,
        inflight: Arc<MerakiInflight>,
    ) -> Self {
        Self {
            orgs,
            inventory,
            creds,
            directory,
            inflight,
        }
    }

    /// Sync one organization now, and record how it went on its row.
    pub async fn sync_org(&self, org: &MerakiOrg) -> Result<MerakiSyncReport, SyncError> {
        let job = Uuid::new_v4();
        if !self.inflight.acquire(org.id, job, LEASE, Instant::now()) {
            metrics::counter!("yagra_meraki_syncs_total", "outcome" => "busy").increment(1);
            return Err(SyncError::Busy);
        }
        let _flight = Flight {
            inflight: &self.inflight,
            job,
        };

        match self.attempt(org).await {
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

    /// The sync itself. Reads first, writes last: every `?` above the two writes leaves the
    /// database exactly as it was.
    async fn attempt(&self, org: &MerakiOrg) -> Result<MerakiSyncReport, MerakiSyncFailure> {
        let api_key = resolve_meraki_key(&self.creds, org.credential_id)
            .await
            .ok_or(MerakiSyncFailure::Credential)?;
        let listing = tokio::time::timeout(SYNC_TIMEOUT, self.directory.inventory(org, &api_key))
            .await
            .map_err(|_| MerakiSyncFailure::Timeout)??;

        let internal = |what: &'static str| {
            move |e: anyhow::Error| {
                tracing::warn!(error = %e, "meraki sync: {what}");
                MerakiSyncFailure::Internal
            }
        };
        let seen = seen_devices(&listing);
        let stored = self
            .inventory
            .stored(org.id)
            .await
            .map_err(internal("reading the inventory failed"))?;
        let bound = self
            .inventory
            .bound(org.id)
            .await
            .map_err(internal("reading the device bindings failed"))?;
        let plan = plan_sync(&stored, &seen, &bound);

        let networks: Vec<(String, String)> = listing
            .networks
            .iter()
            .map(|n| (n.id.clone(), n.name.clone()))
            .collect();
        let network_rows = self
            .orgs
            .record_networks(org.id, &networks)
            .await
            .map_err(internal("recording the networks failed"))?;
        let device_rows = self
            .inventory
            .apply(org.id, &plan)
            .await
            .map_err(internal("writing the inventory failed"))?;

        Ok(MerakiSyncReport {
            devices: count(seen.len()),
            networks: count(listing.networks.len()),
            written: u32::try_from(network_rows + device_rows).unwrap_or(u32::MAX),
            newly_missing: count(plan.newly_missing.len()),
        })
    }
}

fn count(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
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

/// The leader-only periodic sync.
///
/// ⚠️ **Spawned from `LeaderTasks`, never from `run_live`** (ADR-090). Leader-gated for a stronger
/// reason than NetBox's loop: the single flight lives in this process, so two cores syncing would
/// each believe they held an organization alone.
///
/// Honours the Meraki kill switch — that switch exists to give the Dashboard API budget back at
/// once, and a sync spends it like a collect does. Organizations are synced one after another; a
/// sync is a few seconds, and running them side by side would buy nothing the per-organization
/// rate limit cares about.
pub async fn run_sync_loop(sync: Arc<MerakiSync>, settings: Arc<NodeRepo>) {
    let mut tick = tokio::time::interval(TICK);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut schedule = SyncSchedule::default();
    loop {
        tick.tick().await;
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
        for org in &orgs {
            if !schedule.is_due(org, Utc::now(), Instant::now()) {
                continue;
            }
            match sync.sync_org(org).await {
                Ok(_) => schedule.succeeded(org.id),
                // A collect holds the organization. Not a failure: ask again next tick.
                Err(SyncError::Busy) => {}
                Err(SyncError::Failed(_)) => schedule.failed(org.id, Instant::now()),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pgtest;
    use std::sync::Mutex;
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
            enabled_tiers: Vec::new(),
            target_rps: 2.0,
            group_id: None,
            enabled: true,
            last_sync_at,
            last_sync_ok: None,
            last_sync_error: None,
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
    }

    impl FakeDirectory {
        fn answering(answer: Result<MerakiInventory, MerakiFetchError>) -> Arc<Self> {
            Arc::new(Self {
                answer: Mutex::new(answer),
                asked: Mutex::new(0),
            })
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
        async fn inventory(
            &self,
            _org: &MerakiOrg,
            _api_key: &str,
        ) -> Result<MerakiInventory, MerakiFetchError> {
            *self.asked.lock().expect("asked") += 1;
            self.answer.lock().expect("answer").clone()
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
        let sync = MerakiSync::new(
            orgs.clone(),
            inventory.clone(),
            creds,
            directory.clone(),
            inflight.clone(),
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

        let first = r.sync.sync_org(&r.org().await).await.expect("first sync");
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
        let again = r.sync.sync_org(&r.org().await).await.expect("second sync");
        assert_eq!(again.written, 0, "an unchanged listing rewrote rows");

        // Q2-A leaves the listing; Q2-B comes online for the first time.
        r.directory.now_answers(Ok(listing(&[("Q2-B", UP)])));
        let third = r.sync.sync_org(&r.org().await).await.expect("third sync");
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
        r.sync.sync_org(&r.org().await).await.expect("fourth sync");
        let stored = r.inventory.stored(r.org).await.expect("stored");
        let a = stored.iter().find(|d| d.serial == "Q2-A").expect("Q2-A");
        assert_eq!(a.missing_since, marked, "missing_since was restamped");

        // Back again, offline: the mark clears and first_online_at is not restamped.
        r.directory
            .now_answers(Ok(listing(&[("Q2-A", DOWN), ("Q2-B", UP)])));
        r.sync.sync_org(&r.org().await).await.expect("fifth sync");
        let stored = r.inventory.stored(r.org).await.expect("stored");
        let a = stored.iter().find(|d| d.serial == "Q2-A").expect("Q2-A");
        assert_eq!(a.missing_since, None);
        assert_eq!(a.first_online_at, first_online, "first_online_at moved");
    }

    /// ADR-164 決定 3, the one that matters most: a sync that fails changes **nothing** it could be
    /// wrong about. Not a row of the inventory, and not the stamp the next sync is timed from.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn a_failed_sync_marks_nothing_missing_and_does_not_move_the_stamp(pool: sqlx::PgPool) {
        let r = rig(&pool, Ok(listing(&[("Q2-A", UP), ("Q2-B", UP)]))).await;
        r.sync.sync_org(&r.org().await).await.expect("first sync");
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
                r.sync.sync_org(&r.org().await).await,
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
        r.sync.sync_org(&r.org().await).await.expect("recovered");
        let org = r.org().await;
        assert_eq!((org.last_sync_ok, org.last_sync_error), (Some(true), None));
    }

    /// The single flight, in both directions: a collect in flight refuses the sync without touching
    /// the Dashboard or the row, and a finished sync leaves the organization free for the collector.
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn a_sync_and_a_collect_never_hold_one_organization_together(pool: sqlx::PgPool) {
        let r = rig(&pool, Ok(listing(&[("Q2-A", UP)]))).await;
        let collect = Uuid::new_v4();
        assert!(r
            .inflight
            .acquire(r.org, collect, Duration::from_secs(300), Instant::now()));

        assert_eq!(r.sync.sync_org(&r.org().await).await, Err(SyncError::Busy));
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

        r.inflight.complete(collect);
        r.sync.sync_org(&r.org().await).await.expect("sync");
        assert!(
            !r.inflight.is_inflight(r.org, Instant::now()),
            "a finished sync kept the organization's flight"
        );

        // …and a failed one releases it too.
        r.directory.now_answers(Err(MerakiFetchError::Network));
        let _ = r.sync.sync_org(&r.org().await).await;
        assert!(!r.inflight.is_inflight(r.org, Instant::now()));
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
            r.sync.sync_org(&r.org().await).await,
            Err(SyncError::Failed(MerakiSyncFailure::Credential))
        );
        assert_eq!(r.directory.asked(), 0);
        assert_eq!(r.org().await.last_sync_error.as_deref(), Some("credential"));
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
                orgs.update_cadence(id, 300, 300, 1800, secs, &[], 2.0)
                    .await
            }
        };
        assert!(set(60).await.expect("one minute is allowed"));
        assert!(set(59).await.is_err(), "the CHECK let 59 seconds through");
    }
}
