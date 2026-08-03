// SPDX-License-Identifier: AGPL-3.0-only
//! Role-based access control with group-scoped visibility (ADR-014).
//!
//! Two axes: **what** a principal may do (a [`Role`] → [`Permission`] set) and **which**
//! nodes they may see (a [`Scope`] over groups, soft multi-tenancy). The northbound API
//! enforces both in one place — every state-changing call checks a permission, and every
//! listing is filtered to the principal's visible groups. Roles are predefined for the MVP
//! (viewer/operator/admin); the model leaves room for custom roles later.

use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

/// Predefined roles, ordered least → most privileged.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Hash,
    PartialOrd,
    Ord,
    Serialize,
    Deserialize,
    utoipa::ToSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    /// Read-only: inventory, metrics, alerts.
    Viewer,
    /// Viewer plus operational actions (ack/mute alerts, maintenance windows).
    Operator,
    /// Full control including configuration, credentials, and users.
    Admin,
}

/// A discrete capability checked at the API edge.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Permission {
    /// View inventory, metrics, and alerts.
    View,
    /// Acknowledge / mute / snooze alerts.
    AckAlerts,
    /// Open or close maintenance windows.
    ManageMaintenance,
    /// Create/edit nodes, profiles, thresholds.
    ManageConfig,
    /// Create/rotate monitoring credentials.
    ManageCredentials,
    /// Manage users and role assignments.
    ManageUsers,
    /// Read the audit log (who changed what).
    ViewAudit,
}

/// Which authentication surface a credential may be presented at.
///
/// A personal access token authenticated `/mcp` and nothing else, so there was never a choice to
/// express. Now that the REST API accepts one too the two surfaces differ enough that "may
/// authenticate here" has to be recorded per token: `/mcp` is a curated, mostly-read tool surface,
/// while REST is the entire configuration API. A token minted for an AI client must not become a
/// configuration credential because the server learned a new trick — so the surfaces are named on
/// the token, and anything that does not name [`TokenSurface::Rest`] cannot reach `/api/v1`.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Hash,
    PartialOrd,
    Ord,
    Serialize,
    Deserialize,
    utoipa::ToSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum TokenSurface {
    /// The MCP tool surface at `/mcp` (ADR-028).
    Mcp,
    /// The northbound REST API under `/api/v1`.
    Rest,
}

impl TokenSurface {
    /// Every surface, in display order.
    pub const ALL: [TokenSurface; 2] = [TokenSurface::Mcp, TokenSurface::Rest];

    /// Stable snake_case key — the `api_tokens.surfaces` element and the serde representation.
    #[must_use]
    pub const fn key(self) -> &'static str {
        match self {
            TokenSurface::Mcp => "mcp",
            TokenSurface::Rest => "rest",
        }
    }

    /// Parse a stored key. An unrecognised value yields `None` and is **dropped** by the caller
    /// rather than guessed at: a token written by a newer core must not widen itself when an older
    /// one reads it (N-1, `extensibility.md` §6).
    #[must_use]
    pub fn parse(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|s| s.key() == key)
    }
}

/// How an account authenticates, which decides whether it can log in interactively at all.
///
/// `Service` is the reason this enum exists. An API token needs an owner so that disabling or
/// deleting the owner takes the token with it — but binding an unattended integration to a *person*
/// means it dies when they change teams. A service account is a machine identity: no password, no
/// IdP subject, and therefore no way to sign in. It exists to own tokens.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum UserKind {
    /// A local account with a password.
    Local,
    /// Provisioned from an external IdP (ADR-010); the password column holds a sentinel that can
    /// never verify.
    Oidc,
    /// Provisioned from an LDAP/AD directory (ADR-041). Same shape as [`UserKind::Oidc`] — the
    /// password column holds a sentinel and the directory is authoritative about the identity —
    /// but the credential is checked by binding to the directory rather than by a browser redirect.
    Ldap,
    /// A machine account that owns credentials and cannot sign in.
    Service,
}

impl UserKind {
    /// Every kind, in display order.
    pub const ALL: [UserKind; 4] = [
        UserKind::Local,
        UserKind::Oidc,
        UserKind::Ldap,
        UserKind::Service,
    ];

