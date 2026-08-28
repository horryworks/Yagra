// SPDX-License-Identifier: AGPL-3.0-only
//! MCP tools: about Yagra itself rather than about the network — its configuration, health, audit and reports (ADR-086).
//!
//! Split out of the single `tools.rs` by ADR-086; the module doc for the surface as a whole,
//! and the rules every tool here obeys, are in [`super`].

use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::CallToolResult;
// The module (not just the trait) — the `JsonSchema` derive expands to `schemars::…` paths, so the
// `schemars` name must be in scope. rmcp re-exports it, keeping exactly one schemars version.
use rmcp::schemars;
use rmcp::service::{RequestContext, RoleServer};
use rmcp::{tool, tool_router, ErrorData as McpError};
use serde::Deserialize;
use uuid::Uuid;

use super::YagraMcp;
use crate::api::scope::NodeScope;
use crate::api::ApiError;

// The shared scope: the helpers in `support.rs` and the types the other domain modules declare,
// re-exported by `mod.rs` so no file has to name where a sibling keeps a thing.
use super::*;

/// Which self-health question `get_system_health` was asked (ADR-042 I3a).
///
/// Split out of the tool body so the folding decision is testable without a `RequestContext`, the
/// same shape `topology_kind` uses. Parsing is exact: a caller who types `Pollers` is told, rather
/// than silently handed a different section.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum HealthSection {
    Pollers,
    PollerHealth,
    Pools,
    PollerNodes,
    NodeAssignment,
    MonitoringGaps,
    Dependencies,
    Hosts,
    HostTrends,
    Forwarding,
    Credentials,
    Version,
    Deployment,
    Upgrade,
}

impl HealthSection {
    /// Every accepted `section` value, in the order the description lists them.
    pub(super) const NAMES: &'static [&'static str] = &[
        "pollers",
        "poller_health",
        "pools",
        "poller_nodes",
        "node_assignment",
        "monitoring_gaps",
        "dependencies",
        "hosts",
        "host_trends",
        "forwarding",
        "credentials",
        "version",
        "deployment",
        "upgrade",
    ];

    pub(super) fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "pollers" => Self::Pollers,
            "poller_health" => Self::PollerHealth,
            "pools" => Self::Pools,
            "poller_nodes" => Self::PollerNodes,
            "node_assignment" => Self::NodeAssignment,
            "monitoring_gaps" => Self::MonitoringGaps,
            "dependencies" => Self::Dependencies,
            "hosts" => Self::Hosts,
            "host_trends" => Self::HostTrends,
            "forwarding" => Self::Forwarding,
            "credentials" => Self::Credentials,
            "version" => Self::Version,
            "deployment" => Self::Deployment,
            "upgrade" => Self::Upgrade,
            _ => return None,
        })
    }

    /// The `folded::FOLDED_READS` key for this section — the string the permission is filed under.
    pub(super) fn arg(self) -> &'static str {
        match self {
            Self::Pollers => "pollers",
            Self::PollerHealth => "poller_health",
            Self::Pools => "pools",
            Self::PollerNodes => "poller_nodes",
            Self::NodeAssignment => "node_assignment",
            Self::MonitoringGaps => "monitoring_gaps",
            Self::Dependencies => "dependencies",
            Self::Hosts => "hosts",
            Self::HostTrends => "host_trends",
            Self::Forwarding => "forwarding",
            Self::Credentials => "credentials",
            Self::Version => "version",
            Self::Deployment => "deployment",
            Self::Upgrade => "upgrade",
        }
    }
}

/// The refusal for a `section` [`HealthSection::parse`] does not serve — see
/// [`bad_fleet_summary_kind`] for why this is one function and not two.
///
/// It hands back [`HealthSection::NAMES`] rather than saying only "unknown": a model that is told
/// the vocabulary retries correctly, where one that is only told it was wrong guesses again.
pub(super) fn bad_health_section(section: &str) -> Result<CallToolResult, McpError> {
    tool_bad_params(
        "get_system_health",
        &format!(
            "unknown section {:?}; must be one of: {}",
            section,
            HealthSection::NAMES.join(", ")
        ),
    )
}

/// The id a [`ConfigKind`] needs, and which parameter carries it.
///
/// Named per referent rather than one polymorphic `id`, which is what keeps the 28-branch fold
/// inside the rule I1 set: no argument's meaning changes with another. `org_id` never means a node.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ConfigId {
    Node,
    Template,
    Profile,
    Org,
    Scan,
}

impl ConfigId {
    /// The parameter name, as the refusal message and the published schema both spell it.
    pub(super) fn param(self) -> &'static str {
        match self {
            Self::Node => "node_id",
            Self::Template => "template_id",
            Self::Profile => "profile_id",
            Self::Org => "org_id",
            Self::Scan => "scan_id",
        }
    }
}

/// Which configuration read `get_config` was asked for (ADR-042 I3b).
///
/// Twenty-eight branches behind one `kind`, following [`HealthSection`] rather than becoming
/// twenty-eight tools. The fold is defensible for the reason `alert_trends` was: no argument's
/// meaning changes with another. The five ids are named per referent — `node_id`, `template_id`,
/// `profile_id`, `org_id`, `scan_id` — so none of them ever means two things, which is the rule
/// I1 rejected `top_metrics` over. A single polymorphic `id` would have broken it.
///
/// The other half of the argument is recovery: a caller who mistypes `kind` is handed
/// [`Self::NAMES`] and can retry. A caller who picks the wrong *tool* is not told the right one
/// exists.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ConfigKind {
    Thresholds,
    EventRules,
    EventSources,
    NotificationChannels,
    RoutingRules,
    Profiles,
    ProfileTemplates,
    CollectionTemplates,
    TemplateItems,
    NodeCollection,
    ClassificationRules,
    MibCatalog,
    MetricMeanings,
    UrlCheck,
    DnsCheck,
    DiscoveryCandidates,
    DiscoveryScan,
    DiscoveryScans,
    MerakiOrgs,
    MerakiNetworks,
    MerakiPolling,
    ForwardDestinations,
    ReportDefinitions,
    ReportSchedules,
    Retention,
    AdjacencySettings,
    Llm,
    Roles,
    Oidc,
    Ldap,
}

