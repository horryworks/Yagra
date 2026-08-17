// SPDX-License-Identifier: AGPL-3.0-only
//! The folded-read table: which REST route each branch of a folded tool mirrors, and what it
//! demands (ADR-042 I3a).
//!
//! ADR-042 decision 3 says tools are parameterized rather than transcribed — `get_system_health`
//! answers twelve endpoints behind a `section` argument instead of becoming twelve tools, because a
//! model picks worse from a longer list. That fold creates a problem the ADR did not anticipate:
//! **the endpoints behind one tool do not share a permission.**
//!
//! The measured spread across the routes folded here is `View` ×28, `ManageConfig` ×10,
//! `ManageSystem` ×6, `ManageUsers` ×2, `ManageCredentials` ×1, `ViewAudit` ×1, `AckAlerts` ×1, and
//! two that are deliberately unauthenticated over REST. Picking one permission for the whole tool
//! fails in both directions: a loose choice hands the forwarding topology or the audit log to any viewer, and a
//! strict choice recreates the very gap ADR-042 exists to close.
//!
//! So the permission is **data**, one row per branch, and the tool looks it up before it looks at
//! anything else. ADR-042 decision 2 declined a `Permission` column on the 243-row ledger because
//! nothing could check it; that reasoning holds there and not here — over these 49 rows the
//! permission is a value a test can compare against the REST handler's own extractor, and
//! [`tests::every_folded_read_demands_what_its_rest_route_demands`] does exactly that.
//!
//! The same table drives three more checks, so one edit keeps four things honest:
//! [`tests::every_folded_read_is_claimed_by_its_ledger_line`] (the route exists and the ledger
//! names this tool) and [`tests::every_folded_result_is_free_of_forbidden_keys`] (the response
//! schema carries no secret key). The last one reads the **OpenAPI document** rather than a
//! hand-built instance — see its doc comment for why that is stronger than the instance canary it
//! supplements.

use yagra_common::Permission;

/// One branch of a folded MCP tool.
///
/// Only `tool`, `arg` and `perm` are read at runtime — by [`required_permission`], which every
/// folded tool calls before it touches a store. The rest exist for the guards below, which is the
/// point of writing them down: a claim nothing consumes at runtime is still a claim a test can
/// check against the REST side.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) struct FoldedRead {
    /// The tool that serves this branch.
    pub tool: &'static str,
    /// The argument value selecting it (`section`/`kind`), or the flag that turns it on.
    pub arg: &'static str,
    /// HTTP method of the REST route it mirrors. All but `run_rca` are `GET`.
    pub method: &'static str,
    /// The REST route, in **ledger form** (`:id`, not `{id}`) so it can be matched against
    /// `route_table::ROUTES` directly.
    pub path: &'static str,
    /// What the caller must hold. `None` means the REST route is deliberately unauthenticated —
    /// the two client-bootstrap reads — in which case the tool still requires `View`, because MCP
    /// has no pre-login state to bootstrap and there is no reason to open a second anonymous
    /// surface.
    pub perm: Option<Permission>,
    /// Why this branch may carry `pool`/`profile`, or `None` to hold it to the stricter rule.
    ///
    /// `INVENTORY_NOISE_KEYS` exists because those ids are noise on a row describing monitored
    /// equipment. On the poller-facing reads they are the *answer*, so the exemption is per-row and
    /// has to be argued in writing rather than assumed from the family.
    pub inventory_ids_ok: Option<&'static str>,
    /// Why this branch may return a field the contract describes as `{}` — an untyped blob the key
    /// check cannot see into — or `None` to require that it returns none.
    ///
    /// Almost nothing needs this. A `serde_json::Value` field annotated `#[schema(value_type = T)]`
    /// is **not** opaque: the document carries `T`'s real shape and the walk follows it, which is
    /// how the `pool` on `run_rca`'s stored evidence was found. Only a genuinely undeclared blob
    /// lands here, and it must say so rather than passing for want of anything to check.
    pub opaque_ok: Option<&'static str>,
    /// The `mcp/dto.rs` type this branch serves **instead of** the REST body, and why.
    ///
    /// `None` is the overwhelming default and the reason the schema walk below is a statement about
    /// what the tool *sends*: the branch returns the route's own type, so the route's own schema
    /// describes it. Naming a type here says it does not.
    ///
    /// **This is not an ADR-018 exemption and must not become one.** It moves the key check, it
    /// does not skip it: the named type is declared in `dto.rs`, where
    /// `the_canary_covers_every_dto_in_this_module` forces an instance into the forbidden-key
    /// canary. What is lost is the schema walk's reach — every field of every nested type, whether
    /// or not a sample populates it — which is why the guard also demands that the REST body really
    /// does carry a banned key, so a stale claim cannot sit here covering a future field.
    ///
    /// It exists because ADR-042 I3b found the case the ADR had assumed away: `GET
    /// /nodes/:node_id/url-check` must keep `credential` on the REST body (the WebUI's edit form
    /// round-trips it, and a form that cannot prefill the binding clears it on every unrelated
    /// edit), so "lower it on the MCP side" — the ADR's prescription — left the guard reading a
    /// schema that still carried the key.
    pub lowered_to: Option<(&'static str, &'static str)>,
}

/// The pool exemption, shared by the five reads whose entire subject is poller assignment.
const POOL_IS_THE_ANSWER: Option<&'static str> = Some(
    "the question this branch answers is which poller owns the work, so the pool is the answer \
     rather than inventory noise",
);