    /// Stable snake_case key — the `users.auth_source` value and the serde representation.
    #[must_use]
    pub const fn key(self) -> &'static str {
        match self {
            UserKind::Local => "local",
            UserKind::Oidc => "oidc",
            UserKind::Ldap => "ldap",
            UserKind::Service => "service",
        }
    }

    /// Parse a stored `auth_source`. Unknown values read as [`UserKind::Local`], matching the column
    /// default that predates this enum — an old row without the column is a local account.
    #[must_use]
    pub fn parse(key: &str) -> Self {
        Self::ALL
            .into_iter()
            .find(|k| k.key() == key)
            .unwrap_or(UserKind::Local)
    }

    /// Whether this kind can authenticate interactively (password or SSO).
    ///
    /// Load-bearing beyond display: the "would this leave nobody able to administer the system"
    /// guard must count only accounts a human can actually sign in with, or the last enabled admin
    /// becoming a service account locks every person out of the WebUI.
    #[must_use]
    pub const fn can_log_in(self) -> bool {
        match self {
            UserKind::Local | UserKind::Oidc | UserKind::Ldap => true,
            UserKind::Service => false,
        }
    }

    /// Whether an outside directory — not Yagra — is authoritative about whether this account still
    /// exists.
    ///
    /// Load-bearing for API tokens. Yagra is never *told* that an IdP or a domain controller has
    /// disabled someone: `upsert_external_user` runs on **successful** login only, so a disabled
    /// account's row simply freezes with `enabled = true`. Sessions survive that harmlessly because
    /// they expire within a day; a no-expiry token would not. The only signal available is that the
    /// owner stopped signing in, which is why `ApiTokenStore::verify` treats an idle external owner
    /// as gone. Local and service accounts are exempt — nothing outside Yagra can disagree about
    /// them.
    ///
    /// This is a `match`, not `!= Local && != Service`, so a future kind has to answer the question.
    #[must_use]
    pub const fn is_external(self) -> bool {
        match self {
            UserKind::Oidc | UserKind::Ldap => true,
            UserKind::Local | UserKind::Service => false,
        }
    }
}

impl Role {
    /// Every predefined role, least → most privileged (for the role/privilege matrix).
    pub const ALL: [Role; 3] = [Role::Viewer, Role::Operator, Role::Admin];

    /// Stable snake_case key (matches the serde representation).
    #[must_use]
    pub const fn key(self) -> &'static str {
        match self {
            Role::Viewer => "viewer",
            Role::Operator => "operator",
            Role::Admin => "admin",
        }
    }

    /// Parse a role token, derived from [`Role::ALL`] so the accepted list lives in one place.
    ///
    /// Fallible on purpose. Callers want two different things from an unrecognised token and both
    /// are legitimate: a **stored** value must fall back to the least-privileged role (a row this
    /// binary cannot read must not become an admin), while an **operator-supplied** one must be
    /// rejected so the mistake is visible at the edge instead of silently demoting someone. One
    /// list, two policies, each written where it applies.
    #[must_use]
    pub fn parse(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|r| r.key() == key)
    }

    /// Parse a role name an **operator typed**, tolerating surrounding space and any casing.
    ///
    /// Separate from [`Role::parse`] because the inputs are different in kind, not in strictness.
    /// `parse` reads values Yagra itself wrote (a `users.role` column, an API field the edge already
    /// validated) and a mismatch there means corruption. This one reads a group→role mapping typed
    /// into a settings form, where `Admin` is the same answer as `admin` and refusing it produces a
    /// login denial the operator cannot diagnose from the generic 401. Both derive from
    /// [`Role::ALL`], so the accepted set is still declared once.
    #[must_use]
    pub fn parse_lenient(s: &str) -> Option<Self> {
        Self::parse(&s.trim().to_ascii_lowercase())
    }

    /// Short human label.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Role::Viewer => "Viewer",
            Role::Operator => "Operator",
            Role::Admin => "Admin",
        }
    }

    /// One-line description of the role.
    #[must_use]
    pub const fn description(self) -> &'static str {
        match self {
            Role::Viewer => "Read-only: inventory, metrics, and alerts within scope.",
            Role::Operator => {
                "Viewer plus incident response: acknowledge/mute alerts, run Troubleshoot \
                 analyses and AI root-cause explanations, and manage maintenance windows."
            }
            Role::Admin => "Full control: configuration, credentials, users, and the audit log.",
        }
    }

    /// Whether this role grants a permission (privilege is cumulative).
    #[must_use]
    pub fn grants(self, perm: Permission) -> bool {
        match perm {
            Permission::View => true, // every role can view within its scope
            Permission::AckAlerts | Permission::ManageMaintenance => self >= Role::Operator,
            Permission::ManageConfig
            | Permission::ManageCredentials
            | Permission::ManageUsers
            | Permission::ViewAudit => self == Role::Admin,
        }
    }
}