impl ConfigKind {
    /// Every accepted `kind` value, in the order the description lists them.
    ///
    /// Several names are longer than the REST path's last segment on purpose. `forward_destinations`
    /// rather than `forwarding` because `get_system_health(section="forwarding")` already publishes
    /// that word for the delivery status; `adjacency_settings` rather than `neighbors` because
    /// `get_neighbors` is a live per-node read and this is a deployment-wide policy;
    /// `report_schedules` rather than `schedules` because `list_analyses(kind="schedules")` has it.
    /// One word meaning two things across two tools is how a model comes to guess.
    pub(super) const NAMES: &'static [&'static str] = &[
        "thresholds",
        "event_rules",
        "event_sources",
        "notification_channels",
        "routing_rules",
        "profiles",
        "profile_templates",
        "collection_templates",
        "template_items",
        "node_collection",
        "classification_rules",
        "mib_catalog",
        "metric_meanings",
        "url_check",
        "dns_check",
        "discovery_candidates",
        "discovery_scan",
        "discovery_scans",
        "meraki_orgs",
        "meraki_networks",
        "meraki_polling",
        "forward_destinations",
        "report_definitions",
        "report_schedules",
        "retention",
        "adjacency_settings",
        "llm",
        "roles",
        "oidc",
        "ldap",
    ];

    /// Exact match, with no default. A `kind` is the whole question here — unlike `get_topology`,
    /// where one graph is the obvious default — so a caller who omits it or mistypes it is told,
    /// never quietly served something else.
    pub(super) fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "thresholds" => Self::Thresholds,
            "event_rules" => Self::EventRules,
            "event_sources" => Self::EventSources,
            "notification_channels" => Self::NotificationChannels,
            "routing_rules" => Self::RoutingRules,
            "profiles" => Self::Profiles,
            "profile_templates" => Self::ProfileTemplates,
            "collection_templates" => Self::CollectionTemplates,
            "template_items" => Self::TemplateItems,
            "node_collection" => Self::NodeCollection,
            "classification_rules" => Self::ClassificationRules,
            "mib_catalog" => Self::MibCatalog,
            "metric_meanings" => Self::MetricMeanings,
            "url_check" => Self::UrlCheck,
            "dns_check" => Self::DnsCheck,
            "discovery_candidates" => Self::DiscoveryCandidates,
            "discovery_scan" => Self::DiscoveryScan,
            "discovery_scans" => Self::DiscoveryScans,
            "meraki_orgs" => Self::MerakiOrgs,
            "meraki_networks" => Self::MerakiNetworks,
            "meraki_polling" => Self::MerakiPolling,
            "forward_destinations" => Self::ForwardDestinations,
            "report_definitions" => Self::ReportDefinitions,
            "report_schedules" => Self::ReportSchedules,
            "retention" => Self::Retention,
            "adjacency_settings" => Self::AdjacencySettings,
            "llm" => Self::Llm,
            "roles" => Self::Roles,
            "oidc" => Self::Oidc,
            "ldap" => Self::Ldap,
            _ => return None,
        })
    }

    /// Which id this kind cannot answer without, if any.
    ///
    /// Exhaustive, so a new kind has to state whether it takes one, and **the only place that fact
    /// is written**: `config_in`'s prelude validates from this and the arms use what it produced,
    /// rather than each arm carrying its own copy of "this one needs an id".
    pub(super) fn required_id(self) -> Option<ConfigId> {
        match self {
            Self::NodeCollection | Self::UrlCheck | Self::DnsCheck => Some(ConfigId::Node),
            Self::TemplateItems => Some(ConfigId::Template),
            Self::ProfileTemplates => Some(ConfigId::Profile),
            Self::MerakiNetworks => Some(ConfigId::Org),
            Self::DiscoveryScan => Some(ConfigId::Scan),
            Self::Thresholds
            | Self::EventRules
            | Self::EventSources
            | Self::NotificationChannels
            | Self::RoutingRules
            | Self::Profiles
            | Self::CollectionTemplates
            | Self::ClassificationRules
            | Self::MibCatalog
            | Self::MetricMeanings
            | Self::DiscoveryCandidates
            | Self::DiscoveryScans
            | Self::MerakiOrgs
            | Self::MerakiPolling
            | Self::ForwardDestinations
            | Self::ReportDefinitions
            | Self::ReportSchedules
            | Self::Retention
            | Self::AdjacencySettings
            | Self::Llm
            | Self::Roles
            | Self::Oidc
            | Self::Ldap => None,
        }
    }

    /// The `folded::FOLDED_READS` key for this kind — the string the permission is filed under.
    pub(super) fn arg(self) -> &'static str {
        match self {
            Self::Thresholds => "thresholds",
            Self::EventRules => "event_rules",
            Self::EventSources => "event_sources",
            Self::NotificationChannels => "notification_channels",
            Self::RoutingRules => "routing_rules",
            Self::Profiles => "profiles",
            Self::ProfileTemplates => "profile_templates",
            Self::CollectionTemplates => "collection_templates",
            Self::TemplateItems => "template_items",
            Self::NodeCollection => "node_collection",
            Self::ClassificationRules => "classification_rules",
            Self::MibCatalog => "mib_catalog",
            Self::MetricMeanings => "metric_meanings",
            Self::UrlCheck => "url_check",
            Self::DnsCheck => "dns_check",
            Self::DiscoveryCandidates => "discovery_candidates",
            Self::DiscoveryScan => "discovery_scan",
            Self::DiscoveryScans => "discovery_scans",
            Self::MerakiOrgs => "meraki_orgs",
            Self::MerakiNetworks => "meraki_networks",
            Self::MerakiPolling => "meraki_polling",
            Self::ForwardDestinations => "forward_destinations",
            Self::ReportDefinitions => "report_definitions",
            Self::ReportSchedules => "report_schedules",
            Self::Retention => "retention",
            Self::AdjacencySettings => "adjacency_settings",
            Self::Llm => "llm",
            Self::Roles => "roles",
            Self::Oidc => "oidc",
            Self::Ldap => "ldap",
        }
    }
}

/// The refusal for a `kind` [`ConfigKind::parse`] does not serve — see [`bad_fleet_summary_kind`]
/// for why this is one function and not two, and [`bad_health_section`] for why it lists the
/// vocabulary.
pub(super) fn bad_config_kind(kind: &str) -> Result<CallToolResult, McpError> {
    tool_bad_params(
        "get_config",
        &format!(
            "unknown kind {:?}; must be one of: {}",
            kind,
            ConfigKind::NAMES.join(", ")
        ),
    )
}

#[derive(Debug, Default, Deserialize, schemars::JsonSchema)]
pub(super) struct ConfigParams {
    /// Which configuration to read. Required; see the tool description for the 30 values.
    pub(super) kind: String,
    /// The node (kind=node_collection | url_check | dns_check).
    node_id: Option<Uuid>,
    /// The collection template whose items to list (kind=template_items).
    template_id: Option<Uuid>,
    /// The profile whose templates to list (kind=profile_templates).
    profile_id: Option<Uuid>,
    /// The Meraki organization whose networks to list (kind=meraki_networks).
    org_id: Option<Uuid>,
    /// The discovery scan to report on (kind=discovery_scan).
    scan_id: Option<Uuid>,
    /// Return the effective set the poller collects rather than the node's own overrides
    /// (kind=node_collection; default false).
    resolved: Option<bool>,
    /// Case-insensitive substring: over metric name / OID / vendor (kind=mib_catalog), or over
    /// the metric name alone (kind=thresholds).
    search: Option<String>,
    /// Comma-separated scope levels to keep (kind=thresholds): `global`, `profile`, `group`,
    /// `group_id`, `node`, `interface`. Absent or empty means every level.
    scope_level: Option<String>,
    /// Comma-separated directions to keep (kind=thresholds): `above`, `below`. Absent or empty
    /// means both. A rule bounding **both** sides matches either value, so asking for `below`
    /// returns every rule that can fire as a value drops — including band rules whose reported
    /// `direction` says `above`.
    direction: Option<String>,
    /// Row cap (kind=thresholds 1–500 default 500; mib_catalog 1–2000 default 100;
    /// discovery_candidates 1–50 default 10).
    limit: Option<i64>,
}

/// The owned halves of a [`crate::thresholds::ThresholdFilter`], parsed from one `get_config` call.
///
/// An owning struct because `ThresholdFilter` borrows all three of its fields — building one inline
/// would borrow from temporaries that die at the end of the expression.
#[derive(Debug, Default, PartialEq)]
pub(super) struct ThresholdFilterOwned {
    metric: Option<String>,
    levels: Vec<yagra_common::ScopeLevel>,
    directions: Vec<yagra_common::Direction>,
}

impl ThresholdFilterOwned {
    pub(super) fn as_filter(&self) -> crate::thresholds::ThresholdFilter<'_> {
        crate::thresholds::ThresholdFilter {
            metric: self.metric.as_deref(),
            level: &self.levels,
            direction: &self.directions,
        }
    }
}