/// Every folded branch ADR-042 I3a and I3b ship. **This is also each increment's inventory** — the
/// guards below read it, and the ledger flip in the same increment covers exactly these paths.
pub(crate) const FOLDED_READS: &[FoldedRead] = &[
    // ── get_system_health(section=…) ─────────────────────────────────────────
    FoldedRead {
        tool: "get_system_health",
        arg: "pollers",
        method: "GET",
        path: "/api/v1/pollers",
        perm: Some(Permission::View),
        inventory_ids_ok: POOL_IS_THE_ANSWER,
        opaque_ok: None,
        lowered_to: None,
    },
    FoldedRead {
        tool: "get_system_health",
        arg: "poller_health",
        method: "GET",
        path: "/api/v1/poller-health",
        perm: Some(Permission::View),
        inventory_ids_ok: None,
        opaque_ok: None,
        lowered_to: None,
    },
    FoldedRead {
        tool: "get_system_health",
        arg: "pools",
        method: "GET",
        path: "/api/v1/pools",
        perm: Some(Permission::View),
        inventory_ids_ok: None,
        opaque_ok: None,
        lowered_to: None,
    },
    FoldedRead {
        tool: "get_system_health",
        arg: "poller_nodes",
        method: "GET",
        path: "/api/v1/pollers/:id/nodes",
        perm: Some(Permission::View),
        inventory_ids_ok: POOL_IS_THE_ANSWER,
        opaque_ok: None,
        lowered_to: None,
    },
    FoldedRead {
        tool: "get_system_health",
        arg: "node_assignment",
        method: "GET",
        path: "/api/v1/nodes/:node_id/assignment",
        perm: Some(Permission::View),
        // The inverse of `poller_nodes`, and filed beside it rather than on `get_node_status`
        // for a reason that is not cosmetic: `NodeStatusDto` is an *inventory* DTO held to the
        // strict key rule, so hanging an answer containing `pool` off it would have meant either
        // breaking that invariant or wrapping the result to dodge it. Ownership is a
        // poller-topology question; this is where the rest of that family lives.
        inventory_ids_ok: POOL_IS_THE_ANSWER,
        opaque_ok: None,
        lowered_to: None,
    },
    FoldedRead {
        tool: "get_system_health",
        arg: "monitoring_gaps",
        method: "GET",
        path: "/api/v1/monitoring-gaps",
        perm: Some(Permission::View),
        inventory_ids_ok: POOL_IS_THE_ANSWER,
        opaque_ok: None,
        lowered_to: None,
    },
    FoldedRead {
        tool: "get_system_health",
        arg: "dependencies",
        method: "GET",
        path: "/api/v1/system-health",
        perm: Some(Permission::View),
        inventory_ids_ok: None,
        opaque_ok: None,
        lowered_to: None,
    },
    FoldedRead {
        tool: "get_system_health",
        arg: "hosts",
        method: "GET",
        path: "/api/v1/system/hosts",
        perm: Some(Permission::View),
        inventory_ids_ok: POOL_IS_THE_ANSWER,
        opaque_ok: None,
        lowered_to: None,
    },
    FoldedRead {
        tool: "get_system_health",
        arg: "host_trends",
        method: "GET",
        path: "/api/v1/system/hosts/:instance/metrics/range",
        perm: Some(Permission::View),
        inventory_ids_ok: None,
        opaque_ok: None,
        lowered_to: None,
    },
    FoldedRead {
        tool: "get_system_health",
        arg: "forwarding",
        method: "GET",
        path: "/api/v1/forwarding/status",
        // The one `ManageConfig` member of this family. It names every collector the deployment
        // tees to, which is closer to the forwarding configuration than to a health counter.
        perm: Some(Permission::ManageSystem),
        inventory_ids_ok: None,
        opaque_ok: None,
        lowered_to: None,
    },
    FoldedRead {
        tool: "get_system_health",
        arg: "credentials",
        method: "GET",
        path: "/api/v1/credentials/health",
        perm: Some(Permission::ManageCredentials),
        inventory_ids_ok: None,
        opaque_ok: None,
        lowered_to: None,
    },
    FoldedRead {
        tool: "get_system_health",
        arg: "upgrade",
        method: "GET",
        path: "/api/v1/system/upgrade",
        // The second `ManageConfig` member of this family, for the same reason as `forwarding`: it
        // answers with build provenance and — once the updater sidecar lands — the registry, the
        // resolved digests and the store images the target compose pins. That is the deployment's
        // configuration, not a health counter (ADR-050 decision 13).
        perm: Some(Permission::ManageSystem),
        inventory_ids_ok: None,
        opaque_ok: None,
        lowered_to: None,
    },
    FoldedRead {
        tool: "get_system_health",
        arg: "version",
        method: "GET",
        path: "/api/v1/version",
        perm: None,
        inventory_ids_ok: None,
        opaque_ok: None,
        lowered_to: None,
    },
    FoldedRead {
        tool: "get_system_health",
        arg: "deployment",
        method: "GET",
        path: "/api/v1/config",
        perm: None,
        inventory_ids_ok: None,
        opaque_ok: None,
        lowered_to: None,
    },
    // ── the single-purpose I3a tools ─────────────────────────────────────────
    FoldedRead {
        tool: "fleet_state_history",
        arg: "",
        method: "GET",
        path: "/api/v1/fleet/state-history",
        perm: Some(Permission::View),
        inventory_ids_ok: None,
        opaque_ok: None,
        lowered_to: None,
    },
    FoldedRead {
        tool: "get_report_runs",
        arg: "list",
        method: "GET",
        path: "/api/v1/reports/runs",
        perm: Some(Permission::View),
        inventory_ids_ok: None,
        opaque_ok: None,
        lowered_to: None,
    },
    FoldedRead {
        tool: "get_report_runs",
        arg: "detail",
        method: "GET",
        path: "/api/v1/reports/runs/:id",
        perm: Some(Permission::View),
        inventory_ids_ok: None,
        // `ReportRunDetail.result_json` is the one genuinely untyped blob on this surface: the
        // section renderers decide its keys, so the contract describes it as `{}` and the walk
        // below can say nothing about it. Kept rather than stripped, because the structured result
        // is the whole reason an AI client would read a saved run at all — the HTML is worse. What
        // bounds it is that the renderers are internal and none of them reads a credential: the
        // report catalog is a fixed list of aggregate sections over metrics, alerts and flow.
        opaque_ok: Some(
            "result_json is rendered section output whose keys the internal report renderers \
             decide; no renderer reads credential storage, and the structured result is the reason \
             the run is worth reading",
        ),
        lowered_to: None,
    },
    FoldedRead {
        tool: "get_audit",
        arg: "",
        method: "GET",
        path: "/api/v1/audit",
        // `ViewAudit`, not `View` — who acked at 3am is its own permission.
        perm: Some(Permission::ViewAudit),
        inventory_ids_ok: None,
        opaque_ok: None,
        lowered_to: None,
    },
    FoldedRead {
        tool: "get_dns_chain",
        arg: "current",
        method: "GET",
        path: "/api/v1/nodes/:node_id/dns-chain",
        perm: Some(Permission::View),
        inventory_ids_ok: None,
        opaque_ok: None,
        lowered_to: None,
    },
    FoldedRead {
        tool: "get_dns_chain",
        arg: "history",
        method: "GET",
        path: "/api/v1/nodes/:node_id/dns-chain/history",
        perm: Some(Permission::View),
        inventory_ids_ok: None,
        opaque_ok: None,
        lowered_to: None,
    },
    FoldedRead {
        tool: "list_node_metrics",
        // Single-branch, like `run_rca`: the row is here rather than in `TOOL_RESULT_TYPES` because
        // this table checks more for less — the permission is compared against the REST handler's
        // own extractor, and the forbidden-key check walks the response schema in the OpenAPI
        // document instead of whichever fields one hand-built instance happened to populate.
        arg: "",
        method: "GET",
        path: "/api/v1/nodes/:node_id/metrics",
        perm: Some(Permission::View),
        inventory_ids_ok: None,
        opaque_ok: None,
        lowered_to: None,
    },
    FoldedRead {
        tool: "run_rca",
        arg: "",
        // The only non-GET row: a read wearing POST, like `POST /analysis/jobs`. It changes no
        // configuration, so it does not touch the frozen write surface (ADR-042 decision 6).
        method: "POST",
        path: "/api/v1/rca",
        perm: Some(Permission::AckAlerts),
        // Found by the schema walk, not predicted: the stored evidence embeds `NodeFacts`, which
        // carries the node's pool. Deliberately *not* the `POOL_IS_THE_ANSWER` reason — nobody
        // asked this tool which poller owns anything. The argument is different, so the sentence
        // is different: a saved report is replayed verbatim, the same bytes the UI shows, and
        // rewriting it on one surface would make the two disagree about what was analysed.
        inventory_ids_ok: Some(
            "a stored report is replayed verbatim on both surfaces; its evidence names the node's \
             pool as recorded at generation time, and rewriting that for MCP alone would make the \
             two surfaces disagree about what the model was shown",
        ),
        opaque_ok: None,
        lowered_to: None,
    },
    // ── branches folded into tools that already existed ──────────────────────
    FoldedRead {
        tool: "get_fleet_summary",
        arg: "coverage",
        method: "GET",
        path: "/api/v1/fleet/coverage",
        perm: Some(Permission::View),
        inventory_ids_ok: None,
        opaque_ok: None,
        lowered_to: None,
    },
    // ── get_config(kind=…) — ADR-042 I3b ─────────────────────────────────────
    //
    // The configuration-read family, 28 routes behind one `kind`. This is the block that proves the
    // module doc's point about permission: it spans `ManageConfig` ×10, `View` ×12,
    // `ManageSystem` ×4 and `ManageUsers` ×2, and one permission for the tool would either hand the
    // identity-provider configuration to any viewer or refuse a viewer eleven reads the WebUI
    // already shows them.
    //
    // Order matches `ConfigKind::NAMES`, which matches the order the tool's description lists them,
    // so all three can be read side by side.
    FoldedRead {
        tool: "get_config",
        arg: "thresholds",
        method: "GET",
        path: "/api/v1/thresholds",
        perm: Some(Permission::ManageConfig),
        inventory_ids_ok: None,
        opaque_ok: None,
        lowered_to: None,
    },
    FoldedRead {
        tool: "get_config",
        arg: "event_rules",
        method: "GET",
        path: "/api/v1/event-rules",
        perm: Some(Permission::ManageConfig),
        inventory_ids_ok: None,
        opaque_ok: None,
        lowered_to: None,
    },
    FoldedRead {
        tool: "get_config",
        arg: "event_sources",
        method: "GET",
        path: "/api/v1/event-sources",
        perm: Some(Permission::ManageConfig),
        inventory_ids_ok: None,
        opaque_ok: None,
        lowered_to: None,
    },
    FoldedRead {
        tool: "get_config",
        arg: "notification_channels",
        method: "GET",
        path: "/api/v1/notification-channels",
        perm: Some(Permission::ManageSystem),
        inventory_ids_ok: None,
        opaque_ok: None,
        lowered_to: None,
    },
    FoldedRead {
        tool: "get_config",
        arg: "routing_rules",
        method: "GET",
        path: "/api/v1/routing-rules",
        perm: Some(Permission::ManageSystem),
        inventory_ids_ok: None,
        opaque_ok: None,
        lowered_to: None,
    },
    FoldedRead {
        tool: "get_config",
        arg: "profiles",
        method: "GET",
        path: "/api/v1/profiles",
        perm: Some(Permission::ManageConfig),
        inventory_ids_ok: None,
        opaque_ok: None,
        lowered_to: None,
    },
    FoldedRead {
        tool: "get_config",
        arg: "profile_templates",
        method: "GET",
        path: "/api/v1/profiles/:id/templates",
        perm: Some(Permission::ManageConfig),
        inventory_ids_ok: None,
        opaque_ok: None,
        lowered_to: None,
    },
    FoldedRead {
        tool: "get_config",
        arg: "collection_templates",
        method: "GET",
        path: "/api/v1/collection-templates",
        perm: Some(Permission::ManageConfig),
        inventory_ids_ok: None,
        opaque_ok: None,
        lowered_to: None,
    },
    FoldedRead {
        tool: "get_config",
        arg: "template_items",
        method: "GET",
        path: "/api/v1/collection-templates/:id/items",
        perm: Some(Permission::ManageConfig),
        inventory_ids_ok: None,
        opaque_ok: None,
        lowered_to: None,
    },
    FoldedRead {
        tool: "get_config",
        arg: "node_collection",
        method: "GET",
        path: "/api/v1/nodes/:node_id/collection",
        perm: Some(Permission::ManageConfig),
        inventory_ids_ok: None,
        opaque_ok: None,
        lowered_to: None,
    },
    FoldedRead {
        tool: "get_config",
        arg: "classification_rules",
        method: "GET",
        path: "/api/v1/classification-rules",
        perm: Some(Permission::ManageConfig),
        inventory_ids_ok: None,
        opaque_ok: None,
        lowered_to: None,
    },
    FoldedRead {
        tool: "get_config",
        arg: "mib_catalog",
        method: "GET",
        path: "/api/v1/mib-catalog",
        // `View`, not `ManageConfig`: the catalog is reference data the collection editor reads,
        // and the REST edge has always let any viewer browse it.
        perm: Some(Permission::View),
        inventory_ids_ok: None,
        opaque_ok: None,
        lowered_to: None,
    },
    FoldedRead {
        tool: "get_config",
        arg: "url_check",
        method: "GET",
        path: "/api/v1/nodes/:node_id/url-check",
        perm: Some(Permission::View),
        inventory_ids_ok: None,
        opaque_ok: None,
        // The one lowered row on this surface, and the reason the field exists at all. ADR-042
        // I3b's plan was "drop `credential` on the MCP side", which the walk above cannot see: it
        // reads the *route's* 200 schema. Lowering it on the REST body instead is not available —
        // `checkConfigForm.ts` prefills the credential selector from this field and PUTs the whole
        // config back, and that PUT is a replace, so a form that could not prefill the binding
        // would clear it every time an operator edited a timeout.
        lowered_to: Some((
            "UrlCheckDto",
            "the REST body must keep `credential` because the WebUI's edit form round-trips the \
             binding through a replacing PUT; the id is a reference rather than a secret, but it \
             is one a model can neither resolve nor use, so this surface answers the question it \
             actually has — whether the probe is authenticated — with has_credential",
        )),
    },
    FoldedRead {
        tool: "get_config",
        arg: "dns_check",
        method: "GET",
        path: "/api/v1/nodes/:node_id/dns-check",
        perm: Some(Permission::View),
        inventory_ids_ok: None,
        opaque_ok: None,
        lowered_to: None,
    },
    FoldedRead {
        tool: "get_config",
        arg: "discovery_candidates",
        method: "GET",
        path: "/api/v1/discovery/candidates",
        perm: Some(Permission::View),
        inventory_ids_ok: None,
        opaque_ok: None,
        lowered_to: None,
    },
    FoldedRead {
        tool: "get_config",
        arg: "discovery_scans",
        method: "GET",
        path: "/api/v1/discovery/scans",
        perm: Some(Permission::ManageConfig),
        // The shared sentence fits exactly: a scan row's `pool` is the route the sweep was
        // published on, so "which poller ran this sweep" *is* the question — and ADR-068 exists
        // because that answer was previously nobody's to give. Stripping it would leave a caller
        // unable to tell a sweep that ran at the remote site from one that ran at head office.
        inventory_ids_ok: POOL_IS_THE_ANSWER,
        opaque_ok: None,
        lowered_to: None,
    },
    FoldedRead {
        tool: "get_config",
        arg: "discovery_scan",
        method: "GET",
        path: "/api/v1/discovery/scan/:id",
        perm: Some(Permission::ManageConfig),
        // Same sentence, same reason as `discovery_scans`: ADR-068 gave a scan the route it was
        // published on, and that route is the answer to "where did this sweep run from" — the
        // question the whole increment exists to make answerable.
        inventory_ids_ok: POOL_IS_THE_ANSWER,
        opaque_ok: None,
        lowered_to: None,
    },
    FoldedRead {
        tool: "get_config",
        arg: "meraki_orgs",
        method: "GET",
        path: "/api/v1/meraki/orgs",
        perm: Some(Permission::View),
        inventory_ids_ok: None,
        opaque_ok: None,
        lowered_to: None,
    },
    FoldedRead {
        tool: "get_config",
        arg: "meraki_networks",
        method: "GET",
        path: "/api/v1/meraki/orgs/:id/networks",
        perm: Some(Permission::View),
        inventory_ids_ok: None,
        opaque_ok: None,
        lowered_to: None,
    },
    FoldedRead {
        tool: "get_config",
        arg: "meraki_polling",
        method: "GET",
        path: "/api/v1/meraki/polling",
        perm: Some(Permission::View),
        inventory_ids_ok: None,
        opaque_ok: None,
        lowered_to: None,
    },
    FoldedRead {
        tool: "get_config",
        arg: "forward_destinations",
        method: "GET",
        path: "/api/v1/forwarding/destinations",
        perm: Some(Permission::ManageSystem),
        // Not `POOL_IS_THE_ANSWER`: nobody asked this branch which poller owns anything. The
        // argument is its own, so the sentence is its own.
        inventory_ids_ok: Some(
            "`pool` on a forwarding destination is a field of the configuration being read rather \
             than an inventory tag on a monitored device — it is the operator's restriction of \
             this tee to one poller pool. A destination reported without it reads as fleet-wide \
             when only one pool's stream is forwarded, which is a wrong answer, not a redacted one",
        ),
        opaque_ok: None,
        lowered_to: None,
    },
    FoldedRead {
        tool: "get_config",
        arg: "report_definitions",
        method: "GET",
        path: "/api/v1/reports/definitions",
        perm: Some(Permission::View),
        inventory_ids_ok: None,
        // Not fixable with `#[schema(value_type = ReportSpec)]`, which is the usual answer: that
        // type is private, `Deserialize`-only and deliberately lenient so a newer WebUI shape stays
        // readable by an older core (ADR-017), and its `SectionSpec.settings` is itself untyped.
        // Describing it would freeze a document core does not own and move the blind spot down one
        // level rather than closing it.
        opaque_ok: Some(
            "`spec` is the report document the WebUI owns and core parses leniently so a newer \
             builder stays compatible with an older core; describing it would freeze a shape core \
             does not own, and its `settings` is itself untyped. What bounds it is the section \
             catalog: a spec names section kinds, a range and display settings, and no section \
             kind reads credential storage",
        ),
        lowered_to: None,
    },
    FoldedRead {
        tool: "get_config",
        arg: "report_schedules",
        method: "GET",
        path: "/api/v1/reports/schedules",
        perm: Some(Permission::View),
        inventory_ids_ok: None,
        opaque_ok: None,
        lowered_to: None,
    },
    FoldedRead {
        tool: "get_config",
        arg: "retention",
        method: "GET",
        path: "/api/v1/settings/retention",
        perm: Some(Permission::View),
        inventory_ids_ok: None,
        opaque_ok: None,
        lowered_to: None,
    },
    FoldedRead {
        tool: "get_config",
        arg: "adjacency_settings",
        method: "GET",
        path: "/api/v1/settings/neighbors",
        perm: Some(Permission::View),
        inventory_ids_ok: None,
        opaque_ok: None,
        lowered_to: None,
    },
    FoldedRead {
        tool: "get_config",
        arg: "llm",
        method: "GET",
        path: "/api/v1/llm/config",
        perm: Some(Permission::ManageSystem),
        inventory_ids_ok: None,
        opaque_ok: None,
        lowered_to: None,
    },
    FoldedRead {
        tool: "get_config",
        arg: "roles",
        method: "GET",
        path: "/api/v1/roles",
        // The shape of the permission model, not anyone's account — hence `View` where the rest of
        // `api/users.rs` is `ManageUsers`.
        perm: Some(Permission::View),
        inventory_ids_ok: None,
        opaque_ok: None,
        lowered_to: None,
    },
    FoldedRead {
        tool: "get_config",
        arg: "oidc",
        method: "GET",
        path: "/api/v1/settings/oidc",
        // `ManageUsers`, not `ManageConfig`: an identity provider is account plumbing.
        perm: Some(Permission::ManageUsers),
        inventory_ids_ok: None,
        opaque_ok: None,
        lowered_to: None,
    },
    FoldedRead {
        tool: "get_config",
        arg: "ldap",
        method: "GET",
        path: "/api/v1/settings/ldap",
        perm: Some(Permission::ManageUsers),
        inventory_ids_ok: None,
        opaque_ok: None,
        lowered_to: None,
    },
];

