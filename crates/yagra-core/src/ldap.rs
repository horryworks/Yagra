// SPDX-License-Identifier: AGPL-3.0-only
//! LDAP/AD authentication (ADR-041 Increment 1).
//!
//! One operator-configured directory lets people sign in with their corporate account. Unlike OIDC
//! there is no redirect leg: the existing `POST /api/v1/auth/login` form is the whole UI, and
//! `api/session.rs` routes a name here when the account says `auth_source = 'ldap'` or when no
//! account exists yet and a directory is configured. Local accounts are tried first and never reach
//! this module, so a directory that is down or misconfigured cannot lock out a break-glass admin.
//!
//! **Two-stage bind.** A service account searches for the person's entry, then a *second,
//! independent* connection binds as the DN that search returned. Building a DN from the typed
//! username instead would break on any OU layout the pattern did not anticipate, and rebinding the
//! service connection would leave the group search running with the user's own rights — which
//! usually cannot read `member`, so the groups come back empty and the role silently degrades.
//!
//! **Group→role reuses the OIDC mechanism** (`oidc::resolve_role`, ADR-041 decision 2). What this
//! module adds is the normalisation in front of it: a directory reports groups as DNs
//! (`CN=NetOps,OU=Groups,DC=corp,DC=example,DC=com`) while an operator wants to type `NetOps`, and
//! LDAP treats both case-insensitively. That folding happens here, on the LDAP candidates only, so
//! OIDC's exact matching is unchanged.
//!
//! **Transport is always TLS.** [`LdapSecurity`] has no plaintext variant, the trust anchors come
//! from [`crate::tls`], and the crate's skip-certificate-checks switch is never called — a test
//! reads this file to keep it that way, so the switch's name is deliberately not written here.

use std::collections::HashMap;
use std::sync::LazyLock;
use std::time::Duration;

use ldap3::{LdapConnAsync, LdapConnSettings, Scope, SearchEntry, SearchOptions};
use serde::{Deserialize, Serialize};
use sqlx::{PgPool, Row};
use tokio::sync::Semaphore;
use uuid::Uuid;
use yagra_common::Role;
use yagra_secrets::{EnvelopeCipher, SealedSecret};

use crate::secrets::Kek;

// ── Limits ──────────────────────────────────────────────────────────────────────────────────────

/// TCP + TLS establishment. Short: a login is interactive and a directory on the far side of a dead
/// route must not hold the request open.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
/// Per-operation ceiling (bind, search).
const OP_TIMEOUT: Duration = Duration::from_secs(10);
/// The outer bound on the whole exchange. Not redundant with the two above: a peer that completes
/// the TCP handshake and then says nothing during the TLS one is covered by neither.
const OVERALL_TIMEOUT: Duration = Duration::from_secs(15);
/// Concurrent directory conversations. A DC outage otherwise turns every retrying browser into a
/// connection storm against the customer's domain controller. `try_acquire`, never queued —
/// queueing would push callers past `OVERALL_TIMEOUT` while still holding the request.
const MAX_CONCURRENT_BINDS: usize = 8;
/// How many groups the Test button echoes back. A user in hundreds of groups should not be able to
/// turn a diagnostic into a wall of text.
const MAX_GROUPS_REPORTED: usize = 100;
/// Cap on a stage's detail string in the Test result — a remote server does not get to paste an
/// essay into the settings page (the `api/rca.rs` discipline).
const STAGE_DETAIL_MAX_CHARS: usize = 200;

/// The synthetic provider id written to `users.oidc_provider_id` for a directory account.
///
/// `ldap_config` is a singleton `SMALLINT` row and that column is a `UUID` (mig 0044), so the
/// identity index needs a stable value that is not the row id. Name-derived rather than a random
/// constant, per `extensibility.md` §6.
///
/// **Changing this orphans every account already provisioned** — their stored pair would no longer
/// match, so the next login would try to JIT-create an account whose username is taken and be
/// refused. A test pins the literal.
pub static LDAP_PROVIDER_ID: LazyLock<Uuid> =
    LazyLock::new(|| Uuid::new_v5(&Uuid::NAMESPACE_URL, b"yagra:ldap-directory"));

static BIND_SLOTS: LazyLock<Semaphore> = LazyLock::new(|| Semaphore::new(MAX_CONCURRENT_BINDS));

// ── Types ───────────────────────────────────────────────────────────────────────────────────────

/// How the connection is protected. **Two variants, both TLS** — there is deliberately no plaintext
/// option, so `ldap://` cannot be configured into existence and the bind password cannot cross the
/// wire in the clear. Adding one later would be a certificate-verification-disable flag by another
/// name and needs the same argument (ADR-041 decision 2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum LdapSecurity {
    /// Implicit TLS on connect (the `ldaps://` scheme, conventionally port 636).
    Ldaps,
    /// Plain connect upgraded with the StartTLS extended operation (conventionally port 389).
    //
    // Renamed explicitly: `rename_all = "snake_case"` would produce `start_tls`, which disagrees
    // with the `security` column's CHECK and with `as_str`. The round-trip test is what caught it,
    // and the disagreement would have been rows the writer produces and the reader cannot parse.
    #[serde(rename = "starttls")]
    StartTls,
}

impl LdapSecurity {
    /// Every mode, in display order.
    pub const ALL: [LdapSecurity; 2] = [LdapSecurity::Ldaps, LdapSecurity::StartTls];

    /// Stable token — the `ldap_config.security` value and the serde representation.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            LdapSecurity::Ldaps => "ldaps",
            LdapSecurity::StartTls => "starttls",
        }
    }

    /// Parse a stored token; `None` on anything else.
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|m| m.as_str() == s)
    }

    /// Whether the connection starts plain and is upgraded.
    #[must_use]
    const fn starttls(self) -> bool {
        match self {
            LdapSecurity::StartTls => true,
            LdapSecurity::Ldaps => false,
        }
    }
}

/// A fully-resolved directory configuration: bind password decrypted, role map parsed and folded.
///
/// Deliberately does **not** derive `Serialize`. It carries a plaintext credential, and deriving it
/// here is exactly the mistake this type exists to make hard (ADR-018) — [`LdapConfigView`] is what
/// leaves the process.
pub struct LdapConfig {
    pub host: String,
    pub port: u16,
    pub security: LdapSecurity,
    pub ca_cert: Option<String>,
    pub bind_dn: String,
    bind_password: String,
    pub user_base_dn: String,
    pub user_filter: String,
    pub username_attribute: String,
    pub uid_attribute: String,
    pub member_of_attribute: String,
    pub group_base_dn: Option<String>,
    pub group_filter: Option<String>,
    pub group_name_attribute: String,
    /// Keys already lowercased — see [`group_role_keys`].
    pub role_map: HashMap<String, Role>,
    pub default_role: Option<Role>,
}

impl LdapConfig {
    /// The URL to dial, derived from the stored parts so the scheme can never disagree with the
    /// security mode. StartTLS begins as `ldap://` and is upgraded in-band; only `Ldaps` is
    /// implicit TLS.
    #[must_use]
    pub fn url(&self) -> String {
        let scheme = if self.security.starttls() {
            "ldap"
        } else {
            "ldaps"
        };
        // A bracketed literal is what a v6 address needs in a URL authority; `host` is stored bare.
        if self.host.contains(':') && !self.host.starts_with('[') {
            format!("{scheme}://[{}]:{}", self.host, self.port)
        } else {
            format!("{scheme}://{}:{}", self.host, self.port)
        }
    }
}

/// The configuration as the Settings page shows it — **never** the bind password.
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct LdapConfigView {
    pub host: String,
    pub port: u16,
    pub security: LdapSecurity,
    /// The operator-supplied CA certificate, in PEM. A certificate is public, so unlike the bind
    /// password it round-trips through the form rather than being write-only.
    pub ca_cert: Option<String>,
    pub bind_dn: String,
    /// True once a bind password has been stored, so the form can say "set" without revealing it.
    pub has_bind_password: bool,
    pub user_base_dn: String,
    pub user_filter: String,
    pub username_attribute: String,
    pub uid_attribute: String,
    pub member_of_attribute: String,
    pub group_base_dn: Option<String>,
    pub group_filter: Option<String>,
    pub group_name_attribute: String,
    pub role_map: HashMap<String, String>,
    pub default_role: Option<String>,
    pub enabled: bool,
    pub updated_at: String,
}

