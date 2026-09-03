// SPDX-License-Identifier: AGPL-3.0-only
//! NetBox integration: pull the site hierarchy out of the network's source of truth and mirror it
//! into `node_groups` as a folder tree (ADR-100 Inc.1).
//!
//! # What this module is for
//!
//! The site master already exists in NetBox; Yagra was making operators rebuild it by hand. Because
//! Yagra's folders carry threshold inheritance (ADR-013), dependency suppression, group scope
//! (ADR-014), folder pool (mig 0054) and the Geo-map pins (mig 0025), a tree that drifts is
//! *monitoring* that drifts. So this module reads NetBox and writes folders — and nothing else.
//!
//! # The one idea worth carrying out of here: ownership is split per column
//!
//! | column | owner | what a sync does |
//! |---|---|---|
//! | `node_groups.name` / `parent_id` / `latitude` / `longitude` | NetBox | overwrite every time |
//! | `node_groups.pool` | the operator | **never touched** |
//! | `node_groups.sort_order` | Yagra | kept name-ordered so the tree does not shuffle |
//! | a row that vanished from NetBox | nobody | **marked, never deleted** |
//!
//! Splitting it that way is what makes the sync a plain idempotent upsert: there is no conflict, so
//! there is no conflict-resolution policy to design, tune or get wrong. The inverse — letting the
//! sync own `nodes.group_id` — would put the operator's manual moves and the sync period into a
//! last-writer-wins fight on every cycle. That is why node placement is Inc.2's *suggestion* list
//! and not a write from here.
//!
//! # Read-only, and outbound from core
//!
//! Yagra never writes to NetBox. Nothing here issues anything but `GET`, and
//! [`the_client_only_ever_issues_reads`] pins that. The egress is from **core**, on a leader-only
//! low-frequency task — not from a poller, because reaching NetBox through a poller would mean
//! putting the token on the bus and widening what ADR-030 has to scope down.
//!
//! ⚠️ **A NetBox reachable only from inside a remote site is out of scope**, and deliberately: the
//! alternative is the bus-borne credential above.
//!
//! # Landmines
//!
//! - 🚨 **Regions are a recursive tree, not one layer.** `dcim.Region` has a `parent` and a
//!   `_depth`; the lab has `Japan → Ehime → …`. ADR-100 said "Region → Site" as if it were two
//!   levels and that was wrong (corrected 2026-09-03, on real hardware). Upsert in `_depth` order
//!   or `node_groups.parent_id`'s foreign key rejects the child.
//! - 🚨 **Never request `?brief=1`.** The brief serializer drops `parent` — and keeps `_depth`, so
//!   the ordering still looks right while every folder lands at the root.
//! - 🚨 **`last_sync_at` advances only on a fully successful enumeration.** The "missing from
//!   NetBox" mark is derived by comparing it against `netbox_groups.last_seen_at`, so recording a
//!   half-finished sync as a success marks the whole tree as deleted. Same shape as ADR-080's rule
//!   that a failed read must never become an empty one.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use serde::Deserialize;
use sqlx::{PgPool, Row};
use uuid::Uuid;

use crate::groups::GroupType;
use crate::secrets::{CredentialStore, NetboxTokenSecret, KIND_NETBOX_TOKEN};

/// Fixed namespace for deriving stable folder ids via UUIDv5, so re-syncing never duplicates the
/// tree and an `ON CONFLICT (id) DO UPDATE` is all the write needs. Same device as
/// `meraki::MERAKI_GROUP_NS`, and specifically **not** the seed scheme in `repo/seed.rs`, whose
/// keys are array positions (`extensibility.md` §6).
const NETBOX_GROUP_NS: Uuid = Uuid::from_u128(0x6e65_7462_6f78_0000_0000_0000_0000_0001);

/// Page size asked of NetBox's `limit`/`offset` paginator. NetBox caps this itself
/// (`MAX_PAGE_SIZE`), so a server configured lower simply returns fewer and the loop follows
/// `next` regardless — the constant is a request, never an assumption.
const PAGE_LIMIT: u32 = 250;

/// Hard cap on pages walked for one collection, so a server whose `next` never terminates cannot
/// spin the leader task forever. 250 × 400 = 100,000 objects, far past "sites are a few hundred".
const MAX_PAGES: u32 = 400;

/// Per-request timeout. A sync is a background job on an hour cadence, so this is generous
/// on purpose — the failure we care about is "unreachable", not "slow".
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// Floor on a *deliberately* short sync period, so an operator cannot set a cadence that hammers
/// someone else's NetBox. Site masters do not change by the second.
const MIN_SYNC_INTERVAL: Duration = Duration::from_secs(60);

/// Cadence used when a stored interval is corrupt (non-positive).
///
/// ⚠️ Mirrors `netbox_servers.sync_interval_secs`'s `DEFAULT` in migration 0102. Two copies,
/// because an applied migration is immutable and cannot import a constant — if this changes, the
/// migration keeps its value and only new rows differ, which is the intended behaviour but is
/// worth knowing before "fixing" the drift.
const DEFAULT_SYNC_INTERVAL_SECS: u64 = 3600;

/// How often the loop wakes to see whether any server is due.
const TICK: Duration = Duration::from_secs(30);

/// The deterministic folder id for a NetBox Region.
#[must_use]
pub fn region_group_id(server_id: Uuid, netbox_id: i64) -> Uuid {
    Uuid::new_v5(
        &NETBOX_GROUP_NS,
        format!("{server_id}:region:{netbox_id}").as_bytes(),
    )
}

/// The deterministic folder id for a NetBox Site.
#[must_use]
pub fn site_group_id(server_id: Uuid, netbox_id: i64) -> Uuid {
    Uuid::new_v5(
        &NETBOX_GROUP_NS,
        format!("{server_id}:site:{netbox_id}").as_bytes(),
    )
}

/// Which NetBox object a `netbox_groups` row mirrors.
///
/// Stored as text with no DB `CHECK`, matching `LinkSource`'s reasoning: the Rust enum is the
/// guard, so an older core meeting a token it does not know skips that row instead of failing the
/// whole query.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObjectKind {
    Region,
    Site,
}

impl ObjectKind {
    /// Every kind, so the round-trip below can be asserted over the set rather than over two
    /// literals.
    ///
    /// ⚠️ Test-only, and so is [`Self::from_str`], because **nothing reads `object_kind` back yet**
    /// — Inc.1 writes it as half of `netbox_groups`' primary key (region 6 and site 6 are different
    /// objects) and never selects on it. The pair is kept rather than deleted because the token is
    /// already in a shipped table: what it must round-trip to is a fact about stored data, not
    /// about a caller, and the first reader arrives with Inc.2's folder list.
    #[cfg(test)]
    pub const ALL: [ObjectKind; 2] = [ObjectKind::Region, ObjectKind::Site];

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            ObjectKind::Region => "region",
            ObjectKind::Site => "site",
        }
    }

    /// The stored token back to a kind, or `None` for one this build does not know.
    ///
    /// The `None` is the `LinkSource` rule: an older core meeting an unknown token must skip that
    /// row rather than fail the query it appeared in.
    #[cfg(test)]
    #[must_use]
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "region" => Some(ObjectKind::Region),
            "site" => Some(ObjectKind::Site),
            _ => None,
        }
    }

    /// The folder type a NetBox object of this kind becomes. `GroupType` already had both, which
    /// is why this integration needs no new vocabulary anywhere (ADR-100 decision 4's ⭐).
    #[must_use]
    pub const fn group_type(self) -> GroupType {
        match self {
            ObjectKind::Region => GroupType::Region,
            ObjectKind::Site => GroupType::Site,
        }
    }

    /// The deterministic folder id for one object of this kind.
    #[must_use]
    pub fn group_id(self, server_id: Uuid, netbox_id: i64) -> Uuid {
        match self {
            ObjectKind::Region => region_group_id(server_id, netbox_id),
            ObjectKind::Site => site_group_id(server_id, netbox_id),
        }
    }
}

// ---------------------------------------------------------------------------------------------
// base URL validation (SSRF)
// ---------------------------------------------------------------------------------------------

/// Why a base URL was refused. Carries no user input — the caller has the string already, and an
/// error that echoes it back into a log is how a URL ends up somewhere it was not meant to be.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BaseUrlError {
    /// Not parseable as an absolute URL.
    Malformed,
    /// Scheme other than `http` / `https`.
    Scheme,
    /// No host component at all (`file:///…`, `http:///x`).
    NoHost,
    /// An IP literal that is loopback / link-local / multicast / unspecified.
    Blocked,
}