impl Permission {
    /// Every permission, in display order (for the role/privilege matrix).
    pub const ALL: [Permission; 7] = [
        Permission::View,
        Permission::AckAlerts,
        Permission::ManageMaintenance,
        Permission::ManageConfig,
        Permission::ManageCredentials,
        Permission::ManageUsers,
        Permission::ViewAudit,
    ];

    /// Stable snake_case key (matches the serde representation).
    #[must_use]
    pub const fn key(self) -> &'static str {
        match self {
            Permission::View => "view",
            Permission::AckAlerts => "ack_alerts",
            Permission::ManageMaintenance => "manage_maintenance",
            Permission::ManageConfig => "manage_config",
            Permission::ManageCredentials => "manage_credentials",
            Permission::ManageUsers => "manage_users",
            Permission::ViewAudit => "view_audit",
        }
    }

    /// Short human label.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Permission::View => "View",
            Permission::AckAlerts => "Respond to incidents",
            Permission::ManageMaintenance => "Manage maintenance",
            Permission::ManageConfig => "Manage configuration",
            Permission::ManageCredentials => "Manage credentials",
            Permission::ManageUsers => "Manage users",
            Permission::ViewAudit => "View audit log",
        }
    }

    /// One-line description of the capability.
    #[must_use]
    pub const fn description(self) -> &'static str {
        match self {
            Permission::View => "View inventory, metrics, and alerts within scope.",
            // Naming this one for the *acts* it covers rather than for one of them: it also gates
            // launching a Troubleshoot analysis and asking for an AI root-cause explanation, and a
            // matrix that lists only "acknowledge" leaves an admin unable to see why an operator
            // can run those. Keep this in step with every `RequireAckAlerts` / `Permission::AckAlerts`
            // call site — the privilege matrix renders this string verbatim.
            Permission::AckAlerts => {
                "Acknowledge or mute alerts, close event alerts, run Troubleshoot analyses, \
                 and request AI root-cause explanations."
            }
            Permission::ManageMaintenance => "Open or close maintenance windows.",
            Permission::ManageConfig => {
                "Create and edit nodes, profiles, thresholds, and collection."
            }
            Permission::ManageCredentials => "Create, edit, and rotate monitoring credentials.",
            Permission::ManageUsers => "Manage user accounts and role assignments.",
            Permission::ViewAudit => "Read the audit log (who changed what).",
        }
    }
}

/// Which slice of the inventory an account may see: everything, or a named set of node groups.
///
/// A node is visible if it belongs to an allowed group or to anything beneath one. A node in **no**
/// group is visible only under `All` — an unassigned node is not leaked to a scoped account.
// Soft scoping, ADR-014: a visibility filter over one shared inventory, not tenancy. `///` on this
// type is published verbatim in the OpenAPI document (and on the public site's API reference), so
// design references belong in `//` like this one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
pub enum Scope {
    /// Unrestricted visibility.
    All,
    /// Visibility limited to these group identifiers.
    Groups(BTreeSet<String>),
}

impl Scope {
    /// A scope limited to the given groups.
    #[must_use]
    pub fn groups<I, S>(groups: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Scope::Groups(groups.into_iter().map(Into::into).collect())
    }

    /// Whether a node belonging to `node_groups` is within this scope. A node in **no**
    /// group is visible only under `All` (an unscoped node is not leaked to scoped users).
    #[must_use]
    pub fn allows(&self, node_groups: &BTreeSet<String>) -> bool {
        match self {
            Scope::All => true,
            Scope::Groups(allowed) => node_groups.iter().any(|g| allowed.contains(g)),
        }
    }