/// Parse `get_config(kind=thresholds)`'s three filters, in the REST edge's vocabulary (ADR-079 決定 1).
///
/// **This replaced `&Default::default()`, and the reason the old comment gave was factually wrong.**
/// It said `get_config` is a configuration dump whose callers ask for the ruleset rather than a slice
/// of it, so the filters were a UI narrowing. Reading what the screen actually does refutes that:
/// `ThresholdsPage` issues **three** `GET /thresholds` per page load and only one of them is the
/// visible page. The other two ask for a `total` — *is there any reachability rule at all* and *how
/// many port-level rules are hidden* — which are questions, not slices, and an MCP client could not
/// ask either one.
///
/// The cap makes it worse rather than merely awkward: `ThresholdStore::list_page` orders
/// **broadest scope first**, so a ruleset past 500 rows hides its node- and interface-level rules
/// completely, and those are exactly the ones that grow with the fleet.
///
/// Split out as a plain function over the params so it can be tested without a live `AdminState` —
/// the tool wrapper is unreachable from tests (no way to fabricate a `RequestContext`).
pub(super) fn threshold_filter_of(p: &ConfigParams) -> Result<ThresholdFilterOwned, ApiError> {
    // Both vocabularies are rendered from the enums rather than written out, through the same
    // helper the REST edge uses. Its hand-written copy had rotted to "global, profile, group or
    // node" — four of the six levels, unchanged since ADR-075 added `group_id` and ADR-076 added
    // `interface` — and a second hand-written list here would have rotted the same way (ADR-079).
    Ok(ThresholdFilterOwned {
        metric: crate::api::util::normalize_search(p.search.as_deref()),
        levels: crate::api::util::parse_set(
            "scope_level",
            p.scope_level.as_deref(),
            &crate::api::util::token_list(yagra_common::ScopeLevel::ALL.iter().map(|l| l.as_str())),
            yagra_common::ScopeLevel::from_token,
        )?,
        directions: crate::api::util::parse_set(
            "direction",
            p.direction.as_deref(),
            &crate::api::util::token_list(yagra_common::Direction::ALL.iter().map(|d| d.as_str())),
            yagra_common::Direction::from_token,
        )?,
    })
}

#[derive(Debug, Default, Deserialize, schemars::JsonSchema)]
pub(super) struct SystemHealthParams {
    /// Which self-health question: pollers | poller_health | pools | poller_nodes |
    /// monitoring_gaps | dependencies | hosts | host_trends | forwarding | credentials | version |
    /// deployment.
    pub(super) section: String,
    /// The poller to drill into (section=poller_nodes).
    poller_id: Option<String>,
    /// Which node's poller assignment to resolve (section=node_assignment).
    node_id: Option<Uuid>,
    /// Which host to trend (section=host_trends): `core`, or a poller id from section=hosts.
    instance: Option<String>,
    /// Trend window start, Unix seconds (section=host_trends; default 1h ago).
    from: Option<i64>,
    /// Trend window end, Unix seconds (section=host_trends; default now).
    to: Option<i64>,
    /// Trend resolution in seconds (section=host_trends; clamped to the window).
    step: Option<u64>,
    /// Max nodes to return (section=poller_nodes; 1–500, default 500).
    limit: Option<usize>,
}

#[derive(Debug, Default, Deserialize, schemars::JsonSchema)]
pub(super) struct ReportRunsParams {
    /// One run to fetch with its rendered result; omit for the recent-runs list.
    run_id: Option<Uuid>,
    /// Max runs to list (1–500, default 50). Ignored when `run_id` is given.
    limit: Option<i64>,
    /// Only runs generated from this report definition. Ignored when `run_id` is given.
    definition_id: Option<Uuid>,
    /// Only runs in this state: `queued` | `running` | `succeeded` | `failed`. Ignored when
    /// `run_id` is given.
    state: Option<String>,
    /// Only runs created at or after this RFC 3339 timestamp. Ignored when `run_id` is given.
    since: Option<String>,
}

#[derive(Debug, Default, Deserialize, schemars::JsonSchema)]
pub(super) struct AuditParams {
    /// Max rows (1–500, default 100).
    limit: Option<i64>,
    /// Keyset cursor: return rows older than this RFC 3339 timestamp.
    before: Option<String>,
    /// Only entries at or after this RFC 3339 timestamp.
    since: Option<String>,
    /// Only entries at or before this RFC 3339 timestamp.
    until: Option<String>,
    /// Free text matched against the username and the action (case-insensitive substring).
    q: Option<String>,
    /// Action kinds to include, comma-separated: `post` | `put` | `patch` | `delete` | `login` |
    /// `mcp`. Omit for all. An unknown token is an error rather than being ignored.
    action: Option<String>,
    /// Status classes to include, comma-separated: `ok` | `client` | `server`. Omit for all.
    status: Option<String>,
}