impl BaseUrlError {
    /// A stable machine code for the API edge.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            BaseUrlError::Malformed => "invalid_base_url",
            BaseUrlError::Scheme => "invalid_base_url_scheme",
            BaseUrlError::NoHost => "invalid_base_url_host",
            BaseUrlError::Blocked => "base_url_blocked",
        }
    }

    /// Operator-facing text. Says which rule was broken, because "invalid URL" sends someone
    /// looking at their typing when the answer is "that address is refused on purpose".
    #[must_use]
    pub const fn message(self) -> &'static str {
        match self {
            BaseUrlError::Malformed => "base_url is not a valid absolute URL",
            BaseUrlError::Scheme => "base_url must be http or https",
            BaseUrlError::NoHost => "base_url has no host",
            BaseUrlError::Blocked => {
                "base_url resolves to a loopback, link-local, multicast or unspecified address"
            }
        }
    }
}

/// Validate and normalize an operator-entered NetBox base URL.
///
/// **This is the only implementation**, called by the API edge and again when a client is built,
/// so the check cannot be half-applied (`extensibility.md` §3 — the URL host-parsing bug this repo
/// already paid for was exactly a fourth copy that looked like the other three).
///
/// The policy is ADR-047's, unchanged: an NMS legitimately reaches **private** addresses, so
/// RFC1918 / ULA are allowed and only the SSRF-escalation surface is refused. That is why
/// `notifications.rs::validate_vendor_url`'s host allow-list cannot be reused — there is no list of
/// legitimate NetBox hosts to write down.
///
/// ⚠️ **A hostname is not resolved.** `netbox.internal` pointing at `127.0.0.1` passes here. That
/// is the same gap ADR-047's URL monitoring has, named in ADR-100 rather than quietly inherited;
/// closing it needs a layer that inspects the resolved peer, shared with URL monitoring.
pub fn validate_base_url(raw: &str) -> Result<String, BaseUrlError> {
    // `reqwest::Url` is `url::Url` re-exported, and it is how the rest of this crate spells it
    // (`api/checks.rs`, `alerts/notify.rs`) — so no second URL crate enters the tree.
    let url = reqwest::Url::parse(raw.trim()).map_err(|_| BaseUrlError::Malformed)?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(BaseUrlError::Scheme);
    }
    let host = url.host_str().ok_or(BaseUrlError::NoHost)?;
    if host.is_empty() {
        return Err(BaseUrlError::NoHost);
    }
    // ⚠️ `host_ip`, never `host.parse()`. `Url::host_str` hands back an IPv6 literal *bracketed*
    // (`"[::1]"`), which `IpAddr::from_str` rejects — so the naive spelling reads every IPv6
    // address as a hostname and skips this check entirely. That is a shipped defect this
    // workspace has already had once (`extensibility.md` §3).
    if let Some(ip) = yagra_common::url_check::host_ip(host) {
        if yagra_common::url_check::is_ssrf_blocked(ip) {
            return Err(BaseUrlError::Blocked);
        }
    }
    // Normalize to "scheme://host[:port]" with no path, so joining an API path later cannot
    // double a slash or inherit a stray query the operator pasted.
    let mut base = format!("{}://{}", url.scheme(), host);
    if let Some(port) = url.port() {
        base.push_str(&format!(":{port}"));
    }
    Ok(base)
}

/// Validate a pasted CA certificate before it is stored (ADR-100 decision 8).
///
/// Checked at the edge and not at sync time on purpose: a PEM that only fails an hour later, in a
/// background task, puts the cause a long way from the paste that caused it.
///
/// 🚨 **Neither `Certificate::from_pem` nor building a client is a validation**, and both were
/// tried here before this version. `from_pem` accepts *any* bytes (`b"not pem at all"` returns
/// `Ok`) because the rustls backend defers parsing; and `ClientBuilder::build()` then succeeds
/// too, because a PEM holding **zero** certificate blocks simply adds zero roots. So the obvious
/// spellings both accept an empty paste and fail an hour later, inside a background sync, as an
/// unexplained TLS error — a validation that validates nothing
/// (`graceful-degradation-ships-inert-features`). The check that works is to **parse the blocks
/// and count them**, which is what `server_cert::validate` already does for the inbound
/// certificate; this reuses its rule rather than adding a third.
pub fn validate_ca_pem(pem: &str) -> Result<(), &'static str> {
    use rustls::pki_types::{pem::PemObject, CertificateDer};

    if pem.trim().is_empty() {
        return Err("ca_cert_pem must not be empty when present");
    }
    // A private key pasted into a certificate field would land in a plaintext column the API
    // returns. Refusing is the only safe answer — silently stripping it leaves the operator
    // believing a key they exposed is still private. Same rule, same helper, as ADR-044's.
    if crate::server_cert::contains_private_key_block(pem) {
        return Err("ca_cert_pem contains a private key block; paste only the certificate");
    }
    let mut n = 0usize;
    for der in CertificateDer::pem_slice_iter(pem.as_bytes()) {
        let der = der.map_err(|_| "ca_cert_pem is not a valid PEM certificate")?;
        // ⚠️ And `CertificateDer` is still only a byte wrapper — it de-armours the base64 and
        // stops. `-----BEGIN CERTIFICATE-----\nnope\n-----END CERTIFICATE-----` survives all of
        // the above. The X.509 parse is the first step that actually reads the contents, which is
        // why this reaches for `x509-parser` (already a direct dependency, for ADR-044).
        x509_parser::parse_x509_certificate(&der)
            .map_err(|_| "ca_cert_pem is not a valid X.509 certificate")?;
        n += 1;
    }
    if n == 0 {
        return Err("ca_cert_pem contains no certificate");
    }
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// the HTTP client
// ---------------------------------------------------------------------------------------------

/// A NetBox deployment's REST surface, read-only.
///
/// Built per sync rather than held: a server's base URL, token or CA can change between runs, and
/// a cached client would keep using the old one until a restart.
pub struct NetboxClient {
    http: reqwest::Client,
    base: String,
    token: String,
}

/// One page of a NetBox list endpoint. NetBox's paginator is `limit`/`offset` and always reports
/// `count`, so the loop can follow `next` and still know when it is done.
#[derive(Debug, Deserialize)]
struct Page<T> {
    #[allow(dead_code)] // Part of the documented wire shape; kept so the struct reads as the API.
    count: i64,
    next: Option<String>,
    results: Vec<T>,
}

/// A `dcim.Region`. Only the fields the folder tree needs.
#[derive(Debug, Clone, Deserialize)]
pub struct NetboxRegion {
    pub id: i64,
    pub name: String,
    /// Present because the listing is fetched **without** `brief=1`. 🚨 With `brief=1` this field
    /// is absent and every region silently becomes a root.
    #[serde(default)]
    pub parent: Option<NestedRef>,
    /// Depth from the root, supplied by NetBox's MPTT tree. Used to order the upsert so a parent
    /// always exists before its child.
    #[serde(default, rename = "_depth")]
    pub depth: i32,
}

/// A `dcim.Site`.
#[derive(Debug, Clone, Deserialize)]
pub struct NetboxSite {
    pub id: i64,
    pub name: String,
    #[serde(default)]
    pub region: Option<NestedRef>,
    #[serde(default)]
    pub latitude: Option<f64>,
    #[serde(default)]
    pub longitude: Option<f64>,
}

/// NetBox's nested representation of a foreign key — always at least `{id, name}`.
#[derive(Debug, Clone, Deserialize)]
pub struct NestedRef {
    pub id: i64,
}

/// What `/api/status/` answers.
#[derive(Debug, Clone, Deserialize)]
struct StatusDoc {
    #[serde(rename = "netbox-version")]
    netbox_version: Option<String>,
}

/// The outcome of a connection test, which deliberately distinguishes its two failure modes.
#[derive(Debug, Clone)]
pub struct ProbeResult {
    /// The `netbox-version` from `/api/status/` once the token was accepted.
    pub netbox_version: Option<String>,
    /// The `API-Version` header, which NetBox sends on **every** API response — including the
    /// unauthenticated 403. So this is populated even when the token is wrong, and that is the
    /// whole point: it separates "this is not a NetBox / the URL is wrong" from "the token is
    /// wrong", which a single 403 cannot.
    pub api_version: Option<String>,
    /// `false` when the endpoint answered as NetBox but refused the token.
    pub authenticated: bool,
}