    /// The folder-group ids this scope names, or `None` when it is unrestricted.
    ///
    /// **A `Groups` entry is a `node_groups.id` UUID**, canonically lowercase-hyphenated — not a
    /// group *name*. Names are editable and not unique, so a rename would silently widen or void a
    /// scope; the id is the thing the inventory actually keys on. This is the single place that
    /// rule is written down: parsing it in two modules is how the two come to disagree.
    ///
    /// An entry that does not parse is **dropped**, and dropping every entry yields an empty
    /// `Some(vec![])` — a scope naming only deleted or malformed groups therefore sees **nothing**.
    /// Returning `None` (i.e. unrestricted) there would turn a corrupt scope into a privilege
    /// escalation, so the empty and the unrestricted cases must never collapse into one another.
    #[must_use]
    pub fn group_uuids(&self) -> Option<Vec<uuid::Uuid>> {
        match self {
            Scope::All => None,
            Scope::Groups(raw) => Some(
                raw.iter()
                    .filter_map(|g| uuid::Uuid::parse_str(g).ok())
                    .collect(),
            ),
        }
    }
}

/// An authenticated principal: a role plus a visibility scope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Principal {
    /// The principal's role.
    pub role: Role,
    /// The groups they may see.
    pub scope: Scope,
}

impl Principal {
    /// New principal.
    #[must_use]
    pub fn new(role: Role, scope: Scope) -> Self {
        Self { role, scope }
    }

    /// Whether the principal may perform `perm`.
    #[must_use]
    pub fn can(&self, perm: Permission) -> bool {
        self.role.grants(perm)
    }