/// The permission one branch demands, or `Permission::View` for the two whose REST counterpart is
/// unauthenticated (see [`FoldedRead::perm`]).
///
/// Panics if `arg` is not a branch of `tool` — the caller is a `match` in the same crate whose arms
/// are pinned to this table by [`tests::every_folded_branch_is_reachable_from_its_tool`], so an
/// unknown pair is a programming error rather than caller input.
pub(crate) fn required_permission(tool: &str, arg: &str) -> Permission {
    FOLDED_READS
        .iter()
        .find(|f| f.tool == tool && f.arg == arg)
        .unwrap_or_else(|| panic!("no folded read `{tool}`/`{arg}`"))
        .perm
        .unwrap_or(Permission::View)
}

/// [`required_permission`] for a caller that may be holding **model output** (ADR-028 WS-G).
///
/// `None` means no such branch, which the RCA agent must treat as "let the tool refuse it with its
/// own vocabulary" rather than as an error of its own — the tool's message lists the valid values
/// and the agent's would not. Separate from [`required_permission`] because that one's panic is
/// correct where the caller is a `match` this table pins, and is never correct where the caller is
/// a language model.
pub(crate) fn permission_of(tool: &str, arg: &str) -> Option<Permission> {
    FOLDED_READS
        .iter()
        .find(|f| f.tool == tool && f.arg == arg)
        .map(|f| f.perm.unwrap_or(Permission::View))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    /// Keys that must never appear in a tool result, at any depth (ADR-018). Kept in step with
    /// `dto.rs`'s copy by [`the_two_key_lists_agree_with_the_canary`].
    const SECRET_KEYS: &[&str] = &[
        "credential",
        "community",
        "password",
        "token",
        "auth_key",
        "priv_key",
        "secret",
    ];
    const INVENTORY_NOISE_KEYS: &[&str] = &["pool", "profile"];

    /// The ledger path (`:id`) for an OpenAPI path (`{id}`) — the same normalization
    /// `route_table::documented` uses.
    fn ledger_path(openapi_path: &str) -> String {
        openapi_path
            .split('/')
            .map(
                |seg| match seg.strip_prefix('{').and_then(|s| s.strip_suffix('}')) {
                    Some(name) => format!(":{name}"),
                    None => seg.to_owned(),
                },
            )
            .collect::<Vec<_>>()
            .join("/")
    }

    /// The OpenAPI document as JSON, so schemas can be walked with `serde_json` rather than by
    /// pattern-matching utoipa's `RefOr<Schema>` tree.
    fn document() -> serde_json::Value {
        serde_json::to_value(crate::api::openapi::document())
            .expect("the OpenAPI document serializes")
    }

    /// The operation object for one folded row, found by ledger path + method.
    fn operation_of<'a>(
        doc: &'a serde_json::Value,
        f: &FoldedRead,
    ) -> Option<&'a serde_json::Value> {
        let paths = doc.get("paths")?.as_object()?;
        let (_, item) = paths.iter().find(|(p, _)| ledger_path(p) == f.path)?;
        item.get(f.method.to_lowercase())
    }

    /// What a walk of one response schema found.
    #[derive(Default)]
    struct Walked {
        /// Every property name reachable from the schema, at any depth.
        keys: BTreeSet<String>,
        /// Properties the contract describes as `{}` — no type, no `$ref`, nothing. The key check
        /// is blind inside these, so they are reported rather than skipped.
        opaque: BTreeSet<String>,
    }

    /// Walk a response schema, following `$ref` into `components.schemas`. Cycles are cut by the
    /// visited set.
    fn walk_schema(doc: &serde_json::Value, schema: &serde_json::Value) -> Walked {
        let mut out = Walked::default();
        let mut seen: BTreeSet<String> = BTreeSet::new();
        walk(doc, schema, &mut seen, &mut out, 0);
        out
    }

    /// Whether a schema says nothing at all about its value.
    fn is_opaque(schema: &serde_json::Value) -> bool {
        schema
            .as_object()
            .is_some_and(|o| !o.keys().any(|k| k != "description" && k != "title"))
    }

    fn walk(
        doc: &serde_json::Value,
        schema: &serde_json::Value,
        seen: &mut BTreeSet<String>,
        out: &mut Walked,
        depth: usize,
    ) {
        if depth > 16 {
            return;
        }
        if let Some(r) = schema.get("$ref").and_then(|v| v.as_str()) {
            let name = r.rsplit('/').next().unwrap_or(r).to_owned();
            if !seen.insert(name.clone()) {
                return;
            }
            if let Some(target) = doc.pointer(&format!("/components/schemas/{name}")) {
                walk(doc, target, seen, out, depth + 1);
            }
            return;
        }
        if let Some(props) = schema.get("properties").and_then(|v| v.as_object()) {
            for (k, v) in props {
                out.keys.insert(k.clone());
                if is_opaque(v) {
                    out.opaque.insert(k.clone());
                }
                walk(doc, v, seen, out, depth + 1);
            }
        }
        for key in ["items", "additionalProperties"] {
            if let Some(v) = schema.get(key) {
                if v.is_object() {
                    walk(doc, v, seen, out, depth + 1);
                }
            }
        }
        for key in ["allOf", "anyOf", "oneOf"] {
            if let Some(arr) = schema.get(key).and_then(|v| v.as_array()) {
                for v in arr {
                    walk(doc, v, seen, out, depth + 1);
                }
            }
        }
    }

    /// **Every folded branch mirrors a route the ledger says this tool serves.**
    ///
    /// Catches the drift where a branch is renamed, or points at a path that no longer exists, or
    /// is served by a tool other than the one the ledger credits — any of which would leave the
    /// permission and canary rows below checking the wrong endpoint.
    #[test]
    fn every_folded_read_is_claimed_by_its_ledger_line() {
        use crate::api::route_table::ROUTES;
        for f in FOLDED_READS {
            let row = ROUTES
                .iter()
                .find(|(m, p, _, _)| *m == f.method && *p == f.path)
                .unwrap_or_else(|| {
                    panic!(
                        "folded read `{}`/`{}` names {} {}, which the route ledger does not list",
                        f.tool, f.arg, f.method, f.path
                    )
                });
            let named = match row.3 {
                crate::api::route_table::Mcp::Tool(name) => name,
                _ => panic!(
                    "{} {} is folded into `{}` but its ledger line is not Mcp::Tool — flip it and \
                     lower MCP_PENDING in the same commit",
                    f.method, f.path, f.tool
                ),
            };
            assert_eq!(
                named, f.tool,
                "{} {} is folded into `{}` but the ledger credits `{named}`",
                f.method, f.path, f.tool
            );
        }
    }

    /// **Every folded branch demands what its REST counterpart demands.**
    ///
    /// This is the check ADR-042 decision 2 said could not be written. It could not be written for
    /// the whole ledger — 237 rows, where the permission lives inside handler bodies and MCP tool
    /// bodies in two different spellings. Over the folded rows it can be, because the tool's side
    /// is a value in this file and the REST side is an extractor in a signature:
    ///
    /// `path` → the document's `operationId` (which is the handler's function name) → `async fn
    /// <name>(` in `api/*.rs` → the `Require*` type in its argument list.
    ///
    /// Skips a row it cannot parse and asserts a floor on how many it compared, so "the parser
    /// stopped matching" cannot read as "everything agrees" — the discipline
    /// `openapi.rs::every_documented_body_is_the_type_its_handler_returns` established.
    #[test]
    fn every_folded_read_demands_what_its_rest_route_demands() {
        let doc = document();
        let sources: Vec<(String, String)> =
            std::fs::read_dir(concat!(env!("CARGO_MANIFEST_DIR"), "/src/api"))
                .expect("api module directory")
                .filter_map(Result::ok)
                .filter(|e| e.path().extension().is_some_and(|x| x == "rs"))
                .filter_map(|e| {
                    let name = e.file_name().to_string_lossy().into_owned();
                    std::fs::read_to_string(e.path()).ok().map(|s| (name, s))
                })
                .collect();

        let marker = |p: Permission| match p {
            Permission::View => "RequireView",
            Permission::AckAlerts => "RequireAckAlerts",
            Permission::ManageMaintenance => "RequireManageMaintenance",
            Permission::ManageConfig => "RequireManageConfig",
            Permission::ManageCredentials => "RequireManageCredentials",
            Permission::ManageSystem => "RequireManageSystem",
            Permission::ManageUsers => "RequireManageUsers",
            Permission::ViewAudit => "RequireViewAudit",
        };

        let mut compared = 0usize;
        for f in FOLDED_READS {
            let Some(op) = operation_of(&doc, f) else {
                continue;
            };
            let Some(handler) = op.get("operationId").and_then(|v| v.as_str()) else {
                continue;
            };
            // A row whose REST side is deliberately unauthenticated: the document must say so,
            // rather than the table quietly claiming an exemption nobody granted.
            if f.perm.is_none() {
                let public = op
                    .get("security")
                    .and_then(|v| v.as_array())
                    .is_some_and(|a| {
                        a.len() == 1 && a[0].as_object().is_some_and(|o| o.is_empty())
                    });
                assert!(
                    public,
                    "folded read `{}`/`{}` claims {} {} needs no permission, but the contract does \
                     not mark that operation as unauthenticated",
                    f.tool, f.arg, f.method, f.path
                );
                compared += 1;
                continue;
            }
            let needle = format!("async fn {handler}(");
            let Some(sig) = sources.iter().find_map(|(_, src)| {
                let after = src.split(&needle).nth(1)?;
                after.split(')').next()
            }) else {
                continue;
            };
            let want = marker(f.perm.expect("checked above"));
            assert!(
                sig.contains(want),
                "folded read `{}`/`{}` demands {:?}, but its REST handler `{handler}` takes no \
                 `{want}` — one of the two is wrong, and if it is the table then MCP is either \
                 leaking this read or refusing a caller the UI would serve",
                f.tool,
                f.arg,
                f.perm.expect("checked above")
            );
            compared += 1;
        }
        assert!(
            compared >= 45,
            "only compared {compared} folded reads against their handlers; the parser drifted"
        );
    }

    /// **No folded branch can return a secret, checked against the contract rather than a sample.**
    ///
    /// The instance canary in `dto.rs` proves a *particular value* carries no forbidden key. That
    /// is weaker than it looks: a hand-built instance with `None` in an `Option<Inner>` hides every
    /// field of `Inner`, and the cost of building one is 8–25 lines per shape — which is why I3 was
    /// deferred once as "~20 hand-written instances".
    ///
    /// Walking the OpenAPI schema instead costs nothing per shape and sees every field whether or
    /// not a sample would have populated it. It is not a replacement: a `serde_json::Value` field
    /// is an empty schema, so the two types that have one keep their runtime instance. It is the
    /// stronger half everywhere else.
    #[test]
    fn every_folded_result_is_free_of_forbidden_keys() {
        let doc = document();
        let mut checked = 0usize;
        for f in FOLDED_READS {
            let Some(op) = operation_of(&doc, f) else {
                continue;
            };
            let Some(schema) = op.pointer("/responses/200/content/application~1json/schema") else {
                continue;
            };
            let Walked { keys, opaque } = walk_schema(&doc, schema);
            // An untyped blob is the one way this check can be blind. Reporting it turns "the walk
            // found nothing wrong" into "the walk could not look", which is a different sentence
            // and the only one worth trusting.
            match f.opaque_ok {
                None => assert!(
                    opaque.is_empty(),
                    "folded read `{}`/`{}` returns {opaque:?}, which the contract describes as an \
                     untyped blob — the key check cannot see inside it. Give the field a \
                     `#[schema(value_type = …)]` so its real shape is described, or record in \
                     `opaque_ok` why serving an unexaminable value here is acceptable",
                    f.tool,
                    f.arg
                ),
                Some(why) => {
                    assert!(
                        why.len() >= 30,
                        "`{}`/`{}` waves through an untyped blob without a real reason",
                        f.tool,
                        f.arg
                    );
                    assert!(
                        !opaque.is_empty(),
                        "`{}`/`{}` declares an opaque-field exemption but every field is now \
                         described; drop it rather than leaving it to cover a future blob",
                        f.tool,
                        f.arg
                    );
                }
            }
            // The secret rule, in one of two modes. Unlowered — every row but one — the route's own
            // schema *is* what the tool sends, so a banned key on it is a banned key served. A
            // lowered row says otherwise, and then the obligation moves to `dto.rs` rather than
            // lifting: the named type has to exist there, where the instance canary covers it.
            match f.lowered_to {
                None => {
                    for bad in SECRET_KEYS {
                        assert!(
                            !keys.contains(*bad),
                            "folded read `{}`/`{}` ({} {}) returns a field named {bad:?}; a tool \
                             must not serve it (ADR-018) — give the MCP surface a sanitized DTO \
                             and name it in `lowered_to`",
                            f.tool,
                            f.arg,
                            f.method,
                            f.path
                        );
                    }
                }
                Some((dto, why)) => {
                    assert!(
                        why.len() >= 30,
                        "`{}`/`{}` lowers a key away without a real reason",
                        f.tool,
                        f.arg
                    );
                    // Load-bearing, same discipline as `inventory_ids_ok`: a claim granted where
                    // the REST body carries nothing to lower is a stale claim that would silently
                    // cover a field somebody adds later.
                    assert!(
                        SECRET_KEYS.iter().any(|k| keys.contains(*k)),
                        "`{}`/`{}` names a sanitized DTO, but its REST body carries no \
                         secret-shaped key; drop `lowered_to` and serve the route's own type",
                        f.tool,
                        f.arg
                    );
                    // Without this the row would be an assertion about a type nobody checks. With
                    // it, `dto.rs`'s canary is guaranteed to be the thing that checks it.
                    assert!(
                        include_str!("dto.rs").contains(&format!("pub struct {dto}")),
                        "`{}`/`{}` names `{dto}` as its sanitized result, but mcp/dto.rs declares \
                         no such type — nothing would then check what this branch returns",
                        f.tool,
                        f.arg
                    );
                }
            }
            match f.inventory_ids_ok {
                None => {
                    for bad in INVENTORY_NOISE_KEYS {
                        assert!(
                            !keys.contains(*bad),
                            "folded read `{}`/`{}` returns {bad:?}, which is inventory noise on \
                             anything but a poller-assignment answer; if it *is* the answer here, \
                             say so in `inventory_ids_ok`",
                            f.tool,
                            f.arg
                        );
                    }
                }
                Some(why) => {
                    assert!(
                        why.len() >= 30,
                        "`{}`/`{}` exempts itself from the inventory-id rule without a real reason",
                        f.tool,
                        f.arg
                    );
                    // The exemption must be load-bearing. One granted where the key never appears
                    // is a stale claim that would silently cover a future field.
                    assert!(
                        INVENTORY_NOISE_KEYS.iter().any(|k| keys.contains(*k)),
                        "`{}`/`{}` claims an inventory-id exemption but returns neither `pool` nor \
                         `profile`; drop the exemption rather than leaving it to cover a field \
                         nobody has looked at",
                        f.tool,
                        f.arg
                    );
                }
            }
            checked += 1;
        }
        assert!(
            checked >= 45,
            "only walked {checked} folded response schemas; the parser drifted"
        );
    }

    /// The two key lists here and in `dto.rs` are the same rule; nothing but this makes them agree.
    #[test]
    fn the_two_key_lists_agree_with_the_canary() {
        let dto_src = include_str!("dto.rs");
        for k in SECRET_KEYS.iter().chain(INVENTORY_NOISE_KEYS) {
            assert!(
                dto_src.contains(&format!("\"{k}\"")),
                "`{k}` is banned here but absent from dto.rs's lists — one of the two drifted"
            );
        }
    }

    /// Every branch this table declares is one the tool actually dispatches on.
    ///
    /// One-directional on purpose: the reverse (a tool arm with no row) is caught by
    /// [`every_folded_read_is_claimed_by_its_ledger_line`] via the ledger, which cannot be
    /// satisfied without a row here.
    #[test]
    fn every_folded_branch_is_reachable_from_its_tool() {
        let tools_src = include_str!("tools.rs");
        for f in FOLDED_READS.iter().filter(|f| !f.arg.is_empty()) {
            assert!(
                tools_src.contains(&format!("\"{}\"", f.arg)),
                "folded branch `{}`/`{}` appears in no arm of tools.rs — a caller naming it would \
                 be told it is unknown while this table says it exists",
                f.tool,
                f.arg
            );
        }
    }

    #[test]
    fn required_permission_falls_back_to_view_for_the_unauthenticated_reads() {
        assert_eq!(
            required_permission("get_system_health", "version"),
            Permission::View,
            "an unauthenticated REST route still needs View over MCP"
        );
        assert_eq!(required_permission("get_audit", ""), Permission::ViewAudit);
    }
}