impl NetboxClient {
    /// Build a client for one server. `ca_pem`, when present, is trusted **by this client only** —
    /// the process trust store is not modified, and certificate verification is never disabled
    /// (`security.md`; there is deliberately no `danger_accept_invalid_certs` anywhere here).
    pub fn new(base_url: &str, token: &str, ca_pem: Option<&str>) -> anyhow::Result<Self> {
        let base = validate_base_url(base_url)
            .map_err(|e| anyhow::anyhow!("netbox base_url refused: {}", e.message()))?;
        let mut builder = reqwest::Client::builder()
            .timeout(REQUEST_TIMEOUT)
            // No redirect following: a 302 is the classic way an allowed base URL becomes a
            // request somewhere else, and `validate_base_url` only ever saw the first hop.
            .redirect(reqwest::redirect::Policy::none())
            .user_agent("Yagra-core");
        if let Some(pem) = ca_pem {
            let cert = reqwest::Certificate::from_pem(pem.as_bytes())
                .map_err(|_| anyhow::anyhow!("ca_cert_pem is not a valid PEM certificate"))?;
            builder = builder.add_root_certificate(cert);
        }
        Ok(Self {
            http: builder.build()?,
            base,
            token: token.to_owned(),
        })
    }

    /// `GET {base}{path}` with the token attached. The only request builder in this module, so
    /// "read-only" is one line rather than a convention.
    fn get(&self, path_and_query: &str) -> reqwest::RequestBuilder {
        self.http
            .get(format!("{}{}", self.base, path_and_query))
            // ⚠️ NetBox's scheme is `Token <t>`, not `Bearer <t>`.
            .header(
                reqwest::header::AUTHORIZATION,
                format!("Token {}", self.token),
            )
            .header(reqwest::header::ACCEPT, "application/json")
    }

    /// Connection test. Answers the operator's two separate questions in one round trip's worth of
    /// vocabulary — see [`ProbeResult`].
    pub async fn probe(&self) -> anyhow::Result<ProbeResult> {
        let resp = self.get("/api/status/").send().await?;
        let api_version = resp
            .headers()
            .get("api-version")
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned);
        if resp.status() == reqwest::StatusCode::UNAUTHORIZED
            || resp.status() == reqwest::StatusCode::FORBIDDEN
        {
            return Ok(ProbeResult {
                netbox_version: None,
                api_version,
                authenticated: false,
            });
        }
        let resp = resp.error_for_status()?;
        let doc: StatusDoc = resp.json().await?;
        Ok(ProbeResult {
            netbox_version: doc.netbox_version,
            api_version,
            authenticated: true,
        })
    }

    /// Walk every page of a list endpoint.
    ///
    /// 🚨 `path` must not carry `brief=1`; see the module header. The loop follows `next` rather
    /// than computing offsets, because NetBox already encodes its own paging state there and a
    /// second implementation of the arithmetic is a second thing to get wrong.
    async fn fetch_all<T: serde::de::DeserializeOwned>(
        &self,
        path: &str,
    ) -> anyhow::Result<Vec<T>> {
        let mut out = Vec::new();
        let mut next = Some(format!("{path}?limit={PAGE_LIMIT}"));
        let mut pages = 0u32;
        while let Some(target) = next {
            pages += 1;
            if pages > MAX_PAGES {
                anyhow::bail!("netbox listing {path} exceeded {MAX_PAGES} pages");
            }
            // `next` comes back as an absolute URL built from NetBox's own configured host, which
            // may not be the address we reached it on (a reverse proxy, a container hostname). So
            // only the path+query is taken from it and re-joined to the base we validated —
            // otherwise a misconfigured NetBox redirects the sync onto an unvalidated host.
            let rel = if target.starts_with('/') {
                target
            } else {
                let parsed = reqwest::Url::parse(&target)
                    .map_err(|_| anyhow::anyhow!("netbox returned an unparseable next page"))?;
                match parsed.query() {
                    Some(q) => format!("{}?{}", parsed.path(), q),
                    None => parsed.path().to_owned(),
                }
            };
            let page: Page<T> = self
                .get(&rel)
                .send()
                .await?
                .error_for_status()?
                .json()
                .await?;
            out.extend(page.results);
            next = page.next;
        }
        Ok(out)
    }

    /// Every region, ordered shallowest-first so a parent is always written before its child.
    pub async fn regions(&self) -> anyhow::Result<Vec<NetboxRegion>> {
        let mut rows: Vec<NetboxRegion> = self.fetch_all("/api/dcim/regions/").await?;
        rows.sort_by(|a, b| a.depth.cmp(&b.depth).then_with(|| a.id.cmp(&b.id)));
        Ok(rows)
    }

    /// Every site.
    pub async fn sites(&self) -> anyhow::Result<Vec<NetboxSite>> {
        self.fetch_all("/api/dcim/sites/").await
    }
}

// ---------------------------------------------------------------------------------------------
// persistence
// ---------------------------------------------------------------------------------------------

/// A configured NetBox deployment.
#[derive(Debug, Clone)]
pub struct NetboxServer {
    pub id: Uuid,
    pub name: String,
    pub base_url: String,
    pub credential_id: Uuid,
    pub ca_cert_pem: Option<String>,
    pub enabled: bool,
    pub sync_interval_secs: i32,
    pub api_version: Option<String>,
    pub last_sync_at: Option<chrono::DateTime<chrono::Utc>>,
    pub last_sync_ok: Option<bool>,
    pub last_sync_error: Option<String>,
}

impl NetboxServer {
    /// The column list every read projects. One constant so a reader and a writer cannot name
    /// different sets — the `alert_history` shape, where a dropped column compiles and then fails
    /// at `try_get` in production.
    const COLUMNS: &'static str = "id, name, base_url, credential_id, ca_cert_pem, enabled, \
                                   sync_interval_secs, api_version, last_sync_at, last_sync_ok, \
                                   last_sync_error";

    fn from_row(row: &sqlx::postgres::PgRow) -> Result<Self, sqlx::Error> {
        Ok(Self {
            id: row.try_get("id")?,
            name: row.try_get("name")?,
            base_url: row.try_get("base_url")?,
            credential_id: row.try_get("credential_id")?,
            ca_cert_pem: row.try_get("ca_cert_pem")?,
            enabled: row.try_get("enabled")?,
            sync_interval_secs: row.try_get("sync_interval_secs")?,
            api_version: row.try_get("api_version")?,
            last_sync_at: row.try_get("last_sync_at")?,
            last_sync_ok: row.try_get("last_sync_ok")?,
            last_sync_error: row.try_get("last_sync_error")?,
        })
    }

    /// How often this server should sync.
    ///
    /// Two guards, and they answer different questions. A **non-positive** stored value is a
    /// corrupt row rather than a request to sync constantly, so it falls back to the column's own
    /// `DEFAULT` — the failure mode of bad data is then "the usual cadence", never "hammer someone
    /// else's NetBox". A **small positive** value is a deliberate choice, so it is honoured down to
    /// [`MIN_SYNC_INTERVAL`] and floored there.
    ///
    /// ⚠️ The first version wrote this as one `unwrap_or(3600).max(MIN)`, which reads as a floor
    /// and is not one: `u64::try_from(-5)` fails, so a negative became 3600 while the doc claimed
    /// it became the floor. Two rules, two lines.
    #[must_use]
    pub fn interval(&self) -> Duration {
        let secs = match u64::try_from(self.sync_interval_secs) {
            Ok(0) | Err(_) => DEFAULT_SYNC_INTERVAL_SECS,
            Ok(n) => n,
        };
        Duration::from_secs(secs).max(MIN_SYNC_INTERVAL)
    }
}

/// `netbox_servers` + `netbox_groups`, and the `node_groups` upsert a sync performs.
pub struct NetboxRepo {
    pool: PgPool,
}

/// What an edit changes about a configured server.
///
/// A struct rather than a parameter list because the fields are near-identical types in a row, and
/// because [`Self::ca_cert_pem`] is three-state — the one field a caller can get subtly wrong.
pub struct ServerUpdate<'a> {
    pub name: &'a str,
    pub base_url: &'a str,
    pub credential_id: Uuid,
    /// Three-state, like `GroupRepo::update`'s pool: outer `None` leaves the column alone,
    /// `Some(None)` clears it, `Some(Some(p))` sets it.
    ///
    /// ⚠️ Without the middle state there is no way to remove a pasted CA short of deleting the
    /// server, and without the outer one every unrelated settings save would silently drop it.
    pub ca_cert_pem: Option<Option<&'a str>>,
    pub enabled: bool,
    pub sync_interval_secs: i32,
}