    /// Whether the principal may see a node belonging to `node_groups`.
    #[must_use]
    pub fn can_see(&self, node_groups: &BTreeSet<String>) -> bool {
        self.scope.allows(node_groups)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn groups(items: &[&str]) -> BTreeSet<String> {
        items.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn role_privileges_are_cumulative() {
        assert!(Role::Viewer.grants(Permission::View));
        assert!(!Role::Viewer.grants(Permission::AckAlerts));

        assert!(Role::Operator.grants(Permission::View));
        assert!(Role::Operator.grants(Permission::AckAlerts));
        assert!(!Role::Operator.grants(Permission::ManageConfig));

        assert!(Role::Admin.grants(Permission::ManageConfig));
        assert!(Role::Admin.grants(Permission::ManageUsers));

        // The audit log is admin-only (it exposes who did what across the system).
        assert!(!Role::Operator.grants(Permission::ViewAudit));
        assert!(Role::Admin.grants(Permission::ViewAudit));
    }

    #[test]
    fn role_ordering() {
        assert!(Role::Viewer < Role::Operator);
        assert!(Role::Operator < Role::Admin);
    }

    #[test]
    fn matrix_catalog_keys_match_serde() {
        // The matrix API serializes by the snake_case key; key() must agree with serde.
        for role in Role::ALL {
            let json = serde_json::to_string(&role).expect("serialize role");
            assert_eq!(json, format!("\"{}\"", role.key()));
        }
        for perm in Permission::ALL {
            let json = serde_json::to_string(&perm).expect("serialize permission");
            assert_eq!(json, format!("\"{}\"", perm.key()));
        }
    }

    #[test]
    fn matrix_reflects_grants() {
        // Viewer grants only View; Admin grants every permission.
        let viewer_grants: Vec<_> = Permission::ALL
            .into_iter()
            .filter(|p| Role::Viewer.grants(*p))
            .collect();
        assert_eq!(viewer_grants, vec![Permission::View]);
        assert!(Permission::ALL.into_iter().all(|p| Role::Admin.grants(p)));
    }

    #[test]
    fn all_scope_sees_everything_including_ungrouped() {
        let p = Principal::new(Role::Admin, Scope::All);
        assert!(p.can_see(&groups(&["tokyo"])));
        assert!(p.can_see(&BTreeSet::new())); // ungrouped node
    }

    #[test]
    fn group_scope_filters_visibility() {
        let p = Principal::new(Role::Operator, Scope::groups(["tokyo", "osaka"]));
        assert!(p.can_see(&groups(&["tokyo"])));
        assert!(p.can_see(&groups(&["osaka", "edge"])));
        assert!(!p.can_see(&groups(&["london"])));
        assert!(!p.can_see(&BTreeSet::new())); // ungrouped not leaked to scoped user
    }

    #[test]
    fn scoped_operator_can_ack_but_not_configure() {
        let p = Principal::new(Role::Operator, Scope::groups(["tokyo"]));
        assert!(p.can(Permission::AckAlerts));
        assert!(!p.can(Permission::ManageConfig));
    }

    #[test]
    fn all_scope_names_no_groups_at_all() {
        assert_eq!(Scope::All.group_uuids(), None);
    }

    #[test]
    fn group_uuids_parses_the_ids_it_holds() {
        let a = uuid::Uuid::from_u128(1);
        let b = uuid::Uuid::from_u128(2);
        let ids = Scope::groups([a.to_string(), b.to_string()])
            .group_uuids()
            .expect("a group scope names groups");
        assert_eq!(ids.len(), 2);
        assert!(ids.contains(&a) && ids.contains(&b));
    }

    // The inversion that matters: a scope whose entries are all unusable must see NOTHING, not
    // everything. `None` means unrestricted, so an empty parse result has to stay `Some(vec![])` —
    // if the two ever collapse, a corrupt scope becomes a privilege escalation.
    #[test]
    fn a_scope_whose_entries_all_fail_to_parse_sees_nothing_not_everything() {
        let s = Scope::groups(["tokyo", "not-a-uuid"]);
        assert_eq!(s.group_uuids(), Some(Vec::new()));
        assert_ne!(s.group_uuids(), None);
    }

    #[test]
    fn an_unparseable_entry_does_not_discard_its_valid_siblings() {
        let a = uuid::Uuid::from_u128(7);
        let ids = Scope::groups([a.to_string(), "tokyo".to_owned()])
            .group_uuids()
            .expect("a group scope names groups");
        assert_eq!(ids, vec![a]);
    }

    // `key()` is what lands in a database column and `#[serde(rename_all)]` is what lands in JSON.
    // Nothing makes the two agree, so a disagreement means the writer produces rows the reader
    // cannot parse (testing.md). Both directions, for both enums.
    #[test]
    fn every_token_surface_round_trips_through_its_key_and_through_serde() {
        for s in TokenSurface::ALL {
            assert_eq!(TokenSurface::parse(s.key()), Some(s));
            assert_eq!(
                serde_json::to_value(s).expect("a surface serializes"),
                serde_json::Value::String(s.key().to_owned()),
            );
        }
        assert_eq!(TokenSurface::parse("kafka"), None);
    }

    #[test]
    fn every_user_kind_round_trips_through_its_key_and_through_serde() {
        for k in UserKind::ALL {
            assert_eq!(UserKind::parse(k.key()), k);
            assert_eq!(
                serde_json::to_value(k).expect("a kind serializes"),
                serde_json::Value::String(k.key().to_owned()),
            );
        }
    }

    // The `auth_source` column predates this enum and defaults to 'local', so an unknown value has
    // to read as a local account rather than panic or invent a kind.
    //
    // The needle used to be "ldap", which stopped being unknown when ADR-041 landed. "saml" is the
    // deliberate replacement: ADR-041 decided *not* to implement SAML (an external SAML→OIDC bridge
    // is the recommended configuration), so it is the one authentication token this schema is not
    // expected to grow.
    #[test]
    fn an_unknown_auth_source_reads_as_a_local_account() {
        assert_eq!(UserKind::parse("saml"), UserKind::Local);
        assert_eq!(UserKind::parse(""), UserKind::Local);
    }

    // The guard that keeps humans from being locked out counts only accounts a human can sign in
    // with. If a service account ever counted, promoting one to admin and disabling the last person
    // would leave the WebUI unreachable with no way back in.
    #[test]
    fn only_a_service_account_cannot_log_in() {
        for k in UserKind::ALL {
            assert_eq!(k.can_log_in(), k != UserKind::Service, "{}", k.key());
        }
    }

    // Which kinds an outside directory can disable behind Yagra's back. This is what decides whether
    // an owner's idleness kills their API tokens (`ApiTokenStore::verify`), and the rule was
    // hardcoded to OIDC before LDAP existed — so an AD-disabled account's tokens would have lived
    // forever, with nothing failing to compile. Pinned as an exact set, not a spot check.
    #[test]
    fn an_external_kind_is_exactly_the_directory_backed_ones() {
        let external: Vec<&str> = UserKind::ALL
            .into_iter()
            .filter(|k| k.is_external())
            .map(UserKind::key)
            .collect();
        assert_eq!(external, vec!["oidc", "ldap"]);
    }

    #[test]
    fn every_role_round_trips_through_its_key() {
        for r in Role::ALL {
            assert_eq!(Role::parse(r.key()), Some(r));
        }
        assert_eq!(Role::parse("superuser"), None);
        assert_eq!(Role::parse(""), None);
    }
}