/// The save payload from the Settings page.
#[derive(Debug, Clone, Deserialize, utoipa::ToSchema)]
pub struct LdapConfigInput {
    pub host: String,
    #[serde(default = "default_port")]
    pub port: u16,
    #[serde(default = "default_security")]
    pub security: LdapSecurity,
    #[serde(default)]
    pub ca_cert: Option<String>,
    pub bind_dn: String,
    /// Write-only credential. Two-valued, **not** three: `None` keeps what is stored, a non-blank
    /// value replaces it. An empty string is a validation error rather than "clear", because a bind
    /// with a DN and no password is an unauthenticated bind that a permissive directory answers
    /// `success` — so "no password" is not a configuration, it is a silent downgrade to anonymous.
    #[serde(default)]
    pub bind_password: Option<String>,
    pub user_base_dn: String,
    #[serde(default = "default_user_filter")]
    pub user_filter: String,
    #[serde(default = "default_username_attribute")]
    pub username_attribute: String,
    #[serde(default = "default_uid_attribute")]
    pub uid_attribute: String,
    #[serde(default = "default_member_of_attribute")]
    pub member_of_attribute: String,
    #[serde(default)]
    pub group_base_dn: Option<String>,
    #[serde(default)]
    pub group_filter: Option<String>,
    #[serde(default = "default_group_name_attribute")]
    pub group_name_attribute: String,
    #[serde(default)]
    pub role_map: HashMap<String, String>,
    #[serde(default)]
    pub default_role: Option<String>,
    #[serde(default)]
    pub enabled: bool,
}

fn default_port() -> u16 {
    636
}
fn default_security() -> LdapSecurity {
    LdapSecurity::Ldaps
}
fn default_user_filter() -> String {
    "(&(objectClass=user)(sAMAccountName={username}))".to_owned()
}
fn default_username_attribute() -> String {
    "sAMAccountName".to_owned()
}
fn default_uid_attribute() -> String {
    "objectGUID".to_owned()
}
fn default_member_of_attribute() -> String {
    "memberOf".to_owned()
}
fn default_group_name_attribute() -> String {
    "cn".to_owned()
}

/// Who the directory says this is, once the search succeeded.
#[derive(Debug, Clone)]
pub struct DirectoryIdentity {
    /// The immutable per-entry id (`objectGUID`/`entryUUID`), stored as the external subject.
    pub subject: String,
    /// The canonical login name, taken from the directory — never from what was typed.
    pub username: String,
    /// The entry's DN, used for the user bind and shown by the Test button.
    pub dn: String,
    /// Group DNs and/or names, as the directory reported them.
    pub groups: Vec<String>,
}

/// The three-way answer a login attempt produces.
///
/// `Unavailable` not collapsing into `Denied` is the most consequential decision in this module.
/// The caller records a throttle failure for `Denied` and **not** for `Unavailable`: if a domain
/// controller outage counted as a wrong password, five attempts would arm the per-account
/// exponential lockout for every user, and when the DC came back everyone would still be locked out
/// of Yagra — an outage that outlives its cause. The client sees an identical 401 either way, so
/// nothing leaks; only the audit log and the throttle can tell them apart.
pub enum LdapAuth {
    /// The directory authenticated them and a role was resolved.
    Ok(Box<DirectoryIdentity>, Role),
    /// The directory answered, and the answer was no.
    Denied(&'static str),
    /// Yagra could not get an answer. Not the person's fault.
    Unavailable(String),
}

/// One step of the Test button's report.
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct LdapTestStage {
    /// `connect` | `bind_service` | `search_user` | `resolve_groups`.
    pub name: String,
    pub ok: bool,
    /// Why it failed, or what it found. Truncated — this is remote-influenced text.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// The Test button's result.
///
/// Staged rather than a single boolean on purpose. This probe cannot prove that *logging in* works,
/// because it deliberately never binds as the user (see [`probe`]) — so an `ok: true` alone would
/// be read as "login works" when the directory may still refuse simple binds, or the account may be
/// disabled. Naming the stages says exactly how far it got.
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct LdapTestResult {
    /// Every stage passed.
    pub ok: bool,
    pub stages: Vec<LdapTestStage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_dn: Option<String>,
    /// Whether the entry carried the configured id attribute — the flag, not the value. Its absence
    /// is the failure this field exists to surface, and the value itself is of no use here.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subject_present: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub username_resolved: Option<String>,
    pub groups: Vec<String>,
    pub groups_truncated: bool,
    /// The role this user would receive. **`None` means they would be denied** — the commonest
    /// misconfiguration, and one the login form reports as an indistinguishable wrong password.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    /// What this probe did not test, so a green result is not over-read.
    pub note: String,
}

// ── Pure helpers (no socket, no database — every one of these is unit-tested) ────────────────────

/// Escape a value for interpolation into a search filter (RFC 4515).
///
/// `ldap3` does **not** escape for you: `search()` takes a `&str` filter, so an unescaped username
/// like `*)(uid=*` rewrites the filter's structure. This wraps the crate's helper rather than
/// calling it at the use site so the property has somewhere to be tested.
#[must_use]
pub fn escape_filter(value: &str) -> String {
    ldap3::ldap_escape(value).into_owned()
}

/// Substitute the escaped username into the configured filter template.
///
/// # Errors
/// Returns operator-facing text when the template has no `{username}` placeholder. That is not
/// pedantry: a filter such as `(&(objectClass=user))` matches every entry under the base DN, and any
/// code path that took the first result would authenticate whoever sorts first. This is one of three
/// independent gates — the others are the save-time validation and the exactly-one-entry rule.
pub fn render_filter(template: &str, escaped_username: &str) -> Result<String, String> {
    if !template.contains("{username}") {
        return Err("the user filter must contain the {username} placeholder".to_owned());
    }
    Ok(template.replace("{username}", escaped_username))
}

/// Whether a password may be sent in a simple bind at all.
///
/// RFC 4513: a simple bind with a non-empty DN and an **empty** password is an *unauthenticated*
/// bind, and many directories answer it `success`. Sending one would authenticate anybody who left
/// the password box blank. Checked as the first statement of [`authenticate`], before the semaphore
/// and before any socket, so no caller can route around it.
#[must_use]
pub fn password_is_bindable(password: &str) -> bool {
    !password.trim().is_empty()
}

/// Split the first RDN of a DN into `(attribute, value)`, honouring `\,` and `\=` escapes.
///
/// `CN=Net\,Ops,OU=Groups,DC=x` has the first RDN `CN=Net\,Ops`, not `CN=Net\`.
#[must_use]
pub fn parse_first_rdn(dn: &str) -> Option<(String, String)> {
    let mut escaped = false;
    let mut end = dn.len();
    for (i, c) in dn.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        match c {
            '\\' => escaped = true,
            ',' => {
                end = i;
                break;
            }
            _ => {}
        }
    }
    let rdn = &dn[..end];
    let mut escaped = false;
    for (i, c) in rdn.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        match c {
            '\\' => escaped = true,
            '=' => {
                return Some((rdn[..i].trim().to_owned(), rdn[i + 1..].trim().to_owned()));
            }
            _ => {}
        }
    }
    None
}

/// The keys a directory group should be looked up under, lowercased.
///
/// A group arrives as a DN (`CN=NetOps,OU=Groups,DC=corp,DC=example,DC=com`) but an operator wants
/// to type `NetOps`, so both spellings map to the same role. The DN form is normalised by trimming
/// space around each comma, because `CN=NetOps, OU=Groups` and `CN=NetOps,OU=Groups` are the same
/// group and a directory may report either.
///
/// Case folding is not politeness: DNs and CNs are case-insensitive in practice, and an operator who
/// types `netops` for `NetOps` gets a login denial with a generic 401 and no diagnostic — which they
/// will "fix" by setting `default_role: admin`.
#[must_use]
pub fn group_role_keys(group: &str) -> Vec<String> {
    let compact: String = group
        .split(',')
        .map(str::trim)
        .collect::<Vec<_>>()
        .join(",");
    let mut keys = vec![compact.to_ascii_lowercase()];
    if let Some((_, value)) = parse_first_rdn(&compact) {
        let value = value.to_ascii_lowercase();
        if !keys.contains(&value) {
            keys.push(value);
        }
    }
    keys
}