/// What one sync did, for the log line and the API response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncReport {
    pub regions: usize,
    pub sites: usize,
    /// Folders whose `netbox_groups` row was not refreshed by this run — i.e. the object is gone
    /// from NetBox. **Counted, never deleted** (decision 5).
    pub missing: usize,
    /// The database's clock **at the moment the run began**, before a single row was written.
    ///
    /// 🚨 This has to be the *start* and not the finish, and getting it wrong is not subtle — it
    /// marks the whole tree as deleted. Every `last_seen_at` a run writes is necessarily *earlier*
    /// than the moment that run ends, so storing the finish time makes `last_seen_at <
    /// last_sync_at` true for every row the sync just refreshed. Measured: 5 of 5 folders reported
    /// missing immediately after a fully successful sync.
    pub started_at: chrono::DateTime<chrono::Utc>,
}

impl NetboxRepo {
    #[must_use]
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Every configured server, newest last.
    pub async fn list(&self) -> anyhow::Result<Vec<NetboxServer>> {
        let sql = format!(
            "SELECT {} FROM netbox_servers ORDER BY created_at",
            NetboxServer::COLUMNS
        );
        let rows = sqlx::query(&sql).fetch_all(&self.pool).await?;
        Ok(rows
            .iter()
            .map(NetboxServer::from_row)
            .collect::<Result<Vec<_>, _>>()?)
    }

    /// One server by id.
    pub async fn get(&self, id: Uuid) -> anyhow::Result<Option<NetboxServer>> {
        let sql = format!(
            "SELECT {} FROM netbox_servers WHERE id = $1",
            NetboxServer::COLUMNS
        );
        let row = sqlx::query(&sql)
            .bind(id)
            .fetch_optional(&self.pool)
            .await?;
        row.as_ref()
            .map(NetboxServer::from_row)
            .transpose()
            .map_err(Into::into)
    }

    /// Register a server. The caller has already validated `base_url` and `ca_cert_pem`.
    pub async fn create(
        &self,
        name: &str,
        base_url: &str,
        credential_id: Uuid,
        ca_cert_pem: Option<&str>,
        sync_interval_secs: i32,
    ) -> anyhow::Result<Uuid> {
        let id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO netbox_servers \
             (id, name, base_url, credential_id, ca_cert_pem, sync_interval_secs) \
             VALUES ($1, $2, $3, $4, $5, $6)",
        )
        .bind(id)
        .bind(name)
        .bind(base_url)
        .bind(credential_id)
        .bind(ca_cert_pem)
        .bind(sync_interval_secs)
        .execute(&self.pool)
        .await?;
        Ok(id)
    }

    /// Update a server's settings.
    ///
    /// Takes a [`ServerUpdate`] rather than eight positional arguments: two `&str`s, a `Uuid` and a
    /// `bool` next to each other are four chances to swap a pair with no compile error, and the
    /// `Option<Option<&str>>` in the middle is the one a caller is most likely to get backwards.
    pub async fn update(&self, id: Uuid, u: ServerUpdate<'_>) -> anyhow::Result<bool> {
        let ServerUpdate {
            name,
            base_url,
            credential_id,
            ca_cert_pem,
            enabled,
            sync_interval_secs,
        } = u;
        let res = sqlx::query(
            "UPDATE netbox_servers SET name = $2, base_url = $3, credential_id = $4, \
                    ca_cert_pem = CASE WHEN $5 THEN $6 ELSE ca_cert_pem END, \
                    enabled = $7, sync_interval_secs = $8 \
             WHERE id = $1",
        )
        .bind(id)
        .bind(name)
        .bind(base_url)
        .bind(credential_id)
        .bind(ca_cert_pem.is_some())
        .bind(ca_cert_pem.flatten())
        .bind(enabled)
        .bind(sync_interval_secs)
        .execute(&self.pool)
        .await?;
        Ok(res.rows_affected() > 0)
    }

    /// Forget a server. `netbox_groups` cascades; **the folders it created stay**, which is
    /// decision 5 applied to the server itself — disconnecting an integration must not restructure
    /// the monitoring tree. They simply become hand-maintained folders.
    pub async fn delete(&self, id: Uuid) -> anyhow::Result<bool> {
        let res = sqlx::query("DELETE FROM netbox_servers WHERE id = $1")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(res.rows_affected() > 0)
    }

    /// The database's clock. Read before a run starts, stored as `last_sync_at` when it succeeds.
    ///
    /// The *database's* and not the process's, because it is compared against `last_seen_at`, which
    /// the same server stamps with `now()`. Two clocks either side of a `<` is a comparison that
    /// works until the container drifts.
    async fn db_now(&self) -> anyhow::Result<chrono::DateTime<chrono::Utc>> {
        Ok(sqlx::query_scalar("SELECT now()")
            .fetch_one(&self.pool)
            .await?)
    }

    /// Record a **successful** sync. `at` is the run's start ([`SyncReport::started_at`]).
    ///
    /// 🚨 There is deliberately no way to record a failure through this method, and no `ok` flag
    /// anywhere that could be passed `false` with a timestamp. The invariant — `last_sync_at`
    /// advances only on a full success — is therefore held up by the type signature rather than by
    /// a `CASE WHEN` and a comment, which is what the first version had.
    pub async fn record_success(
        &self,
        id: Uuid,
        at: chrono::DateTime<chrono::Utc>,
        api_version: Option<&str>,
    ) -> anyhow::Result<()> {
        sqlx::query(
            "UPDATE netbox_servers SET last_sync_at = $2, last_sync_ok = TRUE, \
                    last_sync_error = NULL, api_version = COALESCE($3, api_version) \
             WHERE id = $1",
        )
        .bind(id)
        .bind(at)
        .bind(api_version)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Record a **failed** sync: the reason, and nothing else.
    ///
    /// `last_sync_at` is untouched, so the previous successful run stays the reference the
    /// "missing from NetBox" mark is measured against. `api_version` is likewise left alone — a
    /// failure must not erase what the last success learned.
    pub async fn record_failure(&self, id: Uuid, error: &str) -> anyhow::Result<()> {
        sqlx::query(
            "UPDATE netbox_servers SET last_sync_ok = FALSE, last_sync_error = $2 WHERE id = $1",
        )
        .bind(id)
        .bind(error)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Upsert one NetBox object's folder, and the mapping row that records who owns it.
    ///
    /// 🚨 The `DO UPDATE SET` names exactly the four columns NetBox owns. **`pool` must never be
    /// added** — that is decision 2's entire content, and a sync that overwrote it would silently
    /// undo an operator's poller placement on every cycle.
    ///
    /// `sort_order` is set from the caller's `order` so siblings stay name-ordered; it is Yagra's
    /// column, but leaving it at the `DEFAULT 0` would make the tree's order depend on insertion
    /// history rather than on anything visible.
    #[allow(clippy::too_many_arguments)] // A parameter struct here would be one shape used once.
    async fn upsert_group(
        &self,
        server_id: Uuid,
        kind: ObjectKind,
        object_id: i64,
        name: &str,
        parent: Option<Uuid>,
        latitude: Option<f64>,
        longitude: Option<f64>,
        order: f64,
    ) -> anyhow::Result<()> {
        let group_id = kind.group_id(server_id, object_id);
        let mut tx = self.pool.begin().await?;
        sqlx::query(
            "INSERT INTO node_groups \
               (id, name, group_type, parent_id, sort_order, latitude, longitude) \
             VALUES ($1, $2, $3, $4, $5, $6, $7) \
             ON CONFLICT (id) DO UPDATE SET \
               name = EXCLUDED.name, \
               group_type = EXCLUDED.group_type, \
               parent_id = EXCLUDED.parent_id, \
               sort_order = EXCLUDED.sort_order, \
               latitude = EXCLUDED.latitude, \
               longitude = EXCLUDED.longitude",
        )
        .bind(group_id)
        .bind(name)
        .bind(kind.group_type().key())
        .bind(parent)
        .bind(order)
        .bind(latitude)
        .bind(longitude)
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "INSERT INTO netbox_groups (server_id, object_kind, object_id, group_id, last_seen_at) \
             VALUES ($1, $2, $3, $4, now()) \
             ON CONFLICT (server_id, object_kind, object_id) DO UPDATE SET \
               group_id = EXCLUDED.group_id, last_seen_at = now()",
        )
        .bind(server_id)
        .bind(kind.as_str())
        .bind(object_id)
        .bind(group_id)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(())
    }

    /// How many of this server's folders were not refreshed by the run that just finished.
    ///
    /// Derived by comparing against `last_sync_at`, which is why that column may only advance on a
    /// full success: after a failed run every row would look stale.
    pub async fn count_missing(&self, server_id: Uuid) -> anyhow::Result<usize> {
        let n: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM netbox_groups g \
             JOIN netbox_servers s ON s.id = g.server_id \
             WHERE g.server_id = $1 AND s.last_sync_at IS NOT NULL \
               AND g.last_seen_at < s.last_sync_at",
        )
        .bind(server_id)
        .fetch_one(&self.pool)
        .await?;
        Ok(usize::try_from(n).unwrap_or(0))
    }
}