#[tool_router(router = system_router, vis = "pub(super)")]
impl YagraMcp {
    #[tool(
        description = "Is Yagra itself healthy? Check this before trusting anything else you read. \
                       `section` is one of: pollers (the poller fleet and per-pool summary), \
                       poller_health (poll-loop counters), pools, poller_nodes (which nodes one \
                       poller holds — needs `poller_id`), node_assignment (the inverse: which \
                       poller owns one node — needs `node_id`; the first thing to check when a \
                       single node stops reporting while its pool is fine), monitoring_gaps (recent core↔poller \
                       outages: data missing from these windows is missing, not flat), \
                       dependencies (per-store reachability), hosts (core/poller CPU, memory, \
                       disk), host_trends (one host over time — needs `instance`, optional \
                       `from`/`to`/`step`), forwarding (relay delivery status), credentials \
                       (whether stored credentials still decrypt), version, deployment (which \
                       optional tiers are enabled), upgrade (which binary is actually running — \
                       commit and build profile, not just the version — how much schema is \
                       applied, and whether this deployment could still be taken back to an \
                       earlier release). Sections require different permissions: most need view, \
                       forwarding and upgrade need manage-system, credentials needs \
                       manage-credentials."
    )]
    async fn get_system_health(
        &self,
        Parameters(p): Parameters<SystemHealthParams>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        const TOOL: &str = "get_system_health";
        let Some(section) = HealthSection::parse(&p.section) else {
            return bad_health_section(&p.section);
        };
        // Resolve → authorize → scope → availability. The permission check sits above every store
        // lookup so a caller who may not read a section cannot infer, from a 403-vs-unavailable,
        // whether this deployment has that subsystem configured at all.
        if let Some(deny) = self.deny_unless_permitted(&ctx, TOOL, section.arg()) {
            return deny;
        }
        let scope = match self.scope_of(&ctx).await {
            Ok(s) => s,
            Err(e) => return tool_api_error(TOOL, &e),
        };
        self.system_health_in(section, p, &scope).await
    }

    pub(super) async fn system_health_in(
        &self,
        section: HealthSection,
        p: SystemHealthParams,
        scope: &NodeScope,
    ) -> Result<CallToolResult, McpError> {
        const TOOL: &str = "get_system_health";
        // The sections that read the write side; `dependencies` and `hosts` deliberately do not,
        // because reporting that the database is unreachable is most useful when it is.
        let admin = self.state.admin.as_ref();
        match section {
            HealthSection::Pollers => match admin {
                Some(a) => ok_json(TOOL, &crate::api::pollers::poller_inventory(a).await),
                None => tool_unavailable(TOOL, "the poller inventory requires live mode"),
            },
            HealthSection::PollerHealth => match admin {
                Some(a) => ok_json(TOOL, &a.scheduler_stats.snapshot()),
                None => tool_unavailable(TOOL, "poll-loop counters require live mode"),
            },
            HealthSection::Pools => match admin {
                Some(a) => ok_json(TOOL, &crate::api::pools::pool_options(a).await),
                None => tool_unavailable(TOOL, "the pool list requires live mode"),
            },
            HealthSection::PollerNodes => {
                let Some(poller_id) = p.poller_id else {
                    return tool_bad_params(TOOL, "section poller_nodes needs `poller_id`");
                };
                match admin {
                    Some(a) => {
                        let page = crate::api::pollers::poller_nodes_page(
                            &self.state,
                            a,
                            poller_id,
                            p.limit,
                            scope,
                        )
                        .await;
                        ok_json(TOOL, &page)
                    }
                    None => tool_unavailable(TOOL, "the poller drill-down requires live mode"),
                }
            }
            HealthSection::NodeAssignment => {
                let Some(node_id) = p.node_id else {
                    return tool_bad_params(TOOL, "section node_assignment needs `node_id`");
                };
                // The one node-scoped section. Out of scope answers exactly what a nonexistent id
                // answers, so the tool cannot be used to confirm a node exists outside the
                // caller's groups.
                if let Some(deny) = deny_invisible_node(&self.state, scope, TOOL, node_id) {
                    return deny;
                }
                match admin {
                    Some(a) => {
                        match crate::api::pollers::node_assignment_of(&self.state, a, node_id).await
                        {
                            Ok(r) => ok_json(TOOL, &r),
                            Err(e) => tool_api_error(TOOL, &e),
                        }
                    }
                    None => tool_unavailable(TOOL, "node assignment requires live mode"),
                }
            }
            HealthSection::MonitoringGaps => match admin {
                Some(a) => ok_json(TOOL, &crate::api::pollers::monitoring_gaps(a).await),
                None => tool_unavailable(TOOL, "monitoring gaps require live mode"),
            },
            HealthSection::Dependencies => ok_json(
                TOOL,
                &crate::api::health::system_health_snapshot(&self.state).await,
            ),
            HealthSection::Hosts => ok_json(TOOL, &crate::api::system::host_inventory(&self.state)),
            HealthSection::HostTrends => {
                let Some(instance) = p.instance else {
                    return tool_bad_params(
                        TOOL,
                        "section host_trends needs `instance` (`core`, or a poller id from \
                         section=hosts)",
                    );
                };
                match crate::api::system::host_trends(&self.state, instance, p.from, p.to, p.step)
                    .await
                {
                    Ok(r) => ok_json(TOOL, &r),
                    Err(e) => tool_api_error(TOOL, &e),
                }
            }
            HealthSection::Forwarding => match admin {
                Some(a) => ok_json(
                    TOOL,
                    &crate::api::forwarding::forwarding_delivery_status(&self.state, a),
                ),
                None => tool_unavailable(TOOL, "forwarding status requires live mode"),
            },
            HealthSection::Credentials => match admin {
                Some(a) => match crate::api::credentials::credential_decrypt_health(a).await {
                    Ok(h) => ok_json(TOOL, &h),
                    Err(e) => tool_api_error(TOOL, &e),
                },
                None => tool_unavailable(TOOL, "credential health requires live mode"),
            },
            HealthSection::Version => ok_json(TOOL, &crate::api::health::running_version()),
            HealthSection::Deployment => {
                ok_json(TOOL, &crate::api::health::client_config(&self.state).await)
            }
            HealthSection::Upgrade => {
                match (self.state.upgrade.as_ref(), self.state.admin.as_ref()) {
                    (Some(u), Some(admin)) => match crate::api::upgrade::upgrade_status(
                        u,
                        self.state.started,
                        &crate::api::upgrade::poller_builds(admin),
                    )
                    .await
                    {
                        Ok(r) => ok_json(TOOL, &r),
                        Err(e) => tool_unavailable(TOOL, &format!("{e}")),
                    },
                    _ => tool_unavailable(TOOL, "the upgrade view requires live mode"),
                }
            }
        }
    }

    #[tool(
        description = "Saved report runs. Without `run_id`, the most recent runs (newest first, \
                       `limit` 1–500, default 50), optionally narrowed by `definition_id`, \
                       `state` (queued|running|succeeded|failed — note `succeeded`, not the \
                       `done` an analysis run uses) and `since` (RFC 3339); with `run_id`, that \
                       run plus its rendered result. Fleet-wide only: a rendered report keeps no \
                       per-node attribution, so a group-scoped token is refused rather than shown \
                       the whole fleet."
    )]
    async fn get_report_runs(
        &self,
        Parameters(p): Parameters<ReportRunsParams>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        const TOOL: &str = "get_report_runs";
        let arg = if p.run_id.is_some() { "detail" } else { "list" };
        if let Some(deny) = self.deny_unless_permitted(&ctx, TOOL, arg) {
            return deny;
        }
        match self.scope_of(&ctx).await {
            Ok(scope) => self.report_runs_in(p, &scope).await,
            Err(e) => tool_api_error(TOOL, &e),
        }
    }

    pub(super) async fn report_runs_in(
        &self,
        p: ReportRunsParams,
        scope: &NodeScope,
    ) -> Result<CallToolResult, McpError> {
        const TOOL: &str = "get_report_runs";
        match p.run_id {
            Some(id) => {
                match crate::api::reports::report_run_detail(&self.state, scope, id).await {
                    Ok(r) => ok_json(TOOL, &r),
                    Err(e) => tool_api_error(TOOL, &e),
                }
            }
            None => {
                let filter = match crate::api::reports::parse_run_filter(
                    p.definition_id,
                    p.state.as_deref(),
                    p.since.as_deref(),
                ) {
                    Ok(f) => f,
                    Err(e) => return tool_api_error(TOOL, &e),
                };
                match crate::api::reports::report_runs(&self.state, scope, p.limit, &filter).await {
                    Ok(rows) => ok_json(TOOL, &rows),
                    Err(e) => tool_api_error(TOOL, &e),
                }
            }
        }
    }

    #[tool(
        description = "The audit log: who changed, acknowledged or triggered what, newest first. \
                       `limit` is 1–500 (default 100); `before` is an RFC 3339 timestamp for the \
                       next page. Narrow with `since`/`until` (the window to search, distinct from \
                       the `before` cursor), `q` (free text over the username and the action), \
                       `action` (post|put|patch|delete|login|mcp — `login` covers local, LDAP and \
                       OIDC sign-ins; `mcp` covers actions taken through this tool surface) and \
                       `status` (ok|client|server). `action` and `status` each take several values \
                       comma-separated. Requires the view-audit permission, which is separate from \
                       view."
    )]
    async fn get_audit(
        &self,
        Parameters(p): Parameters<AuditParams>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        const TOOL: &str = "get_audit";
        if let Some(deny) = self.deny_unless_permitted(&ctx, TOOL, "") {
            return deny;
        }
        self.audit_in(p).await
    }

    pub(super) async fn audit_in(&self, p: AuditParams) -> Result<CallToolResult, McpError> {
        const TOOL: &str = "get_audit";
        // Availability is checked **here** rather than left to `audit_page`, and the two surfaces
        // therefore order the two failures differently: REST answers 400 for a malformed filter on
        // a deployment with no log, this answers "unavailable" first. That is deliberate and
        // pre-dates Inc.4b — an assistant that gets a typed "this deployment keeps no audit log"
        // stops asking, whereas a 400 about a cursor invites it to retry with a different cursor
        // forever. `the_audit_tool_reports_a_missing_write_side_rather_than_an_empty_log` pins it.
        if self.state.admin.is_none() {
            return tool_unavailable(TOOL, "the audit log requires live mode");
        }
        // An unparseable cursor is a 400 here as it is over REST, never a silent jump back to the
        // newest page — a client walking the log would otherwise loop forever on page one. The
        // whole page function is the shared seam, so the filter cannot be validated more loosely
        // here than it is over REST (which is the drift `parse_event_filter` already paid for).
        let input = crate::api::audit::AuditFilterInput {
            limit: p.limit,
            before: p.before.as_deref(),
            since: p.since.as_deref(),
            until: p.until.as_deref(),
            q: p.q.as_deref(),
            action: p.action.as_deref(),
            status: p.status.as_deref(),
        };
        match crate::api::audit::audit_page(&self.state, input).await {
            Ok(rows) => ok_json(TOOL, &rows),
            Err(e) => tool_api_error(TOOL, &e),
        }
    }

    #[tool(
        description = "Read Yagra's own configuration — what it is set up to monitor, alert on, \
                       notify and forward, and how. `kind` is one of: **alerting/notification** — \
                       thresholds (the metric alert rules; `limit` 1–500, narrowed by `search` / \
                       `scope_level` / `direction`), event_rules, event_sources, \
                       notification_channels, routing_rules; **collection** — profiles, \
                       profile_templates (needs `profile_id`), collection_templates, \
                       template_items (needs `template_id`), node_collection (one node's collected \
                       metrics — needs `node_id`; `resolved=true` for the effective set the poller \
                       actually uses), classification_rules, mib_catalog (`search` filters, \
                       `limit` 1–2000, default 100), metric_meanings (one sentence per metric, \
                       plus whether it is a `check`, `derived` or `collected` number — \
                       the dictionary behind a bare metric name); **per-node checks** — \
                       url_check, dns_check (both need `node_id`); **discovery** — \
                       discovery_candidates (`limit` 1–50, default 10), discovery_scan (needs \
                       `scan_id`), discovery_scans (the sweeps this core is holding, newest \
                       first, `limit` 1–50, default 20 — this is how to find a `scan_id`); \
                       **Meraki** — meraki_orgs, meraki_networks (needs `org_id`), \
                       meraki_polling; **forwarding** — forward_destinations; **reports** — \
                       report_definitions, report_schedules; **deployment settings** — retention, \
                       adjacency_settings, llm, roles, oidc, ldap. \
                       **Reading kind=thresholds.** A rule is `{scope_level, scope_ids, metric, \
                       warning_below, critical_below, warning_above, critical_above, \
                       dwell_samples}` and fires when `metric` crosses any bound it names for \
                       `dwell_samples` consecutive samples — a count of samples, never seconds. \
                       Any bound may be null; a rule naming one is one-sided, not broken, and a \
                       rule naming a `_below` and an `_above` alerts **outside a band** (a dark \
                       optical link and an overdriven one, from one rule). It reports `direction`, \
                       `warning` and `critical` as well: those are the rule's **primary side** \
                       only, kept for clients written before bands existed. On a band rule they \
                       describe half of it — read the four bounds, not these three. `scope_level` \
                       is one of six, \
                       broadest first: `global` (every node, `scope_ids` empty), `profile`, \
                       `group` (a node **tag value**), `group_id` (a folder group, inherited by \
                       everything inside it), `node`, `interface`. **`scope_ids` holds a \
                       different kind of id at each level** — profile UUIDs, tag strings, \
                       folder-group UUIDs, node UUIDs, or `<node-uuid>:<ifindex>` for one port — \
                       so resolve them with get_config(kind=profiles), list_node_groups or \
                       list_nodes rather than assuming a UUID. The narrowest level that reaches a \
                       target wins, and rules at that level merge by keeping the more restrictive \
                       bound of each severity. `metric` is `__liveness__` for the reachability \
                       rule, a sentinel rather than a collected metric; `if_in_util_pct` / \
                       `if_out_util_pct` / `if_in_bps` / `if_out_bps` are derived per port and \
                       exist in no time series. The reply carries `total` (rules matching the \
                       filter, ignoring the cap) and `truncated` — and when `truncated` is true \
                       the rows you have are the **broadest** ones, so narrow with `scope_level` \
                       rather than raising `limit`. For one port's effective rules use \
                       get_interface_thresholds; for what a metric measures use \
                       kind=metric_meanings. \
                       Kinds require different permissions: oidc and ldap need manage-users; \
                       notification_channels, routing_rules, forward_destinations and llm need \
                       manage-system; mib_catalog, metric_meanings, url_check, dns_check, \
                       discovery_candidates, the three meraki kinds, the two report kinds, \
                       retention, adjacency_settings and roles need view; the rest need \
                       manage-config. This reads configuration only — no tool changes it. No \
                       stored secret is returned: url_check reports whether a credential is \
                       bound, not which one."
    )]
    async fn get_config(
        &self,
        Parameters(p): Parameters<ConfigParams>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        const TOOL: &str = "get_config";
        let Some(kind) = ConfigKind::parse(&p.kind) else {
            return bad_config_kind(&p.kind);
        };
        // Resolve → authorize → scope → availability, as `get_system_health` does and for the same
        // reason: the permission check sits above every store lookup so a caller who may not read a
        // kind cannot infer, from a 403-vs-unavailable, whether this deployment has that subsystem
        // configured at all.
        if let Some(deny) = self.deny_unless_permitted(&ctx, TOOL, kind.arg()) {
            return deny;
        }
        let scope = match self.scope_of(&ctx).await {
            Ok(s) => s,
            Err(e) => return tool_api_error(TOOL, &e),
        };
        self.config_in(kind, p, &scope).await
    }

    pub(super) async fn config_in(
        &self,
        kind: ConfigKind,
        p: ConfigParams,
        scope: &NodeScope,
    ) -> Result<CallToolResult, McpError> {
        const TOOL: &str = "get_config";
        // Required-id and scope both run **above** the availability check, which is the ordering
        // `get_system_health` documents and the reason this prelude exists rather than a guard per
        // arm: a caller who cannot see a node must get the same answer whether or not this
        // deployment has a write side, or the 503-vs-unavailable difference is itself a
        // disclosure. `required_id` is the single source of which kinds take one.
        let id = match kind.required_id() {
            None => Uuid::nil(),
            Some(want) => {
                let got = match want {
                    ConfigId::Node => p.node_id,
                    ConfigId::Template => p.template_id,
                    ConfigId::Profile => p.profile_id,
                    ConfigId::Org => p.org_id,
                    ConfigId::Scan => p.scan_id,
                };
                let Some(id) = got else {
                    return tool_bad_params(
                        TOOL,
                        &format!("kind {} needs `{}`", kind.arg(), want.param()),
                    );
                };
                if want == ConfigId::Node {
                    // The same answer a nonexistent id gets, so the tool cannot confirm that a node
                    // exists outside the caller's groups.
                    if let Some(deny) = deny_invisible_node(&self.state, scope, TOOL, id) {
                        return deny;
                    }
                }
                id
            }
        };
        // `id` is `Uuid::nil()` for the 23 kinds that need none, and every arm that reads it is one
        // `required_id` just validated — so there is no unwrap here and no second copy of the fact.
        // Answered above the live-mode gate, because it looks nothing up: the dictionary is
        // compiled in, and `GET /api/v1/metric-meanings` takes no `Admin` extractor for the same
        // reason. A tool that reported "unavailable" where its REST twin answers would be the two
        // surfaces disagreeing about a question neither has to consult a store to settle.
        if kind == ConfigKind::MetricMeanings {
            return ok_json(TOOL, &crate::api::mib::metric_meanings());
        }
        let Some(a) = self.state.admin.as_ref() else {
            return tool_unavailable(TOOL, "reading configuration requires live mode");
        };
        match kind {
            // ── alerting / notification ──────────────────────────────────────
            ConfigKind::Thresholds => {
                // Narrowed by the same three filters the REST edge accepts, through the same
                // helper and the same enum vocabulary — see `threshold_filter_of` for why the
                // earlier "no filter" reasoning did not survive reading the screen's own calls.
                let filter = match threshold_filter_of(&p) {
                    Ok(f) => f,
                    Err(e) => return tool_api_error(TOOL, &e),
                };
                match crate::api::thresholds::threshold_page(a, p.limit, &filter.as_filter()).await
                {
                    Ok(page) => ok_json(TOOL, &page),
                    Err(e) => tool_api_error(TOOL, &e),
                }
            }
            ConfigKind::EventRules => match a.events.list_rules().await {
                Ok(rules) => ok_json(TOOL, &rules),
                Err(e) => tool_error(TOOL, "list event rules", &e),
            },
            ConfigKind::EventSources => match a.events.list_sources().await {
                Ok(sources) => ok_json(TOOL, &sources),
                Err(e) => tool_error(TOOL, "list event sources", &e),
            },
            ConfigKind::NotificationChannels => match a.notifications.list_channels().await {
                Ok(list) => ok_json(TOOL, &list),
                Err(e) => tool_error(TOOL, "list notification channels", &e),
            },
            ConfigKind::RoutingRules => match a.notifications.list_rules().await {
                Ok(list) => ok_json(TOOL, &list),
                Err(e) => tool_error(TOOL, "list routing rules", &e),
            },
            // ── collection ───────────────────────────────────────────────────
            ConfigKind::Profiles => match a.repo.list_profiles().await {
                Ok(list) => ok_json(TOOL, &list),
                Err(e) => tool_error(TOOL, "list profiles", &e),
            },
            ConfigKind::ProfileTemplates => match a.collection.list_profile_templates(id).await {
                Ok(list) => ok_json(TOOL, &list),
                Err(e) => tool_error(TOOL, "list profile templates", &e),
            },
            ConfigKind::CollectionTemplates => match a.collection.list_templates().await {
                Ok(list) => ok_json(TOOL, &list),
                Err(e) => tool_error(TOOL, "list collection templates", &e),
            },
            ConfigKind::TemplateItems => match a.collection.list_template_items(id).await {
                Ok(list) => ok_json(TOOL, &list),
                Err(e) => tool_error(TOOL, "list template items", &e),
            },
            ConfigKind::NodeCollection => {
                match crate::api::collection::node_collection(a, id, p.resolved.unwrap_or(false))
                    .await
                {
                    Ok(set) => ok_json(TOOL, &set),
                    Err(e) => tool_api_error(TOOL, &e),
                }
            }
            ConfigKind::ClassificationRules => match a.classification.list_rules().await {
                Ok(list) => ok_json(TOOL, &list),
                Err(e) => tool_error(TOOL, "list classification rules", &e),
            },
            // Unreachable: answered above the live-mode gate. Written out anyway because the
            // match is exhaustive on purpose — that is what makes a new kind impossible to
            // forget — and because `unreachable!()` would put a panic here that only an edit to
            // the guard above could reach. Both call the one function in `api::mib`.
            ConfigKind::MetricMeanings => ok_json(TOOL, &crate::api::mib::metric_meanings()),
            ConfigKind::MibCatalog => {
                // Default 100 where REST defaults to the 2000 cap: a model asking about one OID
                // does not want the whole catalog in its context, and `search` is the narrowing
                // this branch expects to be used with. The *ceiling* is shared (`api::mib`); only
                // the default differs, which is the `get_topology` precedent.
                match crate::api::mib::mib_catalog(
                    a,
                    p.search.as_deref(),
                    Some(p.limit.unwrap_or(100)),
                )
                .await
                {
                    Ok(list) => ok_json(TOOL, &list),
                    Err(e) => tool_api_error(TOOL, &e),
                }
            }
            // ── per-node checks ──────────────────────────────────────────────
            ConfigKind::UrlCheck => {
                use crate::api::checks::CheckKind as _;
                match crate::api::checks::UrlCheck::load(a, id).await {
                    Ok(Some(cfg)) => {
                        ok_json(TOOL, &crate::mcp::dto::UrlCheckDto::from_config(&cfg))
                    }
                    Ok(None) => tool_unavailable(TOOL, "that node has no URL check configured"),
                    Err(e) => tool_error(TOOL, "load url check", &e),
                }
            }
            ConfigKind::DnsCheck => {
                use crate::api::checks::CheckKind as _;
                match crate::api::checks::DnsCheck::load(a, id).await {
                    Ok(Some(cfg)) => ok_json(TOOL, &cfg),
                    Ok(None) => tool_unavailable(TOOL, "that node has no DNS check configured"),
                    Err(e) => tool_error(TOOL, "load dns check", &e),
                }
            }
            // ── discovery ────────────────────────────────────────────────────
            ConfigKind::DiscoveryCandidates => {
                // `matched_credential_id` stays on these rows, and that is a decision rather than
                // an oversight: `SECRET_KEYS` is an exact-match rule, so nothing would have caught
                // it either way. Unlike a URL check's binding — which a model can neither resolve
                // nor use — *which stored credential answered on an unclassified device* is the
                // answer to a discovery-triage question, and it names something an operator can
                // look up in the UI. Different question, different treatment.
                let limit = p.limit.and_then(|n| usize::try_from(n).ok());
                ok_json(
                    TOOL,
                    &crate::api::discovery::recent_candidates(&self.state, limit),
                )
            }
            ConfigKind::DiscoveryScan => match a.discovery.get(id) {
                Some(status) => ok_json(TOOL, &status),
                None => tool_unavailable(TOOL, "no scan with that id"),
            },
            ConfigKind::DiscoveryScans => {
                let limit = p.limit.and_then(|n| usize::try_from(n).ok());
                ok_json(TOOL, &a.discovery.list(limit.unwrap_or(20).clamp(1, 50)))
            }
            // ── Meraki ───────────────────────────────────────────────────────
            ConfigKind::MerakiOrgs => match crate::api::meraki::org_views(a).await {
                Ok(list) => ok_json(TOOL, &list),
                Err(e) => tool_api_error(TOOL, &e),
            },
            ConfigKind::MerakiNetworks => match crate::api::meraki::network_views(a, id).await {
                Ok(list) => ok_json(TOOL, &list),
                Err(e) => tool_api_error(TOOL, &e),
            },
            ConfigKind::MerakiPolling => {
                ok_json(TOOL, &crate::api::meraki::polling_switch(a).await)
            }
            // ── forwarding ───────────────────────────────────────────────────
            ConfigKind::ForwardDestinations => match a.forward.list().await {
                Ok(rows) => ok_json(TOOL, &rows),
                Err(e) => tool_error(TOOL, "list forward destinations", &e),
            },
            // ── reports ──────────────────────────────────────────────────────
            ConfigKind::ReportDefinitions => match a.reports.repo().list_definitions().await {
                Ok(list) => ok_json(TOOL, &list),
                Err(e) => tool_error(TOOL, "list report definitions", &e),
            },
            ConfigKind::ReportSchedules => match a.reports.repo().list_schedules().await {
                Ok(list) => ok_json(TOOL, &list),
                Err(e) => tool_error(TOOL, "list report schedules", &e),
            },
            // ── deployment settings ──────────────────────────────────────────
            ConfigKind::Retention => ok_json(
                TOOL,
                &crate::api::retention::retention_policy(&self.state, a).await,
            ),
            ConfigKind::AdjacencySettings => {
                ok_json(TOOL, &crate::api::neighbors::adjacency_config(a).await)
            }
            ConfigKind::Llm => match crate::api::rca::llm_config_view(a).await {
                Ok(view) => ok_json(TOOL, &view),
                Err(e) => tool_api_error(TOOL, &e),
            },
            // Pure: the matrix is the type system's, not the deployment's.
            ConfigKind::Roles => ok_json(TOOL, &crate::api::users::roles_matrix()),
            ConfigKind::Oidc => match self.state.oidc.as_ref() {
                Some(oidc) => match oidc.list().await {
                    Ok(list) => ok_json(TOOL, &list),
                    Err(e) => tool_error(TOOL, "list oidc providers", &e),
                },
                None => tool_unavailable(TOOL, "this deployment persists no SSO configuration"),
            },
            ConfigKind::Ldap => match self.state.ldap.as_ref() {
                Some(ldap) => match ldap.view().await {
                    Ok(view) => ok_json(TOOL, &view),
                    Err(e) => tool_error(TOOL, "read ldap config", &e),
                },
                None => tool_unavailable(TOOL, "this deployment persists no LDAP configuration"),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp::tools::testkit::*;
    use axum::http::StatusCode;

    /// Every dimension `ThresholdFilter` declares is reachable from `get_config(kind=thresholds)`,
    /// and each parameter moves **only** its own dimension (ADR-079 決定 1).
    ///
    /// The behavioural half is the point. A source scan can say `p.scope_level` appears somewhere
    /// in the initializer; it cannot say the value arrives in the right field. This drives the
    /// parser and compares the whole result, so swapping two `parse_set` calls — which compiles,
    /// runs, and answers a different question — fails here.
    ///
    /// ⚠️ **It still cannot say the filter narrows the SQL.** `ThresholdFilter` is handed to
    /// `ThresholdStore::list_page`, which needs PostgreSQL. The proof that the predicate is
    /// evaluated rather than ignored is `above` + `below` summing to the unfiltered `total` on a
    /// real deployment (ADR-053's rule: a check that cannot separate "ignored" from "applied"
    /// proves neither).
    #[test]
    fn every_threshold_filter_dimension_is_reachable_from_get_config() {
        const SRC: &str = include_str!("../../thresholds.rs");
        let declared: Vec<&str> = SRC
            .split("pub struct ThresholdFilter<'a> {")
            .nth(1)
            .expect("ThresholdFilter is declared in thresholds.rs")
            .lines()
            .take_while(|l| !l.starts_with('}'))
            .map(str::trim)
            .filter_map(|l| l.strip_prefix("pub "))
            .filter_map(|l| l.split(':').next())
            .collect();
        assert_eq!(
            declared,
            ["metric", "level", "direction"],
            "ThresholdFilter gained or lost a dimension — decide whether get_config should offer              it, then update this test and the tool's description"
        );

        // Absent means unfiltered on every axis. This is the case the cap makes load-bearing: an
        // accidentally-narrowing default would silently shrink the ruleset a client believes is whole.
        assert_eq!(
            threshold_filter_of(&ConfigParams::default()).expect("no filter parses"),
            ThresholdFilterOwned::default()
        );

        let metric = threshold_filter_of(&ConfigParams {
            search: Some("  cpu  ".into()),
            ..Default::default()
        })
        .expect("a metric term parses");
        assert_eq!(
            metric,
            ThresholdFilterOwned {
                metric: Some("cpu".into()),
                levels: vec![],
                directions: vec![],
            },
            "search must trim and must move nothing but the metric term"
        );

        let level = threshold_filter_of(&ConfigParams {
            scope_level: Some("node,interface".into()),
            ..Default::default()
        })
        .expect("a level set parses");
        assert_eq!(
            level,
            ThresholdFilterOwned {
                metric: None,
                levels: vec![
                    yagra_common::ScopeLevel::Node,
                    yagra_common::ScopeLevel::Interface
                ],
                directions: vec![],
            },
            "scope_level must move nothing but the level set, in the order given"
        );

        let direction = threshold_filter_of(&ConfigParams {
            direction: Some("below".into()),
            ..Default::default()
        })
        .expect("a direction parses");
        assert_eq!(
            direction,
            ThresholdFilterOwned {
                metric: None,
                levels: vec![],
                directions: vec![yagra_common::Direction::Below],
            },
            "direction must move nothing but the direction set"
        );
    }

    /// The filter accepts every token its enums define, and refuses everything else.
    ///
    /// **Both halves, because either alone is worthless.** A tool that refused every token would
    /// pass a rejection-only test; a tool that accepted every string would pass an
    /// acceptance-only one. The refusals below are also chosen to be *near misses* — `groupid`
    /// against the real `group_id`, `interfaces` against `interface` — so a substring or
    /// prefix match could not pass either.
    #[test]
    fn the_threshold_filter_takes_the_enums_vocabulary_and_nothing_else() {
        for level in yagra_common::ScopeLevel::ALL {
            let parsed = threshold_filter_of(&ConfigParams {
                scope_level: Some(level.as_str().into()),
                ..Default::default()
            })
            .unwrap_or_else(|_| panic!("{} is a level the store writes", level.as_str()));
            assert_eq!(parsed.levels, vec![level]);
        }
        for direction in yagra_common::Direction::ALL {
            let parsed = threshold_filter_of(&ConfigParams {
                direction: Some(direction.as_str().into()),
                ..Default::default()
            })
            .unwrap_or_else(|_| panic!("{} is a direction the store writes", direction.as_str()));
            assert_eq!(parsed.directions, vec![direction]);
        }

        // The whole set at once, spelled from the enum, must also be accepted — a per-token loop
        // would still pass if the splitter dropped everything after the first comma.
        let every_level: String = yagra_common::ScopeLevel::ALL
            .iter()
            .map(|l| l.as_str())
            .collect::<Vec<_>>()
            .join(",");
        assert_eq!(
            threshold_filter_of(&ConfigParams {
                scope_level: Some(every_level),
                ..Default::default()
            })
            .expect("the full level set parses")
            .levels,
            yagra_common::ScopeLevel::ALL.to_vec()
        );

        // An **empty** token is not in this list on purpose: `split_set` drops it, so `scope_level=`
        // and a trailing comma both mean unfiltered — the spelling the WebUI sends while clearing a
        // filter. Pinned below rather than left to chance, because the alternative (a set holding
        // one empty string) would match nothing and read as "no rules exist".
        assert_eq!(
            threshold_filter_of(&ConfigParams {
                scope_level: Some("node,".into()),
                ..Default::default()
            })
            .expect("a trailing comma is not an error")
            .levels,
            vec![yagra_common::ScopeLevel::Node]
        );
        assert_eq!(
            threshold_filter_of(&ConfigParams {
                scope_level: Some(String::new()),
                direction: Some(String::new()),
                ..Default::default()
            })
            .expect("an empty set is not an error"),
            ThresholdFilterOwned::default(),
            "an empty filter must mean unfiltered, exactly as omitting it does"
        );

        for bad in ["groupid", "interfaces", "Global", "scope_leval"] {
            let err = threshold_filter_of(&ConfigParams {
                scope_level: Some(format!("node,{bad}")),
                ..Default::default()
            })
            .expect_err("an unknown level must be refused, never dropped");
            assert_eq!(
                err.status(),
                StatusCode::BAD_REQUEST,
                "a bad filter must reach the client as bad_params, not as an internal error"
            );
        }
        for bad in ["sideways", "abov", "Above"] {
            let err = threshold_filter_of(&ConfigParams {
                direction: Some(bad.into()),
                ..Default::default()
            })
            .expect_err("an unknown direction must be refused, never dropped");
            assert_eq!(err.status(), StatusCode::BAD_REQUEST);
        }
    }

    // ── ADR-042 I3a ─────────────────────────────────────────────────────────────────────────────

    /// Every section the description advertises is one the dispatcher accepts, and vice versa.
    ///
    /// The description is published verbatim, so a section named there but not parsed would have a
    /// model calling it and reasoning from the failure — the same class of harm as a wrong tool
    /// name, one level down.
    #[test]
    fn every_advertised_health_section_parses() {
        for name in HealthSection::NAMES {
            let parsed = HealthSection::parse(name)
                .unwrap_or_else(|| panic!("section {name} is advertised but not parsed"));
            assert_eq!(
                parsed.arg(),
                *name,
                "section {name} round-trips to a different key, so its permission would be looked \
                 up under the wrong row"
            );
        }
        assert_eq!(
            HealthSection::NAMES.len(),
            14,
            "the advertised section list changed; check the description and folded.rs together"
        );
    }

    /// The biconditional the reachability guard cannot express — the `get_system_health` twin of
    /// [`every_config_kind_has_a_folded_row_and_vice_versa`].
    ///
    /// `folded::every_folded_branch_is_reachable_from_its_tool` is a bare substring search over this
    /// file, so a section whose `arg` is a word already quoted somewhere here would pass with no row
    /// at all — and this family is full of them: `credentials`, `forwarding`, `version`, `hosts` and
    /// `deployment` are all present as literals for other reasons.
    ///
    /// 🚨 A section with no row does not fail a test and does not fail to compile. It reaches
    /// production and **panics on the first call**, because `folded::required_permission` resolves
    /// the permission by looking the pair up in this table and `unwrap_or_else(|| panic!(…))` when
    /// it is absent. `ConfigKind` has had this guard since ADR-042 I3b; `HealthSection` — the older
    /// and larger of the two folds — did not.
    #[test]
    fn every_health_section_has_a_folded_row_and_vice_versa() {
        let rows: std::collections::BTreeSet<&str> = crate::mcp::folded::FOLDED_READS
            .iter()
            .filter(|f| f.tool == "get_system_health")
            .map(|f| f.arg)
            .collect();
        let sections: std::collections::BTreeSet<&str> =
            HealthSection::NAMES.iter().copied().collect();
        assert_eq!(
            rows, sections,
            "the `get_system_health` folded rows and its advertised sections disagree"
        );
    }

    /// The panic that makes the test above load-bearing, demonstrated rather than asserted about.
    ///
    /// A rejection-only test would pass on a `required_permission` that panicked for everything, so
    /// the acceptance case comes first: every advertised section resolves, and only then does an
    /// unknown one blow up.
    #[test]
    fn an_unlisted_section_panics_where_a_listed_one_resolves() {
        for name in HealthSection::NAMES {
            let _ = crate::mcp::folded::required_permission("get_system_health", name);
        }
        let unlisted = std::panic::catch_unwind(|| {
            crate::mcp::folded::required_permission("get_system_health", "storage_pressure")
        });
        assert!(
            unlisted.is_err(),
            "a section with no folded row must fail loudly, which is why the set-equality test above \
             has to catch it at build time instead"
        );
    }

    #[test]
    fn an_unknown_health_section_is_rejected() {
        assert!(
            HealthSection::parse("Pollers").is_none(),
            "parsing is exact"
        );
        assert!(HealthSection::parse("poller-nodes").is_none());
        assert!(HealthSection::parse("").is_none());
    }

    /// The two sections that need an id say so instead of answering about something else.
    #[tokio::test]
    async fn the_sections_that_need_an_id_refuse_without_one() {
        let m = mcp();
        for section in [HealthSection::PollerNodes, HealthSection::NodeAssignment] {
            let r = m
                .system_health_in(section, SystemHealthParams::default(), &unrestricted())
                .await;
            assert!(
                r.is_err(),
                "section {} answered without the id it needs",
                section.arg()
            );
        }
    }

    /// A section whose subsystem is absent reports that as an availability note, not a hard error —
    /// the model should say "this deployment has no write side" and move on.
    #[tokio::test]
    async fn skeleton_mode_reports_unavailable_rather_than_failing() {
        let r = mcp()
            .system_health_in(
                HealthSection::Pollers,
                SystemHealthParams::default(),
                &unrestricted(),
            )
            .await
            .expect("ok result");
        assert_eq!(json_of(&r)["available"], serde_json::json!(false));
    }

    /// The sections that read no store answer for real even in skeleton mode. `dependencies` in
    /// particular must: reporting that the database is unreachable is most useful when it is.
    #[tokio::test]
    async fn the_stateless_sections_answer_in_skeleton_mode() {
        let m = mcp();
        let deps = json_of(
            &m.system_health_in(
                HealthSection::Dependencies,
                SystemHealthParams::default(),
                &unrestricted(),
            )
            .await
            .expect("ok result"),
        );
        assert_eq!(
            deps["overall"], "degraded",
            "skeleton mode has no write side, and that is a fact to report rather than an error"
        );
        let version = json_of(
            &m.system_health_in(
                HealthSection::Version,
                SystemHealthParams::default(),
                &unrestricted(),
            )
            .await
            .expect("ok result"),
        );
        assert!(version["core"].is_string());
    }

    // ── get_config(kind=…) — ADR-042 I3b ─────────────────────────────────────

    /// Every kind the description advertises is one the dispatcher accepts, and round-trips to the
    /// key its permission is filed under. Same reasoning as the health-section version: the
    /// description ships verbatim, and a kind named there but not parsed has a model reasoning from
    /// a failure it cannot learn from.
    #[test]
    fn every_advertised_config_kind_parses() {
        for name in ConfigKind::NAMES {
            let parsed = ConfigKind::parse(name)
                .unwrap_or_else(|| panic!("kind {name} is advertised but not parsed"));
            assert_eq!(
                parsed.arg(),
                *name,
                "kind {name} round-trips to a different key, so its permission would be looked up \
                 under the wrong row"
            );
        }
        assert_eq!(
            ConfigKind::NAMES.len(),
            30,
            "the advertised kind list changed; check the description and folded.rs together"
        );
    }

    /// The biconditional the reachability guard cannot express.
    ///
    /// `folded::every_folded_branch_is_reachable_from_its_tool` is a bare substring search over this
    /// file, so a row whose `arg` happens to be a word already quoted somewhere here would pass with
    /// no arm at all — `schedules`, `credentials`, `forwarding`, `version`, `list` and `history` are
    /// all already present as literals. Comparing the two *sets* closes that class: a kind with no
    /// row loses its permission lookup (`required_permission` panics), and a row with no kind is a
    /// route the ledger claims is served and is not.
    #[test]
    fn every_config_kind_has_a_folded_row_and_vice_versa() {
        let rows: std::collections::BTreeSet<&str> = crate::mcp::folded::FOLDED_READS
            .iter()
            .filter(|f| f.tool == "get_config")
            .map(|f| f.arg)
            .collect();
        let kinds: std::collections::BTreeSet<&str> = ConfigKind::NAMES.iter().copied().collect();
        assert_eq!(
            rows, kinds,
            "the `get_config` folded rows and its advertised kinds disagree"
        );
    }

    /// A typo is refused, and there is no default to fall back to: unlike `get_topology`, no one
    /// kind is the obvious question here.
    #[test]
    fn an_unknown_config_kind_is_rejected() {
        assert!(ConfigKind::parse("url-check").is_none(), "parsing is exact");
        assert!(ConfigKind::parse("Thresholds").is_none());
        assert!(ConfigKind::parse("forwarding").is_none());
        assert!(ConfigKind::parse("schedules").is_none());
        assert!(ConfigKind::parse("").is_none());
    }

    /// The five kinds that need an id say so rather than answering about something else.
    #[tokio::test]
    async fn the_config_kinds_that_need_an_id_refuse_without_one() {
        let m = mcp();
        for kind in [
            ConfigKind::NodeCollection,
            ConfigKind::UrlCheck,
            ConfigKind::DnsCheck,
            ConfigKind::TemplateItems,
            ConfigKind::ProfileTemplates,
            ConfigKind::MerakiNetworks,
            ConfigKind::DiscoveryScan,
        ] {
            let r = m
                .config_in(kind, ConfigParams::default(), &unrestricted())
                .await;
            assert!(
                r.is_err(),
                "kind {} answered without the id it needs",
                kind.arg()
            );
        }
    }

    /// The three node-scoped kinds hide a node outside the caller's groups, and hide it the same
    /// way a nonexistent id is hidden — otherwise the tool is an existence oracle.
    #[tokio::test]
    async fn a_per_node_config_kind_hides_a_node_outside_the_scope() {
        for kind in [
            ConfigKind::NodeCollection,
            ConfigKind::UrlCheck,
            ConfigKind::DnsCheck,
        ] {
            let r = mcp()
                .config_in(
                    kind,
                    ConfigParams {
                        kind: kind.arg().to_owned(),
                        node_id: Some(Uuid::nil()),
                        ..Default::default()
                    },
                    &sees_nothing(),
                )
                .await
                .unwrap_or_else(|_| panic!("kind {} should answer, not error", kind.arg()));
            let body = json_of(&r);
            assert_eq!(
                body["available"],
                serde_json::json!(false),
                "{}",
                kind.arg()
            );
            assert_eq!(body["reason"], "no node with that id", "{}", kind.arg());
        }
    }

    /// A node the caller cannot see answers exactly what a nonexistent one answers.
    #[tokio::test]
    async fn node_assignment_hides_a_node_outside_the_scope() {
        let r = mcp()
            .system_health_in(
                HealthSection::NodeAssignment,
                SystemHealthParams {
                    section: "node_assignment".to_owned(),
                    node_id: Some(Uuid::nil()),
                    ..Default::default()
                },
                &sees_nothing(),
            )
            .await
            .expect("ok result");
        let body = json_of(&r);
        assert_eq!(body["available"], serde_json::json!(false));
        assert_eq!(body["reason"], "no node with that id");
    }

    /// Without a write side the audit tool says so, rather than answering an empty log — "nobody
    /// has done anything" and "this deployment keeps no audit log" must not read alike to a model.
    ///
    /// The cursor rule itself is not reachable from here (it lives in `audit::audit_page`, behind
    /// the live store, and is covered on the REST side). Saying that is better than a test whose
    /// name claims more than it checks.
    #[tokio::test]
    async fn the_audit_tool_reports_a_missing_write_side_rather_than_an_empty_log() {
        let r = mcp()
            .audit_in(AuditParams {
                limit: None,
                before: Some("yesterday".to_owned()),
                since: None,
                until: None,
                q: None,
                action: None,
                status: None,
            })
            .await
            .expect("ok result");
        let body = json_of(&r);
        assert_eq!(body["available"], serde_json::json!(false));
        assert_eq!(body["reason"], "the audit log requires live mode");
    }
}