/// Fold an operator-typed `role_map` into the exact-match form [`crate::oidc::resolve_role`] wants.
///
/// Keys are lowercased here, once at load, so the stored row keeps whatever casing the operator
/// typed — rewriting their input in the form they are looking at is its own confusion. Two keys that
/// collide under lowercase keep the **higher** role: deterministic, and it fails in the direction
/// they meant when they wrote both.
#[must_use]
pub fn fold_role_map(raw: &HashMap<String, String>) -> HashMap<String, Role> {
    let mut out: HashMap<String, Role> = HashMap::new();
    for (group, role) in raw {
        let Some(role) = Role::parse_lenient(role) else {
            tracing::warn!(group = %group, role = %role, "LDAP role_map has unknown role; skipping");
            continue;
        };
        let key = group.trim().to_ascii_lowercase();
        out.entry(key)
            .and_modify(|existing| {
                if role > *existing {
                    tracing::warn!(
                        group = %group,
                        "two LDAP role_map entries differ only in case; keeping the higher role"
                    );
                    *existing = role;
                }
            })
            .or_insert(role);
    }
    out
}

/// Look an attribute up case-insensitively.
///
/// LDAP attribute *names* are case-insensitive and a server may echo `samaccountname` for a
/// `sAMAccountName` request. An exact `HashMap` lookup would read that as "absent", which for the id
/// attribute means refusing every login against an otherwise fine directory.
fn attr_values<'a>(map: &'a HashMap<String, Vec<String>>, name: &str) -> Option<&'a Vec<String>> {
    map.iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(name))
        .map(|(_, v)| v)
}

fn bin_attr_values<'a>(
    map: &'a HashMap<String, Vec<Vec<u8>>>,
    name: &str,
) -> Option<&'a Vec<Vec<u8>>> {
    map.iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(name))
        .map(|(_, v)| v)
}