// ---------------------------------------------------------------------------------------------
// the sync
// ---------------------------------------------------------------------------------------------

/// Resolve a server's sealed API token. `None` on any failure (missing / wrong kind / unparsable);
/// the caller then skips the sync. Mirrors `meraki::resolve_meraki_key`, and like it the token is
/// never logged.
pub async fn resolve_netbox_token(creds: &CredentialStore, credential_id: Uuid) -> Option<String> {
    match creds.open(credential_id).await {
        Ok(Some((kind, secret))) if kind == KIND_NETBOX_TOKEN => {
            NetboxTokenSecret::parse(&secret).ok().map(|s| s.token)
        }
        Ok(Some(_)) => {
            tracing::warn!("netbox server credential is not a netbox_token kind");
            None
        }
        Ok(None) => None,
        Err(e) => {
            tracing::warn!(error = %e, "failed to open netbox credential");
            None
        }
    }
}

/// Pull one server's region tree and sites into `node_groups`, once.
///
/// Fetch here, decide in [`apply`]. The split is what lets the whole of the interesting half — the
/// ordering, the parenting, the idempotence, the columns that must not be written — be tested
/// against a real database with no HTTP server anywhere (ADR-111's lesson: the code that decides
/// something needs a seam, or its tests can only ever be about the transport).
pub async fn sync_once(
    repo: &NetboxRepo,
    client: &NetboxClient,
    server_id: Uuid,
) -> anyhow::Result<SyncReport> {
    let regions = client.regions().await?;
    let sites = client.sites().await?;
    apply(repo, server_id, &regions, &sites).await
}

/// Write a fetched region tree and site list into `node_groups`.
///
/// Idempotent: every id is derived, so running it twice writes the same rows. The order is fixed —
/// regions shallowest-first (so `parent_id` always resolves), then sites (whose parent is a region
/// that now exists).
///
/// ⚠️ Takes `regions` **already ordered by depth** ([`NetboxClient::regions`] sorts). Handed an
/// arbitrary order it will fail on the foreign key rather than silently mis-parent, which is the
/// preferable failure — but it is not resorted here, because doing so in two places is how the two
/// would eventually disagree.
pub async fn apply(
    repo: &NetboxRepo,
    server_id: Uuid,
    regions: &[NetboxRegion],
    sites: &[NetboxSite],
) -> anyhow::Result<SyncReport> {
    // 🚨 Before the first write. See `SyncReport::started_at` for what goes wrong otherwise.
    let started_at = repo.db_now().await?;

    // Which region ids this server actually returned. A site whose region is filtered out of the
    // caller's view (NetBox permissions) must land at the root rather than pointing at a folder
    // that was never written — a dangling `parent_id` is a foreign-key error that would fail the
    // whole sync over one object.
    let known: std::collections::HashSet<i64> = regions.iter().map(|r| r.id).collect();

    // Siblings are ordered by name so the tree does not depend on NetBox's id order. Built per
    // parent, which is the scope `sort_order` is compared within.
    let mut region_order: BTreeMap<Option<i64>, Vec<(String, i64)>> = BTreeMap::new();
    for r in regions {
        region_order
            .entry(r.parent.as_ref().map(|p| p.id))
            .or_default()
            .push((r.name.clone(), r.id));
    }
    for v in region_order.values_mut() {
        v.sort();
    }
    let order_of =
        |map: &BTreeMap<Option<i64>, Vec<(String, i64)>>, parent: Option<i64>, id: i64| {
            map.get(&parent)
                .and_then(|v| v.iter().position(|(_, i)| *i == id))
                .map_or(0.0, |p| p as f64)
        };

    for r in regions {
        let parent_netbox = r.parent.as_ref().map(|p| p.id);
        // A parent NetBox refused to show us is treated as absent, for the reason above.
        let parent = parent_netbox
            .filter(|p| known.contains(p))
            .map(|p| region_group_id(server_id, p));
        repo.upsert_group(
            server_id,
            ObjectKind::Region,
            r.id,
            &r.name,
            parent,
            None,
            None,
            order_of(&region_order, parent_netbox, r.id),
        )
        .await?;
    }

    let mut site_order: BTreeMap<Option<i64>, Vec<(String, i64)>> = BTreeMap::new();
    for s in sites {
        site_order
            .entry(s.region.as_ref().map(|r| r.id))
            .or_default()
            .push((s.name.clone(), s.id));
    }
    for v in site_order.values_mut() {
        v.sort();
    }

    for s in sites {
        let region_netbox = s.region.as_ref().map(|r| r.id);
        let parent = region_netbox
            .filter(|r| known.contains(r))
            .map(|r| region_group_id(server_id, r));
        repo.upsert_group(
            server_id,
            ObjectKind::Site,
            s.id,
            &s.name,
            parent,
            s.latitude,
            s.longitude,
            // Sites sort after regions within the same parent, so a folder that contains both
            // reads "areas, then places". The offset is large enough that no realistic region
            // count collides with it.
            10_000.0 + order_of(&site_order, region_netbox, s.id),
        )
        .await?;
    }

    Ok(SyncReport {
        regions: regions.len(),
        sites: sites.len(),
        missing: 0,
        started_at,
    })
}

/// Sync one server end to end: resolve the token, build the client, pull, and record the outcome.
///
/// Returns the report on success. The failure path still writes `record_sync(ok=false, …)`, which
/// is what puts the reason on the screen — and it deliberately leaves `last_sync_at` where it was.
pub async fn sync_server(
    repo: &NetboxRepo,
    creds: &CredentialStore,
    server: &NetboxServer,
) -> anyhow::Result<SyncReport> {
    let outcome = async {
        let token = resolve_netbox_token(creds, server.credential_id)
            .await
            .ok_or_else(|| anyhow::anyhow!("no usable netbox_token credential"))?;
        let client = NetboxClient::new(&server.base_url, &token, server.ca_cert_pem.as_deref())?;
        let probe = client.probe().await?;
        if !probe.authenticated {
            anyhow::bail!("NetBox refused the API token");
        }
        let report = sync_once(repo, &client, server.id).await?;
        Ok::<_, anyhow::Error>((report, probe.netbox_version))
    }
    .await;

    match outcome {
        Ok((mut report, version)) => {
            repo.record_success(server.id, report.started_at, version.as_deref())
                .await?;
            // Counted after `last_sync_at` moved, because that timestamp is what "missing" is
            // measured against.
            report.missing = repo.count_missing(server.id).await?;
            tracing::info!(
                server = %server.id,
                regions = report.regions,
                sites = report.sites,
                missing = report.missing,
                "netbox sync completed"
            );
            Ok(report)
        }
        Err(e) => {
            // `{e}`, never `{e:?}`: the chain can carry a URL, and the token is not in it but the
            // habit is what keeps it that way.
            let msg = e.to_string();
            repo.record_failure(server.id, &msg).await?;
            tracing::warn!(server = %server.id, error = %msg, "netbox sync failed");
            Err(e)
        }
    }
}

/// The leader-only periodic sync.
///
/// ⚠️ **Spawned from `LeaderTasks`, never from `run_live`** — ADR-090, pinned by
/// `run_live_starts_no_task_of_its_own`. Leader-gated because two cores syncing the same server
/// would both write the same folders: harmless by construction (the upsert is idempotent) but
/// twice the load on someone else's NetBox for nothing.
///
/// Returned as a future rather than self-spawning, so `yagra_telemetry::spawn_cancellable` owns the
/// shutdown path the way it does for every other background loop in this binary.
pub async fn run_sync_loop(repo: Arc<NetboxRepo>, creds: Arc<CredentialStore>) {
    let mut tick = tokio::time::interval(TICK);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tick.tick().await;
        let servers = match repo.list().await {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(error = %e, "netbox: could not list servers");
                continue;
            }
        };
        let now = chrono::Utc::now();
        for server in servers.iter().filter(|s| s.enabled) {
            let due = server.last_sync_at.is_none_or(|last| {
                now.signed_duration_since(last).to_std().unwrap_or_default() >= server.interval()
            });
            if !due {
                continue;
            }
            // One server's failure must not stop the others, and `sync_server` has already
            // recorded the reason on its row.
            let _ = sync_server(&repo, &creds, server).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_folder_id_is_derived_and_therefore_stable_across_syncs() {
        // The property the whole "idempotent upsert" design rests on: the same object must produce
        // the same folder id forever, or a re-sync duplicates the tree.
        let s1 = Uuid::from_u128(1);
        let s2 = Uuid::from_u128(2);
        assert_eq!(region_group_id(s1, 7), region_group_id(s1, 7));
        assert_eq!(site_group_id(s1, 7), site_group_id(s1, 7));
        // A region and a site with the same NetBox id are different objects.
        assert_ne!(region_group_id(s1, 7), site_group_id(s1, 7));
        // Two servers must not collide: NetBox ids restart at 1 in every deployment, so without
        // the server in the derivation, two NetBoxes would fight over one folder.
        assert_ne!(region_group_id(s1, 7), region_group_id(s2, 7));
    }

    #[test]
    fn object_kind_round_trips_and_maps_onto_the_folder_types_that_already_existed() {
        for k in ObjectKind::ALL {
            assert_eq!(ObjectKind::from_str(k.as_str()), Some(k));
            // `group_id` must agree with the free functions, or a caller reaching for either
            // spelling would write a different row.
            assert_eq!(k.group_id(Uuid::from_u128(3), 9), {
                match k {
                    ObjectKind::Region => region_group_id(Uuid::from_u128(3), 9),
                    ObjectKind::Site => site_group_id(Uuid::from_u128(3), 9),
                }
            });
        }
        assert_eq!(ObjectKind::from_str("location"), None);
        assert_eq!(ObjectKind::Region.group_type(), GroupType::Region);
        assert_eq!(ObjectKind::Site.group_type(), GroupType::Site);
    }

    #[test]
    fn the_base_url_check_allows_private_and_refuses_the_escalation_surface() {
        // An NMS monitors inside the perimeter, so RFC1918 is the *normal* case — this is the half
        // that a copied "block everything internal" SSRF rule would get backwards.
        for ok in [
            "http://192.168.1.214:8000/",
            "https://netbox.example.com",
            "http://10.0.0.5",
            "http://[fd00::1]:8000",
        ] {
            assert!(validate_base_url(ok).is_ok(), "{ok} must be allowed");
        }
        for (bad, want) in [
            ("http://127.0.0.1:8000/", BaseUrlError::Blocked),
            ("http://[::1]/", BaseUrlError::Blocked),
            ("http://169.254.169.254/", BaseUrlError::Blocked),
            ("http://[fe80::1]/", BaseUrlError::Blocked),
            ("http://0.0.0.0/", BaseUrlError::Blocked),
            ("ftp://netbox.example.com", BaseUrlError::Scheme),
            ("file:///etc/passwd", BaseUrlError::Scheme),
            ("not a url", BaseUrlError::Malformed),
        ] {
            assert_eq!(validate_base_url(bad), Err(want), "{bad}");
        }
    }

    #[test]
    fn a_bracketed_ipv6_literal_is_actually_inspected() {
        // The specific defect `extensibility.md` §3 records: `host.parse()` reads "[::1]" as a
        // hostname, so the SSRF check is skipped rather than failed. If this module ever grows its
        // own bracket handling, this is what notices.
        assert_eq!(
            validate_base_url("http://[::1]/"),
            Err(BaseUrlError::Blocked)
        );
        // …and the allowed side must survive the round trip too, or the check would be "refuse
        // every IPv6", which passes a refusal-only test just as well.
        assert_eq!(
            validate_base_url("http://[fd00::1]:8000/api/"),
            Ok("http://[fd00::1]:8000".to_owned())
        );
    }

    #[test]
    fn the_base_url_is_normalized_to_scheme_host_port_with_no_path() {
        // A pasted URL usually carries a path (people copy the browser's address bar). Keeping it
        // would produce `/api/dcim/…` appended to `/dcim/regions/`.
        assert_eq!(
            validate_base_url("http://192.168.1.214:8000/dcim/sites/?q=x"),
            Ok("http://192.168.1.214:8000".to_owned())
        );
        assert_eq!(
            validate_base_url("  https://netbox.example.com/  "),
            Ok("https://netbox.example.com".to_owned())
        );
    }

    // ── the lab's own inventory, as fixtures ──────────────────────────────────────────────────
    //
    // Taken verbatim from `http://192.168.1.214:8000/` (NetBox 4.6.9, 2026-09-03) rather than
    // invented, because the shape that mattered — a region tree two levels deep — is exactly the
    // one ADR-100 got wrong from the published spec. A fixture built from the corrected belief
    // would have proved nothing; this one is the belief's source.
    //
    //   Japan (2)
    //   ├─ Ehime (6)     → Matsuyama Home (6)  33.850422 / 132.775909
    //   └─ Kanagawa (7)  → Yokohama Home  (7)  35.53652  / 139.51545

    fn region(id: i64, name: &str, parent: Option<i64>, depth: i32) -> NetboxRegion {
        NetboxRegion {
            id,
            name: name.to_owned(),
            parent: parent.map(|id| NestedRef { id }),
            depth,
        }
    }

    fn site(id: i64, name: &str, region: Option<i64>, geo: Option<(f64, f64)>) -> NetboxSite {
        NetboxSite {
            id,
            name: name.to_owned(),
            region: region.map(|id| NestedRef { id }),
            latitude: geo.map(|g| g.0),
            longitude: geo.map(|g| g.1),
        }
    }

    fn lab_regions() -> Vec<NetboxRegion> {
        vec![
            region(2, "Japan", None, 0),
            region(6, "Ehime", Some(2), 1),
            region(7, "Kanagawa", Some(2), 1),
        ]
    }

    fn lab_sites() -> Vec<NetboxSite> {
        vec![
            site(6, "Matsuyama Home", Some(6), Some((33.850422, 132.775909))),
            site(7, "Yokohama Home", Some(7), Some((35.53652, 139.51545))),
        ]
    }

    /// A registered server row to hang a sync off.
    async fn lab_server(pool: &sqlx::PgPool) -> (NetboxRepo, Uuid) {
        let cred = crate::pgtest::credential(pool, "netbox-token", KIND_NETBOX_TOKEN).await;
        let repo = NetboxRepo::new(pool.clone());
        let id = repo
            .create("lab", "http://192.168.1.214:8000", cred, None, 3600)
            .await
            .expect("create server");
        (repo, id)
    }

    /// One folder's stored row, by the id the derivation gives it.
    async fn folder(
        pool: &sqlx::PgPool,
        id: Uuid,
    ) -> Option<(
        String,
        Option<Uuid>,
        Option<f64>,
        Option<f64>,
        Option<String>,
    )> {
        sqlx::query_as(
            "SELECT name, parent_id, latitude, longitude, pool FROM node_groups WHERE id = $1",
        )
        .bind(id)
        .fetch_optional(pool)
        .await
        .expect("read folder")
    }

    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn the_lab_hierarchy_lands_as_a_tree_and_a_second_sync_changes_nothing(
        pool: sqlx::PgPool,
    ) {
        let (repo, server) = lab_server(&pool).await;

        let report = apply(&repo, server, &lab_regions(), &lab_sites())
            .await
            .expect("first sync");
        assert_eq!((report.regions, report.sites), (3, 2));
        assert_eq!(crate::pgtest::rows(&pool, "node_groups").await, 5);

        // The shape, not just the count: this is the assertion that would have failed against the
        // pre-correction belief that regions are one flat layer.
        let japan = region_group_id(server, 2);
        let ehime = region_group_id(server, 6);
        let matsuyama = site_group_id(server, 6);
        assert_eq!(folder(&pool, japan).await.expect("Japan").1, None, "root");
        assert_eq!(
            folder(&pool, ehime).await.expect("Ehime").1,
            Some(japan),
            "a region's parent is a region — the tree is recursive"
        );
        let (name, parent, lat, lon, _) = folder(&pool, matsuyama).await.expect("Matsuyama");
        assert_eq!(name, "Matsuyama Home");
        assert_eq!(parent, Some(ehime), "a site hangs off its region");
        // The Geo-map pins fill themselves in, which is ADR-100 decision 4's payoff.
        assert_eq!(lat, Some(33.850422));
        assert_eq!(lon, Some(132.775909));

        // Idempotence — the property every derived id exists for. A second run must not duplicate
        // the tree, and `create` is not called again.
        apply(&repo, server, &lab_regions(), &lab_sites())
            .await
            .expect("second sync");
        assert_eq!(
            crate::pgtest::rows(&pool, "node_groups").await,
            5,
            "a re-sync must not duplicate folders"
        );
        assert_eq!(crate::pgtest::rows(&pool, "netbox_groups").await, 5);
    }

    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn a_sync_owns_the_name_and_the_operator_owns_the_pool(pool: sqlx::PgPool) {
        // ADR-100 decision 2, both directions, which is the whole design in one test.
        let (repo, server) = lab_server(&pool).await;
        apply(&repo, server, &lab_regions(), &lab_sites())
            .await
            .expect("sync");

        let ehime = region_group_id(server, 6);
        // The operator does two things: renames a folder, and assigns it a poller pool.
        sqlx::query("UPDATE node_groups SET name = 'Renamed', pool = 'site-a' WHERE id = $1")
            .bind(ehime)
            .execute(&pool)
            .await
            .expect("operator edit");

        apply(&repo, server, &lab_regions(), &lab_sites())
            .await
            .expect("re-sync");

        let (name, _, _, _, folder_pool) = folder(&pool, ehime).await.expect("Ehime");
        assert_eq!(
            name, "Ehime",
            "NetBox owns the name, so the rename is undone"
        );
        assert_eq!(
            folder_pool.as_deref(),
            Some("site-a"),
            "the operator owns `pool` — a sync that overwrote it would silently undo their \
             poller placement on every cycle (ADR-100 decision 2)"
        );
    }

    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn an_object_that_disappears_from_netbox_is_marked_and_never_deleted(pool: sqlx::PgPool) {
        let (repo, server) = lab_server(&pool).await;
        let first = apply(&repo, server, &lab_regions(), &lab_sites())
            .await
            .expect("sync");
        repo.record_success(server, first.started_at, Some("4.6.9"))
            .await
            .expect("record");
        // 🚨 The regression that motivated `started_at`. Storing the run's *finish* time here made
        // this 5: every row a successful sync had just written read as older than the stamp, so a
        // healthy tree reported itself entirely deleted.
        assert_eq!(
            repo.count_missing(server).await.expect("count"),
            0,
            "a successful sync must not mark the folders it just wrote"
        );

        // Yokohama is decommissioned in NetBox. Every other object is still returned.
        let sites: Vec<NetboxSite> = lab_sites().into_iter().filter(|s| s.id != 7).collect();
        let second = apply(&repo, server, &lab_regions(), &sites)
            .await
            .expect("re-sync");
        repo.record_success(server, second.started_at, Some("4.6.9"))
            .await
            .expect("record");

        assert_eq!(
            crate::pgtest::rows(&pool, "node_groups").await,
            5,
            "deleting a folder re-parents its child nodes, so an external system's one mistaken \
             click must never do it (ADR-100 decision 5)"
        );
        assert_eq!(
            repo.count_missing(server).await.expect("count"),
            1,
            "…but it must be *marked*, or the integration is silently out of date"
        );
        // …and it is the *site* that is marked, not something else that happened to be stale.
        // Without this the count above is satisfied by any one row going missing.
        let stale: Vec<String> = sqlx::query_scalar(
            "SELECT g.object_kind || ':' || g.object_id FROM netbox_groups g \
             JOIN netbox_servers s ON s.id = g.server_id \
             WHERE g.last_seen_at < s.last_sync_at",
        )
        .fetch_all(&pool)
        .await
        .expect("stale rows");
        assert_eq!(stale, vec![format!("{}:7", ObjectKind::Site.as_str())]);
    }

    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn a_failed_sync_does_not_advance_the_timestamp_the_mark_is_measured_against(
        pool: sqlx::PgPool,
    ) {
        // 🚨 The trap this guards is not "the error is lost" — it is that a *successful-looking*
        // failed sync marks the entire tree as deleted, because nothing was seen this run. Same
        // shape as ADR-080's "a failed read must never become an empty read".
        let (repo, server) = lab_server(&pool).await;
        let report = apply(&repo, server, &lab_regions(), &lab_sites())
            .await
            .expect("sync");
        repo.record_success(server, report.started_at, Some("4.6.9"))
            .await
            .expect("ok");
        let after_success = repo.get(server).await.expect("get").expect("row");
        let stamp = after_success
            .last_sync_at
            .expect("a successful sync stamps");

        repo.record_failure(server, "connection refused")
            .await
            .expect("fail");
        let after_failure = repo.get(server).await.expect("get").expect("row");

        assert_eq!(
            after_failure.last_sync_at,
            Some(stamp),
            "a failed sync must not move last_sync_at, or every folder reads as missing"
        );
        assert_eq!(after_failure.last_sync_ok, Some(false));
        assert_eq!(
            after_failure.last_sync_error.as_deref(),
            Some("connection refused"),
            "and the reason belongs on the screen, not only in the container log"
        );
        assert_eq!(
            after_failure.api_version.as_deref(),
            Some("4.6.9"),
            "a failure must not erase what the last success learned"
        );
        assert_eq!(
            repo.count_missing(server).await.expect("count"),
            0,
            "nothing is marked missing by a failure"
        );
    }

    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn a_site_whose_region_was_not_returned_lands_at_the_root(pool: sqlx::PgPool) {
        // NetBox permissions can hide a region from the token while still listing its sites. A
        // dangling `parent_id` is a foreign-key error that would fail the *whole* sync over one
        // object, so the site is re-homed instead.
        let (repo, server) = lab_server(&pool).await;
        let sites = vec![site(9, "Orphan", Some(999), None)];
        apply(&repo, server, &lab_regions(), &sites)
            .await
            .expect("sync must not fail on an invisible parent");
        assert_eq!(
            folder(&pool, site_group_id(server, 9))
                .await
                .expect("row")
                .1,
            None
        );

        // The accept side: a site whose region *is* visible must still be parented, or "put
        // everything at the root" would pass the assertion above.
        apply(&repo, server, &lab_regions(), &lab_sites())
            .await
            .expect("sync");
        assert_eq!(
            folder(&pool, site_group_id(server, 6))
                .await
                .expect("row")
                .1,
            Some(region_group_id(server, 6))
        );
    }

    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn forgetting_a_server_keeps_the_folders_it_created(pool: sqlx::PgPool) {
        // Same reasoning as decision 5: disconnecting an integration must not restructure the
        // monitoring tree. The folders simply become hand-maintained.
        let (repo, server) = lab_server(&pool).await;
        apply(&repo, server, &lab_regions(), &lab_sites())
            .await
            .expect("sync");
        assert!(repo.delete(server).await.expect("delete"));
        assert_eq!(crate::pgtest::rows(&pool, "netbox_groups").await, 0);
        assert_eq!(
            crate::pgtest::rows(&pool, "node_groups").await,
            5,
            "the tree survives the integration being removed"
        );
    }

    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn a_server_round_trips_and_its_ca_is_three_state(pool: sqlx::PgPool) {
        let cred = crate::pgtest::credential(&pool, "netbox-token", KIND_NETBOX_TOKEN).await;
        let repo = NetboxRepo::new(pool.clone());
        let id = repo
            .create("lab", "http://192.168.1.214:8000", cred, Some("PEM"), 900)
            .await
            .expect("create");

        let got = repo.get(id).await.expect("get").expect("row");
        assert_eq!(got.base_url, "http://192.168.1.214:8000");
        assert_eq!(got.ca_cert_pem.as_deref(), Some("PEM"));
        assert_eq!(got.sync_interval_secs, 900);
        assert!(got.enabled, "a new server starts enabled");
        assert_eq!(repo.list().await.expect("list").len(), 1);

        let edit = |ca| ServerUpdate {
            name: "lab2",
            base_url: "http://192.168.1.214:8000",
            credential_id: cred,
            ca_cert_pem: ca,
            enabled: false,
            sync_interval_secs: 1800,
        };

        // Outer `None` leaves the CA alone — otherwise every settings save would silently drop a
        // certificate the form does not round-trip.
        repo.update(id, edit(None)).await.expect("update");
        let got = repo.get(id).await.expect("get").expect("row");
        assert_eq!(got.name, "lab2");
        assert!(!got.enabled);
        assert_eq!(got.ca_cert_pem.as_deref(), Some("PEM"), "untouched");

        // `Some(None)` is the only way to remove one without deleting the server.
        repo.update(
            id,
            ServerUpdate {
                enabled: true,
                ..edit(Some(None))
            },
        )
        .await
        .expect("clear ca");
        assert_eq!(
            repo.get(id).await.expect("get").expect("row").ca_cert_pem,
            None
        );
    }

    /// Mint a real self-signed certificate, so the accept side of the CA check is a certificate
    /// and not another string that happens to pass.
    fn a_real_certificate_pem() -> String {
        let kp = rcgen::KeyPair::generate().expect("keypair");
        rcgen::CertificateParams::new(vec!["netbox.test".to_owned()])
            .expect("params")
            .self_signed(&kp)
            .expect("sign")
            .pem()
    }

    #[test]
    fn a_ca_certificate_is_checked_when_it_is_pasted_not_when_it_is_used() {
        // 🚨 The reason this function is not the one-liner it looks like, measured on this
        // reqwest/rustls pair. Both obvious spellings accept junk, so both are inert as
        // validations. If either of these ever starts failing, `validate_ca_pem` can be
        // simplified — until then the block-counting is load-bearing.
        assert!(
            reqwest::Certificate::from_pem(b"not pem at all").is_ok(),
            "from_pem defers parsing, so it accepts anything"
        );
        assert!(
            reqwest::Certificate::from_pem(b"not pem at all")
                .map(|c| reqwest::Client::builder().add_root_certificate(c).build())
                .is_ok_and(|b| b.is_ok()),
            "…and building a client with zero certificate blocks adds zero roots and succeeds"
        );

        // Refuse side.
        assert!(validate_ca_pem("").is_err(), "empty");
        assert!(validate_ca_pem("   \n ").is_err(), "whitespace");
        assert!(validate_ca_pem("not pem at all").is_err(), "not PEM");
        assert!(
            validate_ca_pem("-----BEGIN CERTIFICATE-----\nnope\n-----END CERTIFICATE-----\n")
                .is_err(),
            "PEM armour around garbage"
        );
        // A private key in the certificate field goes into a plaintext, API-readable column.
        let kp = rcgen::KeyPair::generate().expect("keypair");
        assert!(
            validate_ca_pem(&kp.serialize_pem()).is_err(),
            "a private key pasted into the CA field must be refused, not stored"
        );

        // Accept side — without it, "refuse everything" passes every assertion above
        // (`rejection-only-tests-pass-when-everything-rejects`).
        let real = a_real_certificate_pem();
        assert_eq!(validate_ca_pem(&real), Ok(()), "a real certificate");
        // …and it must be usable for the thing it was validated for.
        assert!(
            NetboxClient::new("https://netbox.example.com", "t", Some(&real)).is_ok(),
            "a certificate that validates must also build a client"
        );
    }

    #[test]
    fn the_region_listing_is_ordered_so_a_parent_is_always_written_first() {
        // The correction ADR-100 needed: regions are a tree. Writing a child before its parent
        // fails `node_groups.parent_id`'s foreign key, and the lab's own data (Japan → Ehime) is
        // enough to hit it.
        let mut rows = [
            NetboxRegion {
                id: 6,
                name: "Ehime".into(),
                parent: Some(NestedRef { id: 2 }),
                depth: 1,
            },
            NetboxRegion {
                id: 2,
                name: "Japan".into(),
                parent: None,
                depth: 0,
            },
        ];
        rows.sort_by(|a, b| a.depth.cmp(&b.depth).then_with(|| a.id.cmp(&b.id)));
        assert_eq!(
            rows.iter().map(|r| r.id).collect::<Vec<_>>(),
            vec![2, 6],
            "shallowest first"
        );
    }

    #[test]
    fn a_region_page_from_the_real_api_parses() {
        // Captured from the lab (NetBox 4.6.9) rather than invented, so the field names are the
        // ones the server actually sends. The trimmed fields are the ones this module ignores.
        let body = r#"{"count":3,"next":null,"previous":null,"results":[
            {"id":2,"name":"Japan","slug":"japan","parent":null,"_depth":0,"site_count":2},
            {"id":6,"name":"Ehime","slug":"ehime",
             "parent":{"id":2,"name":"Japan","slug":"japan","_depth":0},"_depth":1}]}"#;
        let page: Page<NetboxRegion> = serde_json::from_str(body).expect("parses");
        assert_eq!(page.count, 3);
        assert!(page.next.is_none());
        assert_eq!(page.results[0].depth, 0);
        assert!(page.results[0].parent.is_none());
        assert_eq!(page.results[1].parent.as_ref().map(|p| p.id), Some(2));
    }

    #[test]
    fn a_site_page_from_the_real_api_parses_including_the_geo_columns() {
        let body = r#"{"count":2,"next":null,"previous":null,"results":[
            {"id":6,"name":"Matsuyama Home","slug":"matsuyama-home",
             "status":{"value":"active","label":"Active"},
             "region":{"id":6,"name":"Ehime","slug":"ehime","_depth":1},
             "latitude":33.850422,"longitude":132.775909},
            {"id":9,"name":"No Region","slug":"no-region","region":null,
             "latitude":null,"longitude":null}]}"#;
        let page: Page<NetboxSite> = serde_json::from_str(body).expect("parses");
        assert_eq!(page.results[0].region.as_ref().map(|r| r.id), Some(6));
        assert_eq!(page.results[0].latitude, Some(33.850422));
        assert_eq!(page.results[0].longitude, Some(132.775909));
        // A site with no region is legal in NetBox and must land at the root rather than fail.
        assert!(page.results[1].region.is_none());
        assert_eq!(page.results[1].latitude, None);
    }

    #[test]
    fn the_sync_interval_has_a_floor_so_a_bad_row_cannot_spin() {
        let mut s = NetboxServer {
            id: Uuid::nil(),
            name: "x".into(),
            base_url: "http://x".into(),
            credential_id: Uuid::nil(),
            ca_cert_pem: None,
            enabled: true,
            sync_interval_secs: 3600,
            api_version: None,
            last_sync_at: None,
            last_sync_ok: None,
            last_sync_error: None,
        };
        assert_eq!(s.interval(), Duration::from_secs(3600));
        // A deliberate short cadence is honoured, then floored — this is the accept side, and
        // without it "always return the default" would satisfy the two corrupt cases below.
        s.sync_interval_secs = 600;
        assert_eq!(s.interval(), Duration::from_secs(600));
        s.sync_interval_secs = 5;
        assert_eq!(s.interval(), MIN_SYNC_INTERVAL, "floored, not honoured");
        // A corrupt row falls back to the usual cadence, NOT to the floor: bad data must not turn
        // into the highest request rate the system allows.
        s.sync_interval_secs = 0;
        assert_eq!(
            s.interval(),
            Duration::from_secs(DEFAULT_SYNC_INTERVAL_SECS)
        );
        s.sync_interval_secs = -5;
        assert_eq!(
            s.interval(),
            Duration::from_secs(DEFAULT_SYNC_INTERVAL_SECS)
        );
    }

    /// The read-only promise (ADR-100 decision 1) as a check rather than as prose.
    ///
    /// ⚠️ Reads through `module_source`, never `include_str!` — that accessor drops this test
    /// module, so the forbidden needles below cannot match the lines they are written on.
    /// A literal matched against a file's own raw text passes forever, which is the quiet half of
    /// the failure `assert_no_file_matches_a_literal_against_its_own_text` exists for.
    #[test]
    fn the_client_only_ever_issues_reads() {
        let src = crate::module_source::code("src", "netbox");
        for forbidden in [".post(", ".put(", ".patch(", ".send_form("] {
            assert!(
                !src.contains(forbidden),
                "netbox.rs must issue no {forbidden} — the integration is strictly read-only \
                 (ADR-100 decision 1), and Yagra is not the source of truth for NetBox's data"
            );
        }
        // The accept side. Without it a file that stopped issuing HTTP at all — or that this
        // reader returned empty for — would satisfy every assertion above.
        assert!(
            src.contains("fn get(&self, path_and_query: &str)"),
            "…and it must still be the module that does the reading"
        );
    }

    /// Decision 2's centre, as a build failure rather than as a comment.
    #[test]
    fn the_folder_upsert_never_writes_the_operators_column() {
        let src = crate::module_source::code("src", "netbox");
        // Slice exactly the one statement, from its INSERT to the start of the next one. A
        // fixed-length window would drift with formatting and could silently include or exclude
        // the clause under test.
        let start = src
            .find("INSERT INTO node_groups")
            .expect("the node_groups upsert is here");
        let end = src[start..]
            .find("INSERT INTO netbox_groups")
            .map_or(src.len(), |o| start + o);
        let stmt = &src[start..end];
        assert!(
            stmt.contains("ON CONFLICT (id) DO UPDATE SET"),
            "the slice must actually contain the upsert's SET clause, or every assertion below \
             is vacuous"
        );
        assert!(
            !stmt.contains("pool"),
            "the node_groups upsert must never write `pool` — that column is the operator's \
             (ADR-100 decision 2), and overwriting it would undo their poller placement on \
             every sync"
        );
        // Accept side: the four columns NetBox *does* own must be in there, or a statement that
        // updates nothing would satisfy the ban above.
        for owned in [
            "name = EXCLUDED.name",
            "parent_id = EXCLUDED.parent_id",
            "latitude = EXCLUDED.latitude",
            "longitude = EXCLUDED.longitude",
        ] {
            assert!(stmt.contains(owned), "{owned} must be synced");
        }
    }
}