/// Read the immutable entry identifier, whichever side of the entry it arrived on.
///
/// Three ways this silently returns nothing, all of which must be a hard failure upstream rather
/// than an empty subject:
/// * `entryUUID` is an **operational** attribute — it is not returned by `*`, so it has to be named
///   in the requested attribute list (this module does).
/// * `objectGUID` is **binary** and lands in `bin_attrs`, not `attrs`.
/// * an entry may simply not have the configured attribute at all.
///
/// A stored empty subject would be worse than a refusal: `""` is not NULL, so the partial unique
/// index lets the *first* such account own it and every later one collide — and if that index were
/// ever dropped, every directory user would collapse onto one account.
///
/// The binary encoding is lowercase hex of the raw bytes, treated as an opaque key. It is
/// deliberately **not** AD's mixed-endian display form: the value is never shown to anyone, and
/// changing the encoding later would orphan every account provisioned under the old one.
#[must_use]
pub fn format_subject(entry: &SearchEntry, uid_attribute: &str) -> Option<String> {
    if let Some(v) = attr_values(&entry.attrs, uid_attribute) {
        if let Some(first) = v.first().map(|s| s.trim()).filter(|s| !s.is_empty()) {
            return Some(first.to_owned());
        }
    }
    let bytes = bin_attr_values(&entry.bin_attrs, uid_attribute)?
        .first()
        .filter(|b| !b.is_empty())?;
    Some(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

/// Read a single-valued text attribute.
#[must_use]
pub fn first_text(entry: &SearchEntry, attribute: &str) -> Option<String> {
    attr_values(&entry.attrs, attribute)?
        .first()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

/// Why a set of search results is not a single usable entry.
#[derive(Debug, PartialEq, Eq)]
pub enum EntryProblem {
    /// Nobody matched — the directory's answer to "who is this", and it is a denial.
    NotFound,
    /// More than one matched. Never "take the first": which entry sorts first is not a security
    /// decision anyone made, and the filter is what is wrong.
    Ambiguous,
    /// A result with no DN. AD returns continuation references from a subtree search that crosses a
    /// naming context, and constructing an entry from one yields an empty DN — binding to which is
    /// an **anonymous** bind that many servers answer `success`.
    NoDn,
}

/// Require exactly one usable entry.
///
/// # Errors
/// See [`EntryProblem`]. The caller maps `NotFound` to a denial and the other two to unavailable:
/// an ambiguous filter or a referral is the configuration's fault, not the person's, so it must not
/// arm their lockout counter.
pub fn pick_one_entry(entries: Vec<SearchEntry>) -> Result<SearchEntry, EntryProblem> {
    let total = entries.len();
    let mut usable = entries
        .into_iter()
        .filter(|e| !e.dn.trim().is_empty())
        .peekable();
    let first = usable.next();
    match (first, usable.next()) {
        (Some(_), Some(_)) => Err(EntryProblem::Ambiguous),
        (Some(e), None) => Ok(e),
        // Results came back but every one was a continuation reference, which is a broken search
        // rather than an absent person — and must not arm their lockout counter.
        (None, _) if total > 0 => Err(EntryProblem::NoDn),
        (None, _) => Err(EntryProblem::NotFound),
    }
}

fn clip(s: &str, max: usize) -> String {
    s.chars().take(max).collect()
}

// ── Store ───────────────────────────────────────────────────────────────────────────────────────

/// PostgreSQL-backed store for the single directory configuration.
pub struct LdapRepo {
    pool: PgPool,
    cipher: EnvelopeCipher<Kek>,
}

impl LdapRepo {
    #[must_use]
    pub fn new(pool: PgPool, kek: Kek) -> Self {
        Self {
            pool,
            cipher: EnvelopeCipher::new(kek),
        }
    }

    /// The configuration as the Settings page shows it. `None` when nothing is configured, which is
    /// the default state of every installation.
    pub async fn view(&self) -> anyhow::Result<Option<LdapConfigView>> {
        let Some(row) = sqlx::query(VIEW_SQL).fetch_optional(&self.pool).await? else {
            return Ok(None);
        };
        let port: i32 = row.try_get("port")?;
        let security: String = row.try_get("security")?;
        let role_map_raw: serde_json::Value = row.try_get("role_map")?;
        let updated_at: chrono::DateTime<chrono::Utc> = row.try_get("updated_at")?;
        Ok(Some(LdapConfigView {
            host: row.try_get("host")?,
            port: u16::try_from(port).unwrap_or(636),
            security: LdapSecurity::parse(&security).unwrap_or(LdapSecurity::Ldaps),
            ca_cert: row.try_get("ca_cert")?,
            bind_dn: row.try_get("bind_dn")?,
            has_bind_password: row.try_get("has_bind_password")?,
            user_base_dn: row.try_get("user_base_dn")?,
            user_filter: row.try_get("user_filter")?,
            username_attribute: row.try_get("username_attribute")?,
            uid_attribute: row.try_get("uid_attribute")?,
            member_of_attribute: row.try_get("member_of_attribute")?,
            group_base_dn: row.try_get("group_base_dn")?,
            group_filter: row.try_get("group_filter")?,
            group_name_attribute: row.try_get("group_name_attribute")?,
            role_map: serde_json::from_value(role_map_raw).unwrap_or_default(),
            default_role: row.try_get("default_role")?,
            enabled: row.try_get("enabled")?,
            updated_at: updated_at.to_rfc3339(),
        }))
    }

    /// The configuration to authenticate against, or `None` when directory login is off.
    ///
    /// `enabled = false` is treated exactly like no row: an operator who unticks the box has turned
    /// the feature off, and a stored credential must not keep it half-working.
    ///
    /// Read on every login and deliberately **not cached**. A cached copy would mean unticking the
    /// box does not take effect until a restart — the same class of bug as the disabled-SSO-account
    /// one the OIDC path had to fix, where the control existed and did nothing.
    pub async fn enabled_config(&self) -> anyhow::Result<Option<LdapConfig>> {
        self.load(true).await
    }

    /// The configuration **ignoring `enabled`**, for the Test button — validating a directory before
    /// switching it on is the whole point of that button (`RcaRepo::configured`'s reasoning).
    pub async fn configured(&self) -> anyhow::Result<Option<LdapConfig>> {
        self.load(false).await
    }

    async fn load(&self, require_enabled: bool) -> anyhow::Result<Option<LdapConfig>> {
        let Some(row) = sqlx::query(LOAD_SQL).fetch_optional(&self.pool).await? else {
            return Ok(None);
        };
        if require_enabled && !row.try_get::<bool, _>("enabled")? {
            return Ok(None);
        }
        let key_id: i32 = row.try_get("key_id")?;
        let sealed = SealedSecret {
            key_id: u32::try_from(key_id).unwrap_or(0),
            wrapped_dek: row.try_get("wrapped_dek")?,
            dek_nonce: row.try_get("dek_nonce")?,
            ciphertext: row.try_get("ciphertext")?,
            ct_nonce: row.try_get("ct_nonce")?,
        };
        let bind_password = String::from_utf8(
            self.cipher
                .open(&sealed)
                .map_err(|e| anyhow::anyhow!("open the LDAP bind password: {e}"))?,
        )
        .map_err(|_| anyhow::anyhow!("the LDAP bind password is not valid UTF-8"))?;

        let port: i32 = row.try_get("port")?;
        let security: String = row.try_get("security")?;
        let role_map_raw: HashMap<String, String> =
            serde_json::from_value(row.try_get("role_map")?).unwrap_or_default();
        let default_role = row
            .try_get::<Option<String>, _>("default_role")?
            .and_then(|s| Role::parse_lenient(&s));
        Ok(Some(LdapConfig {
            host: row.try_get("host")?,
            port: u16::try_from(port).unwrap_or(636),
            security: LdapSecurity::parse(&security).unwrap_or(LdapSecurity::Ldaps),
            ca_cert: row.try_get("ca_cert")?,
            bind_dn: row.try_get("bind_dn")?,
            bind_password,
            user_base_dn: row.try_get("user_base_dn")?,
            user_filter: row.try_get("user_filter")?,
            username_attribute: row.try_get("username_attribute")?,
            uid_attribute: row.try_get("uid_attribute")?,
            member_of_attribute: row.try_get("member_of_attribute")?,
            group_base_dn: row.try_get("group_base_dn")?,
            group_filter: row.try_get("group_filter")?,
            group_name_attribute: row.try_get("group_name_attribute")?,
            role_map: fold_role_map(&role_map_raw),
            default_role,
        }))
    }

    /// Whether directory login is switched on, for the unauthenticated client bootstrap.
    pub async fn is_enabled(&self) -> anyhow::Result<bool> {
        Ok(
            sqlx::query_scalar::<_, bool>("SELECT enabled FROM ldap_config WHERE id = 1")
                .fetch_optional(&self.pool)
                .await?
                .unwrap_or(false),
        )
    }

    /// Every account provisioned from the directory, so switching it off can revoke their sessions.
    pub async fn account_ids(&self) -> anyhow::Result<Vec<Uuid>> {
        Ok(
            sqlx::query_scalar::<_, Uuid>("SELECT id FROM users WHERE auth_source = 'ldap'")
                .fetch_all(&self.pool)
                .await?,
        )
    }

    /// Create or replace the single configuration row.
    ///
    /// # Errors
    /// Operator-facing text for a malformed input (surfaced as a typed 400), or the database error.
    pub async fn save(&self, input: &LdapConfigInput) -> anyhow::Result<()> {
        validate(input).map_err(|e| anyhow::anyhow!(e))?;

        // Two-valued, unlike `llm_config`'s three: there is no "clear the credential" state, because
        // a blank bind password is an anonymous search rather than an absent one.
        let sealed = match input.bind_password.as_deref() {
            Some(p) if !p.trim().is_empty() => Some(
                self.cipher
                    .seal(p.as_bytes())
                    .map_err(|e| anyhow::anyhow!("seal the LDAP bind password: {e}"))?,
            ),
            _ => None,
        };
        if sealed.is_none() {
            // Keeping the stored password only means something when there is one. A first save with
            // no password would otherwise violate NOT NULL and surface as a 500 for what is really
            // a missing required field.
            let exists: bool =
                sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM ldap_config WHERE id = 1)")
                    .fetch_one(&self.pool)
                    .await?;
            if !exists {
                anyhow::bail!("a bind password is required the first time the directory is saved");
            }
        }
        let keep = sealed.is_none();
        let (key_id, wrapped_dek, dek_nonce, ciphertext, ct_nonce) = match &sealed {
            Some(s) => (
                Some(i32::try_from(s.key_id).unwrap_or(0)),
                Some(s.wrapped_dek.clone()),
                Some(s.dek_nonce.clone()),
                Some(s.ciphertext.clone()),
                Some(s.ct_nonce.clone()),
            ),
            None => (None, None, None, None, None),
        };

        sqlx::query(SAVE_SQL)
            .bind(input.host.trim())
            .bind(i32::from(input.port))
            .bind(input.security.as_str())
            .bind(
                input
                    .ca_cert
                    .as_deref()
                    .map(str::trim)
                    .filter(|s| !s.is_empty()),
            )
            .bind(input.bind_dn.trim())
            .bind(key_id)
            .bind(wrapped_dek)
            .bind(dek_nonce)
            .bind(ciphertext)
            .bind(ct_nonce)
            .bind(input.user_base_dn.trim())
            .bind(input.user_filter.trim())
            .bind(input.username_attribute.trim())
            .bind(input.uid_attribute.trim())
            .bind(input.member_of_attribute.trim())
            .bind(
                input
                    .group_base_dn
                    .as_deref()
                    .map(str::trim)
                    .filter(|s| !s.is_empty()),
            )
            .bind(
                input
                    .group_filter
                    .as_deref()
                    .map(str::trim)
                    .filter(|s| !s.is_empty()),
            )
            .bind(input.group_name_attribute.trim())
            .bind(serde_json::to_value(&input.role_map)?)
            .bind(input.default_role.as_deref())
            .bind(input.enabled)
            .bind(keep)
            .execute(&self.pool)
            .await?;
        Ok(())
    }
}

const VIEW_SQL: &str = "SELECT host, port, security, ca_cert, bind_dn, \
     ciphertext IS NOT NULL AS has_bind_password, user_base_dn, user_filter, username_attribute, \
     uid_attribute, member_of_attribute, group_base_dn, group_filter, group_name_attribute, \
     role_map, default_role, enabled, updated_at \
     FROM ldap_config WHERE id = 1";

const LOAD_SQL: &str = "SELECT host, port, security, ca_cert, bind_dn, \
     key_id, wrapped_dek, dek_nonce, ciphertext, ct_nonce, \
     user_base_dn, user_filter, username_attribute, uid_attribute, member_of_attribute, \
     group_base_dn, group_filter, group_name_attribute, role_map, default_role, enabled \
     FROM ldap_config WHERE id = 1";

/// The single-statement upsert behind [`LdapRepo::save`].
///
/// A named constant so the keep-the-stored-secret semantics are assertable without a database — an
/// in-memory fake cannot catch a mistake in SQL meaning, and this clause is what decides whether
/// editing the base DN silently wipes the bind password.
const SAVE_SQL: &str = "INSERT INTO ldap_config \
     (id, host, port, security, ca_cert, bind_dn, key_id, wrapped_dek, dek_nonce, ciphertext, \
      ct_nonce, user_base_dn, user_filter, username_attribute, uid_attribute, \
      member_of_attribute, group_base_dn, group_filter, group_name_attribute, role_map, \
      default_role, enabled, updated_at) \
     VALUES (1,$1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17,$18,$19,$20,$21, now()) \
     ON CONFLICT (id) DO UPDATE SET \
       host = EXCLUDED.host, port = EXCLUDED.port, security = EXCLUDED.security, \
       ca_cert = EXCLUDED.ca_cert, bind_dn = EXCLUDED.bind_dn, \
       key_id      = CASE WHEN $22 THEN ldap_config.key_id      ELSE EXCLUDED.key_id      END, \
       wrapped_dek = CASE WHEN $22 THEN ldap_config.wrapped_dek ELSE EXCLUDED.wrapped_dek END, \
       dek_nonce   = CASE WHEN $22 THEN ldap_config.dek_nonce   ELSE EXCLUDED.dek_nonce   END, \
       ciphertext  = CASE WHEN $22 THEN ldap_config.ciphertext  ELSE EXCLUDED.ciphertext  END, \
       ct_nonce    = CASE WHEN $22 THEN ldap_config.ct_nonce    ELSE EXCLUDED.ct_nonce    END, \
       user_base_dn = EXCLUDED.user_base_dn, user_filter = EXCLUDED.user_filter, \
       username_attribute = EXCLUDED.username_attribute, uid_attribute = EXCLUDED.uid_attribute, \
       member_of_attribute = EXCLUDED.member_of_attribute, \
       group_base_dn = EXCLUDED.group_base_dn, group_filter = EXCLUDED.group_filter, \
       group_name_attribute = EXCLUDED.group_name_attribute, \
       role_map = EXCLUDED.role_map, default_role = EXCLUDED.default_role, \
       enabled = EXCLUDED.enabled, updated_at = now()";

/// Validate a save payload, returning operator-facing text.
///
/// # Errors
/// One sentence naming the field. Every rule here is also a database CHECK, but this is the primary
/// guard: a constraint violation would surface as a 500 for what is a correctable mistake.
pub fn validate(input: &LdapConfigInput) -> Result<(), String> {
    if input.host.trim().is_empty() {
        return Err("host is required".to_owned());
    }
    if input.port == 0 {
        return Err("port must be between 1 and 65535".to_owned());
    }
    if input.bind_dn.trim().is_empty() {
        return Err("the service account's bind DN is required".to_owned());
    }
    if let Some(p) = input.bind_password.as_deref() {
        if p.trim().is_empty() {
            // Not "clear the password": an empty one is an unauthenticated bind, so accepting it
            // would silently turn the search anonymous.
            return Err(
                "the bind password cannot be blank — leave the field untouched to keep the stored one"
                    .to_owned(),
            );
        }
    }
    if input.user_base_dn.trim().is_empty() {
        return Err("the user base DN is required".to_owned());
    }
    render_filter(input.user_filter.trim(), "probe")?;
    for (field, value) in [
        ("the username attribute", &input.username_attribute),
        ("the id attribute", &input.uid_attribute),
        ("the group membership attribute", &input.member_of_attribute),
        ("the group name attribute", &input.group_name_attribute),
    ] {
        if value.trim().is_empty() {
            return Err(format!("{field} is required"));
        }
    }
    let has_base = input
        .group_base_dn
        .as_deref()
        .is_some_and(|s| !s.trim().is_empty());
    let has_filter = input
        .group_filter
        .as_deref()
        .is_some_and(|s| !s.trim().is_empty());
    if has_base != has_filter {
        return Err(
            "a group search needs both a group base DN and a group filter, or neither".to_owned(),
        );
    }
    if let Some(f) = input.group_filter.as_deref().filter(|_| has_filter) {
        if !f.contains("{user_dn}") && !f.contains("{username}") {
            return Err(
                "the group filter must contain {user_dn} (or {username}) so it matches one person"
                    .to_owned(),
            );
        }
    }
    for (group, role) in &input.role_map {
        if Role::parse_lenient(role).is_none() {
            return Err(format!("role_map[{group}] has unknown role '{role}'"));
        }
    }
    if let Some(r) = input
        .default_role
        .as_deref()
        .filter(|s| !s.trim().is_empty())
    {
        if Role::parse_lenient(r).is_none() {
            return Err(format!("unknown default_role '{r}'"));
        }
    }
    let has_default = input
        .default_role
        .as_deref()
        .is_some_and(|s| !s.trim().is_empty());
    if input.enabled && input.role_map.is_empty() && !has_default {
        // Turning a silent runtime denial into a save-time error. With no mapping and no default,
        // `resolve_role` returns `None` for everyone, so every login is refused with the same
        // generic 401 a wrong password gets — indistinguishable, and the configuration looks fine.
        return Err(
            "enabling the directory needs at least one group→role mapping, or a default role — \
             otherwise every login is denied"
                .to_owned(),
        );
    }
    Ok(())
}

// ── Network ─────────────────────────────────────────────────────────────────────────────────────

/// A live connection plus the task driving it, so the task can be aborted on every exit path.
struct Conn {
    ldap: ldap3::Ldap,
    driver: tokio::task::JoinHandle<()>,
}

impl Conn {
    async fn open(cfg: &LdapConfig) -> Result<Self, String> {
        let tls = crate::tls::client_config(cfg.ca_cert.as_deref())?;
        let settings = LdapConnSettings::new()
            .set_conn_timeout(CONNECT_TIMEOUT)
            .set_starttls(cfg.security.starttls())
            .set_config(tls);
        let (conn, mut ldap) = LdapConnAsync::with_settings(settings, &cfg.url())
            .await
            .map_err(|e| format!("connect: {e}"))?;
        let driver = tokio::spawn(async move {
            if let Err(e) = conn.drive().await {
                tracing::debug!(error = %e, "LDAP connection driver ended");
            }
        });
        ldap.with_timeout(OP_TIMEOUT);
        Ok(Self { ldap, driver })
    }

    /// Close politely and stop the driver. Called on success; `Drop` covers every other path.
    async fn close(mut self) {
        let _ = self.ldap.unbind().await;
        self.driver.abort();
        std::mem::forget(self);
    }
}

impl Drop for Conn {
    fn drop(&mut self) {
        // coding-conventions: a spawned task must not be leaked on a timeout or an early return.
        self.driver.abort();
    }
}

/// What the shared search leg found.
struct Found {
    identity: DirectoryIdentity,
    entry_dn: String,
}

/// Bind as the service account, find the user, and collect their groups.
///
/// Shared by [`authenticate`] and [`probe`] so the Test button cannot become a second, unguarded
/// implementation of the escaping, the exactly-one-entry rule and the group expansion.
async fn bind_and_find(
    cfg: &LdapConfig,
    typed_username: &str,
    stages: &mut Vec<LdapTestStage>,
) -> Result<Found, LdapAuth> {
    let mut service = match Conn::open(cfg).await {
        Ok(c) => {
            stages.push(stage("connect", true, None));
            c
        }
        Err(e) => {
            stages.push(stage("connect", false, Some(&e)));
            return Err(LdapAuth::Unavailable(e));
        }
    };

    // A wrong service password is `Unavailable`, never `Denied`: it is the operator's mistake and
    // must not arm the lockout counter of every person who tries to sign in.
    if let Err(e) = service
        .ldap
        .simple_bind(&cfg.bind_dn, &cfg.bind_password)
        .await
        .map_err(|e| e.to_string())
        .and_then(|r| r.success().map_err(|e| e.to_string()))
    {
        stages.push(stage("bind_service", false, Some(&e)));
        return Err(LdapAuth::Unavailable(format!("service bind: {e}")));
    }
    stages.push(stage("bind_service", true, None));

    let filter = match render_filter(&cfg.user_filter, &escape_filter(typed_username)) {
        Ok(f) => f,
        Err(e) => {
            stages.push(stage("search_user", false, Some(&e)));
            return Err(LdapAuth::Unavailable(e));
        }
    };
    // The id attribute must be named explicitly — `entryUUID` is operational and `*` does not
    // return it. `sizelimit(2)` is enough to detect ambiguity without dragging back an OU.
    let attrs = [
        cfg.uid_attribute.as_str(),
        cfg.username_attribute.as_str(),
        cfg.member_of_attribute.as_str(),
    ];
    service.ldap.with_search_options(
        SearchOptions::new()
            .sizelimit(2)
            .timelimit(i32::try_from(OP_TIMEOUT.as_secs()).unwrap_or(10)),
    );
    let search = service
        .ldap
        .search(&cfg.user_base_dn, Scope::Subtree, &filter, attrs)
        .await
        .map_err(|e| e.to_string())
        .and_then(|r| r.success().map_err(|e| e.to_string()));
    let entries = match search {
        Ok((entries, _)) => entries,
        Err(e) => {
            stages.push(stage("search_user", false, Some(&e)));
            return Err(LdapAuth::Unavailable(format!("user search: {e}")));
        }
    };
    let entry = match pick_one_entry(entries.into_iter().map(SearchEntry::construct).collect()) {
        Ok(e) => e,
        Err(EntryProblem::NotFound) => {
            stages.push(stage("search_user", false, Some("no matching entry")));
            return Err(LdapAuth::Denied("no such user"));
        }
        Err(EntryProblem::Ambiguous) => {
            let d = "the user filter matched more than one entry";
            stages.push(stage("search_user", false, Some(d)));
            return Err(LdapAuth::Unavailable(d.to_owned()));
        }
        Err(EntryProblem::NoDn) => {
            let d = "the directory returned a referral continuation, not an entry";
            stages.push(stage("search_user", false, Some(d)));
            return Err(LdapAuth::Unavailable(d.to_owned()));
        }
    };

    let Some(subject) = format_subject(&entry, &cfg.uid_attribute) else {
        let d = format!(
            "the entry has no {} attribute; an account cannot be identified without it",
            cfg.uid_attribute
        );
        stages.push(stage("search_user", false, Some(&d)));
        return Err(LdapAuth::Unavailable(d));
    };
    // The canonical name comes from the directory. AD matches `sAMAccountName` case-insensitively
    // while `users.username` is compared case-sensitively, so storing what was typed produces a
    // second account the first time somebody capitalises their own name.
    let Some(username) = first_text(&entry, &cfg.username_attribute) else {
        let d = format!("the entry has no {} attribute", cfg.username_attribute);
        stages.push(stage("search_user", false, Some(&d)));
        return Err(LdapAuth::Unavailable(d));
    };
    stages.push(stage("search_user", true, Some(&entry.dn)));

    let mut groups: Vec<String> = attr_values(&entry.attrs, &cfg.member_of_attribute)
        .cloned()
        .unwrap_or_default();

    // The optional group search, on the **service** connection. On AD this is also the only way to
    // see nested groups: `memberOf` is not transitive there.
    if let (Some(base), Some(template)) =
        (cfg.group_base_dn.as_deref(), cfg.group_filter.as_deref())
    {
        let gf = template
            .replace("{user_dn}", &escape_filter(&entry.dn))
            .replace("{username}", &escape_filter(&username));
        match service
            .ldap
            .search(
                base,
                Scope::Subtree,
                &gf,
                [cfg.group_name_attribute.as_str()],
            )
            .await
            .map_err(|e| e.to_string())
            .and_then(|r| r.success().map_err(|e| e.to_string()))
        {
            Ok((found, _)) => {
                for g in found.into_iter().map(SearchEntry::construct) {
                    if let Some(name) = first_text(&g, &cfg.group_name_attribute) {
                        groups.push(name);
                    } else if !g.dn.trim().is_empty() {
                        groups.push(g.dn.clone());
                    }
                }
                stages.push(stage("resolve_groups", true, None));
            }
            Err(e) => {
                // Not fatal on its own: `memberOf` may already have answered. But it must be
                // visible, because a silently empty group set degrades to `default_role`.
                stages.push(stage("resolve_groups", false, Some(&e)));
                tracing::warn!(error = %e, "LDAP group search failed; using memberOf only");
            }
        }
    } else {
        stages.push(stage("resolve_groups", true, None));
    }

    service.close().await;
    Ok(Found {
        identity: DirectoryIdentity {
            subject,
            username,
            dn: entry.dn.clone(),
            groups,
        },
        entry_dn: entry.dn,
    })
}

fn stage(name: &str, ok: bool, detail: Option<&str>) -> LdapTestStage {
    LdapTestStage {
        name: name.to_owned(),
        ok,
        detail: detail.map(|d| clip(d, STAGE_DETAIL_MAX_CHARS)),
    }
}

/// Resolve the role a set of directory groups maps to, via the shared OIDC mechanism.
#[must_use]
pub fn role_for_groups(groups: &[String], cfg: &LdapConfig) -> Option<Role> {
    let candidates: Vec<String> = groups.iter().flat_map(|g| group_role_keys(g)).collect();
    crate::oidc::resolve_role(&candidates, &cfg.role_map, cfg.default_role)
}

/// Authenticate a username/password against the directory.
///
/// Never logs either credential. The user's password reaches exactly one place — `simple_bind`.
pub async fn authenticate(cfg: &LdapConfig, typed_username: &str, password: &str) -> LdapAuth {
    if !password_is_bindable(password) {
        // Before the semaphore and before any socket, so it cannot be reached around.
        metrics::counter!("yagra_ldap_bind_total", "stage" => "precheck", "result" => "denied")
            .increment(1);
        return LdapAuth::Denied("empty password");
    }
    let Ok(_slot) = BIND_SLOTS.try_acquire() else {
        metrics::counter!("yagra_ldap_bind_total", "stage" => "precheck", "result" => "unavailable")
            .increment(1);
        return LdapAuth::Unavailable("too many concurrent directory binds".to_owned());
    };
    let started = std::time::Instant::now();
    let outcome = match tokio::time::timeout(
        OVERALL_TIMEOUT,
        authenticate_inner(cfg, typed_username, password),
    )
    .await
    {
        Ok(o) => o,
        Err(_) => LdapAuth::Unavailable("the directory did not answer in time".to_owned()),
    };
    metrics::histogram!("yagra_ldap_bind_duration_seconds").record(started.elapsed().as_secs_f64());
    let result = match &outcome {
        LdapAuth::Ok(..) => "ok",
        LdapAuth::Denied(_) => "denied",
        LdapAuth::Unavailable(_) => "unavailable",
    };
    metrics::counter!("yagra_ldap_bind_total", "stage" => "login", "result" => result).increment(1);
    outcome
}

async fn authenticate_inner(cfg: &LdapConfig, typed_username: &str, password: &str) -> LdapAuth {
    let mut stages = Vec::new();
    let found = match bind_and_find(cfg, typed_username, &mut stages).await {
        Ok(f) => f,
        Err(e) => return e,
    };

    // A **second, independent** connection for the user bind. Rebinding the service connection
    // would run any later search with the user's own rights and leave a failed bind sitting
    // anonymous; it also makes the handle unshareable across concurrent logins.
    let mut user = match Conn::open(cfg).await {
        Ok(c) => c,
        Err(e) => return LdapAuth::Unavailable(e),
    };
    match user.ldap.simple_bind(&found.entry_dn, password).await {
        Ok(r) if r.rc == 0 => {}
        Ok(_) => {
            user.close().await;
            return LdapAuth::Denied("the directory rejected the password");
        }
        Err(e) => {
            tracing::debug!(error = %e, "LDAP user bind failed");
            return LdapAuth::Unavailable(format!("user bind: {e}"));
        }
    }
    user.close().await;

    match role_for_groups(&found.identity.groups, cfg) {
        Some(role) => LdapAuth::Ok(Box::new(found.identity), role),
        None => LdapAuth::Denied("no directory group maps to a Yagra role"),
    }
}

/// Exercise the stored configuration for the Settings page's Test button.
///
/// **Never binds as the user, and takes no password.** Two reasons, both deliberate: an admin must
/// not be able to use Yagra as a credential-testing proxy into the directory (where the domain
/// controller's own log would show Yagra's service account rather than them), and a probe must not
/// be able to push anybody towards their domain's lockout threshold. The cost is that this cannot
/// prove login works, which is why the result names its stages instead of answering `ok` alone.
pub async fn probe(cfg: &LdapConfig, username: Option<&str>) -> LdapTestResult {
    const NOTE: &str = "The user's own bind is not exercised by this check — it verifies the \
                        connection, the service account, the search and the role mapping.";
    let mut stages = Vec::new();
    // With no username there is nothing to search for, so probe the connection and service bind
    // with a filter that matches the base entry itself as cheaply as possible.
    let probe_name = username.unwrap_or("\u{0}yagra-nonexistent-probe");
    let found =
        tokio::time::timeout(OVERALL_TIMEOUT, bind_and_find(cfg, probe_name, &mut stages)).await;
    let found = match found {
        Ok(r) => r,
        Err(_) => {
            stages.push(stage(
                "connect",
                false,
                Some("the directory did not answer in time"),
            ));
            return LdapTestResult {
                ok: false,
                stages,
                user_dn: None,
                subject_present: None,
                username_resolved: None,
                groups: Vec::new(),
                groups_truncated: false,
                role: None,
                note: NOTE.to_owned(),
            };
        }
    };
    match found {
        Ok(found) => {
            let role = role_for_groups(&found.identity.groups, cfg);
            let truncated = found.identity.groups.len() > MAX_GROUPS_REPORTED;
            let groups: Vec<String> = found
                .identity
                .groups
                .iter()
                .take(MAX_GROUPS_REPORTED)
                .cloned()
                .collect();
            LdapTestResult {
                ok: stages.iter().all(|s| s.ok),
                stages,
                user_dn: Some(found.identity.dn),
                subject_present: Some(true),
                username_resolved: Some(found.identity.username),
                groups,
                groups_truncated: truncated,
                role: role.map(|r| r.key().to_owned()),
                note: NOTE.to_owned(),
            }
        }
        Err(outcome) => {
            // A "no such user" answer with no username asked for is the expected shape: it proves
            // the connection, the service bind and the search all worked.
            let ok = username.is_none() && matches!(outcome, LdapAuth::Denied("no such user"));
            LdapTestResult {
                ok,
                stages,
                user_dn: None,
                subject_present: None,
                username_resolved: None,
                groups: Vec::new(),
                groups_truncated: false,
                role: None,
                note: NOTE.to_owned(),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(dn: &str, attrs: &[(&str, &[&str])], bin: &[(&str, &[u8])]) -> SearchEntry {
        SearchEntry {
            dn: dn.to_owned(),
            attrs: attrs
                .iter()
                .map(|(k, v)| {
                    (
                        (*k).to_owned(),
                        v.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>(),
                    )
                })
                .collect(),
            bin_attrs: bin
                .iter()
                .map(|(k, v)| ((*k).to_owned(), vec![v.to_vec()]))
                .collect(),
        }
    }

    fn input() -> LdapConfigInput {
        LdapConfigInput {
            host: "dc1.corp.example.com".to_owned(),
            port: 636,
            security: LdapSecurity::Ldaps,
            ca_cert: None,
            bind_dn: "CN=svc,DC=corp,DC=example,DC=com".to_owned(),
            bind_password: Some("s3cret".to_owned()),
            user_base_dn: "DC=corp,DC=example,DC=com".to_owned(),
            user_filter: default_user_filter(),
            username_attribute: default_username_attribute(),
            uid_attribute: default_uid_attribute(),
            member_of_attribute: default_member_of_attribute(),
            group_base_dn: None,
            group_filter: None,
            group_name_attribute: default_group_name_attribute(),
            role_map: HashMap::from([("NetOps".to_owned(), "admin".to_owned())]),
            default_role: None,
            enabled: true,
        }
    }

    fn cfg(role_map: &[(&str, Role)], default_role: Option<Role>) -> LdapConfig {
        LdapConfig {
            host: "dc1".to_owned(),
            port: 636,
            security: LdapSecurity::Ldaps,
            ca_cert: None,
            bind_dn: "CN=svc".to_owned(),
            bind_password: "x".to_owned(),
            user_base_dn: "DC=x".to_owned(),
            user_filter: default_user_filter(),
            username_attribute: default_username_attribute(),
            uid_attribute: default_uid_attribute(),
            member_of_attribute: default_member_of_attribute(),
            group_base_dn: None,
            group_filter: None,
            group_name_attribute: default_group_name_attribute(),
            role_map: role_map
                .iter()
                .map(|(g, r)| ((*g).to_ascii_lowercase(), *r))
                .collect(),
            default_role,
        }
    }

    // ── Filter injection ────────────────────────────────────────────────────────────────────────

    // The classic LDAP injection: a username that closes the filter's parenthesis and opens its own
    // clause. `ldap3` does not escape for us, so this is the only thing standing between a login
    // form and an attacker-chosen filter.
    #[test]
    fn a_filter_metacharacter_cannot_escape_the_username_slot() {
        let template = "(&(objectClass=user)(sAMAccountName={username}))";
        let expected_parens = template.matches('(').count();
        for hostile in [
            "*)(uid=*",
            "*",
            "admin)(|(uid=*",
            "a\\b",
            "a\0b",
            "()",
            "山田*",
        ] {
            let rendered = render_filter(template, &escape_filter(hostile)).expect("renders");
            assert_eq!(
                rendered.matches('(').count(),
                expected_parens,
                "`{hostile}` changed the filter's structure: {rendered}"
            );
            assert_eq!(rendered.matches(')').count(), expected_parens);
        }
    }

    #[test]
    fn rendering_a_filter_without_the_placeholder_is_refused() {
        // `(&(objectClass=user))` matches the whole subtree. Refusing it here is one of the three
        // gates; the others are `validate` and the exactly-one-entry rule.
        let err = render_filter("(&(objectClass=user))", "alice").expect_err("must refuse");
        assert!(err.contains("{username}"), "{err}");
    }

    // ── The unauthenticated-bind gate ───────────────────────────────────────────────────────────

    #[test]
    fn an_empty_or_blank_password_is_not_bindable() {
        for blank in ["", " ", "\t", "\n", "   \r\n"] {
            assert!(
                !password_is_bindable(blank),
                "{blank:?} would have been sent as an unauthenticated bind"
            );
        }
        assert!(password_is_bindable("x"));
        // Leading/trailing space is meaningful *inside* a real password; only all-blank is refused.
        assert!(password_is_bindable(" p "));
    }

    #[test]
    fn a_blank_bind_password_is_a_validation_error_not_a_clear() {
        let mut i = input();
        i.bind_password = Some("   ".to_owned());
        let err = validate(&i).expect_err("a blank bind password must be refused");
        assert!(err.contains("blank"), "{err}");
    }

    // ── Entry selection ─────────────────────────────────────────────────────────────────────────

    #[test]
    fn more_than_one_matching_entry_is_a_failure_not_a_first_match() {
        let e = vec![entry("CN=a", &[], &[]), entry("CN=b", &[], &[])];
        assert!(
            matches!(pick_one_entry(e), Err(EntryProblem::Ambiguous)),
            "two matching entries must be a failure, never a first-match"
        );
    }

    #[test]
    fn a_result_with_an_empty_dn_is_not_a_usable_entry() {
        // A referral continuation constructs to an empty DN, and `simple_bind("", pw)` is an
        // anonymous bind that many servers answer `success`.
        // A referral-only answer is `NoDn`, not `NotFound`: the search is broken, the person is not
        // absent, and only one of those may count against their lockout.
        assert!(
            matches!(
                pick_one_entry(vec![entry("   ", &[], &[])]),
                Err(EntryProblem::NoDn)
            ),
            "a referral continuation must not be usable as an entry"
        );
        let one = pick_one_entry(vec![entry("", &[], &[]), entry("CN=real", &[], &[])])
            .expect("the real entry survives");
        assert_eq!(one.dn, "CN=real");
    }

    #[test]
    fn no_entries_is_a_denial_not_an_error() {
        assert!(
            matches!(pick_one_entry(Vec::new()), Err(EntryProblem::NotFound)),
            "no entries at all is a denial"
        );
    }

    // ── The identity attribute ──────────────────────────────────────────────────────────────────

    #[test]
    fn a_binary_object_guid_is_read_from_the_binary_side() {
        // AD returns objectGUID in `bin_attrs`; reading `attrs` alone yields None and would refuse
        // every login against a working directory.
        let e = entry("CN=a", &[], &[("objectGUID", &[0x01, 0xab, 0xff])]);
        assert_eq!(format_subject(&e, "objectGUID").as_deref(), Some("01abff"));
    }

    #[test]
    fn a_text_entry_uuid_is_read_from_the_text_side() {
        let e = entry("CN=a", &[("entryUUID", &["7d3c-…"])], &[]);
        assert_eq!(format_subject(&e, "entryUUID").as_deref(), Some("7d3c-…"));
    }

    #[test]
    fn an_absent_or_empty_identity_attribute_yields_no_subject() {
        // Must be None, never Some(""): "" is not NULL, so the partial unique index would let the
        // first such account own it and collide with every later one.
        assert_eq!(format_subject(&entry("CN=a", &[], &[]), "objectGUID"), None);
        let blank = entry("CN=a", &[("entryUUID", &["  "])], &[]);
        assert_eq!(format_subject(&blank, "entryUUID"), None);
        let empty_bin = entry("CN=a", &[], &[("objectGUID", &[])]);
        assert_eq!(format_subject(&empty_bin, "objectGUID"), None);
    }

    #[test]
    fn attribute_names_are_matched_case_insensitively() {
        // LDAP attribute names are case-insensitive and servers echo their own casing. An exact
        // lookup would read `samaccountname` as "absent".
        let e = entry("CN=a", &[("samaccountname", &["alice"])], &[]);
        assert_eq!(first_text(&e, "sAMAccountName").as_deref(), Some("alice"));
    }

    // ── DN / group matching ─────────────────────────────────────────────────────────────────────

    #[test]
    fn an_escaped_comma_inside_a_cn_does_not_split_the_rdn() {
        let (attr, value) = parse_first_rdn("CN=Net\\,Ops,OU=Groups,DC=x").expect("parses");
        assert_eq!(attr, "CN");
        assert_eq!(value, "Net\\,Ops");
    }

    #[test]
    fn a_group_dn_matches_both_its_full_dn_and_its_cn() {
        let keys = group_role_keys("CN=NetOps,OU=Groups,DC=corp,DC=example,DC=com");
        assert!(keys.contains(&"cn=netops,ou=groups,dc=corp,dc=example,dc=com".to_owned()));
        assert!(keys.contains(&"netops".to_owned()));
    }

    #[test]
    fn a_dn_with_spaces_after_commas_matches_the_compact_form() {
        assert_eq!(
            group_role_keys("CN=NetOps, OU=Groups, DC=x"),
            group_role_keys("CN=NetOps,OU=Groups,DC=x")
        );
    }

    #[test]
    fn a_plain_group_name_yields_itself() {
        assert_eq!(group_role_keys("NetOps"), vec!["netops".to_owned()]);
    }

    #[test]
    fn group_matching_is_case_insensitive_on_both_sides() {
        let c = cfg(&[("NetOps", Role::Admin)], None);
        let groups = vec!["CN=NETOPS,OU=Groups,DC=x".to_owned()];
        assert_eq!(role_for_groups(&groups, &c), Some(Role::Admin));
    }

    #[test]
    fn the_highest_matching_role_still_wins_across_dn_and_cn_forms() {
        let c = cfg(
            &[
                ("cn=noc,ou=g,dc=x", Role::Operator),
                ("NetOps", Role::Admin),
            ],
            None,
        );
        let groups = vec![
            "CN=NOC,OU=G,DC=x".to_owned(),
            "CN=NetOps,OU=G,DC=x".to_owned(),
        ];
        assert_eq!(role_for_groups(&groups, &c), Some(Role::Admin));
    }

    #[test]
    fn no_matching_group_falls_back_to_the_default_then_denies() {
        let groups = vec!["CN=Nobody,DC=x".to_owned()];
        assert_eq!(
            role_for_groups(&groups, &cfg(&[], Some(Role::Viewer))),
            Some(Role::Viewer)
        );
        assert_eq!(role_for_groups(&groups, &cfg(&[], None)), None);
    }

    #[test]
    fn a_case_collision_in_the_role_map_keeps_the_higher_role() {
        let raw = HashMap::from([
            ("NetOps".to_owned(), "viewer".to_owned()),
            ("netops".to_owned(), "admin".to_owned()),
        ]);
        assert_eq!(fold_role_map(&raw).get("netops"), Some(&Role::Admin));
    }

    #[test]
    fn an_unknown_role_in_the_map_is_skipped_not_fatal() {
        let raw = HashMap::from([
            ("a".to_owned(), "root".to_owned()),
            ("b".to_owned(), "Admin".to_owned()),
        ]);
        let folded = fold_role_map(&raw);
        assert_eq!(folded.get("a"), None);
        assert_eq!(folded.get("b"), Some(&Role::Admin));
    }

    // ── Transport ───────────────────────────────────────────────────────────────────────────────

    #[test]
    fn every_security_mode_round_trips_through_its_token_and_through_serde() {
        for m in LdapSecurity::ALL {
            assert_eq!(LdapSecurity::parse(m.as_str()), Some(m));
            assert_eq!(
                serde_json::to_value(m).expect("serializes"),
                serde_json::Value::String(m.as_str().to_owned())
            );
        }
        assert_eq!(LdapSecurity::parse("plain"), None);
    }

    #[test]
    fn the_connection_url_is_derived_from_the_security_mode() {
        let mut c = cfg(&[], None);
        c.host = "dc1.corp.example.com".to_owned();
        c.port = 636;
        assert_eq!(c.url(), "ldaps://dc1.corp.example.com:636");
        c.security = LdapSecurity::StartTls;
        c.port = 389;
        assert_eq!(c.url(), "ldap://dc1.corp.example.com:389");
    }

    #[test]
    fn an_ipv6_host_is_bracketed_in_the_url() {
        let mut c = cfg(&[], None);
        c.host = "2001:db8::1".to_owned();
        assert_eq!(c.url(), "ldaps://[2001:db8::1]:636");
    }

    // ── Validation ──────────────────────────────────────────────────────────────────────────────

    #[test]
    fn an_enabled_config_with_no_role_mapping_and_no_default_is_refused_at_save_time() {
        // Otherwise every login is denied with the same generic 401 a wrong password gets, while
        // the settings page looks correctly filled in.
        let mut i = input();
        i.role_map.clear();
        i.default_role = None;
        let err = validate(&i).expect_err("must refuse");
        assert!(err.contains("every login is denied"), "{err}");

        i.enabled = false;
        assert!(validate(&i).is_ok(), "a disabled directory may be a draft");
    }

    #[test]
    fn a_group_search_needs_both_halves_or_neither() {
        let mut i = input();
        i.group_base_dn = Some("OU=Groups,DC=x".to_owned());
        assert!(validate(&i).is_err());
        i.group_filter = Some("(member={user_dn})".to_owned());
        assert!(validate(&i).is_ok());
    }

    #[test]
    fn a_group_filter_that_names_nobody_is_refused() {
        let mut i = input();
        i.group_base_dn = Some("OU=Groups,DC=x".to_owned());
        i.group_filter = Some("(objectClass=group)".to_owned());
        let err = validate(&i).expect_err("must refuse");
        assert!(err.contains("{user_dn}"), "{err}");
    }

    #[test]
    fn an_unknown_role_is_named_in_the_rejection() {
        let mut i = input();
        i.default_role = Some("root".to_owned());
        assert!(validate(&i).expect_err("must refuse").contains("root"));
    }

    // ── Invariants nothing else can catch ───────────────────────────────────────────────────────

    #[test]
    fn the_ldap_provider_id_is_stable() {
        // Written into `users.oidc_provider_id` for every directory account. Changing it orphans
        // all of them: their stored identity pair stops matching, so the next login tries to
        // JIT-create an account whose username is already taken and is refused.
        assert_eq!(
            LDAP_PROVIDER_ID.to_string(),
            "03770e11-9fbe-5a8b-bbe3-f89611f47cc4"
        );
    }

    // Needles assembled at runtime: this test reads its own file, so a literal would match itself.
    #[test]
    fn the_module_never_disables_certificate_verification() {
        let src = include_str!("ldap.rs");
        for needle in [
            format!("set_no_tls_{}", "verify"),
            format!("{}()", "dangerous"),
            format!("{}CertVerifier", "Server"),
        ] {
            assert!(
                !src.contains(&needle),
                "ldap.rs reaches for `{needle}` — ADR-041 forbids a way to skip certificate \
                 verification, and a private CA goes in `ca_cert` instead"
            );
        }
    }

    #[test]
    fn no_log_statement_names_a_password() {
        // The bind password and the user's password must never reach a log line, a metric label or
        // a serialized DTO. Checks the `tracing::` call sites, which is where it would happen.
        let src = include_str!("ldap.rs");
        let marker = format!("tracing{}", "::");
        for line in src.lines() {
            let line = line.trim_start();
            if line.starts_with("//") || !line.contains(&marker) {
                continue;
            }
            assert!(
                !line.contains("password"),
                "a log statement mentions a password: {line}"
            );
        }
    }

    #[test]
    fn the_resolved_config_cannot_be_serialized() {
        // `LdapConfig` holds the decrypted bind password. Deriving `Serialize` on it is the mistake
        // this asserts against — the field is private and the type has no derive, so any future
        // attempt has to delete this test deliberately.
        let src = include_str!("ldap.rs");
        let decl = "pub struct LdapConfig {";
        let at = src.find(decl).expect("LdapConfig is declared");
        // Only the attribute lines immediately above the declaration; the doc comment above those
        // is prose and legitimately names the trait.
        let attrs: Vec<&str> = src[..at]
            .lines()
            .rev()
            .take_while(|l| l.trim_start().starts_with("#["))
            .collect();
        assert!(
            !attrs.join(" ").contains("Serialize"),
            "LdapConfig must not derive Serialize — it carries the plaintext bind password"
        );
    }

    #[test]
    fn the_save_sql_keeps_the_stored_password_only_when_the_input_omits_it() {
        // The `CASE WHEN $22` arms are what decide whether editing the base DN wipes the bind
        // password. An in-memory fake cannot catch a mistake in SQL meaning.
        for column in [
            "key_id",
            "wrapped_dek",
            "dek_nonce",
            "ciphertext",
            "ct_nonce",
        ] {
            // Whitespace-normalised: the statement is column-aligned for reading, and a test that
            // depended on how many spaces that alignment happens to use would break on any edit to
            // a neighbouring column name.
            let sql: String = SAVE_SQL.split_whitespace().collect::<Vec<_>>().join(" ");
            assert!(
                sql.contains(&format!("{column} = CASE WHEN $22")),
                "the upsert does not conditionally keep `{column}`"
            );
        }
        assert!(
            !SAVE_SQL.contains("ldap_config.host"),
            "non-secret columns are always replaced; only the sealed ones are conditional"
        );
    }
}
