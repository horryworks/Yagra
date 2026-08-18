// SPDX-License-Identifier: AGPL-3.0-only
// API types — the northbound contract, taken from the generated OpenAPI schema (ADR-035).
//
// This file used to be a hand-written mirror of the Rust serde shapes, headed by an instruction to
// "keep these in sync when an endpoint or a shared type changes". Nothing enforced that, and a
// drifted mirror is never a compile error — it is a user seeing `undefined`. The chain is now:
// Rust handlers (`#[utoipa::path]` / `ToSchema`) → `web/src/api/openapi.json` →
// `web/src/api/schema.d.ts` (`npm run generate:api`) → this file.
//
// Exactly three kinds of declaration belong here:
//
//   1. **Re-exports of a generated schema.** The TypeScript name is kept even where the Rust schema
//      name differs (`RoleMatrix` ⟷ `RolesMatrix`, `CollectionTemplate` ⟷ `TemplateSummary`,
//      `MaintenanceWindow` ⟷ `StoredWindow`, …) because ~84 files import these names. The alias is
//      the only hand-written part of the shape, and the compiler checks it resolves.
//   2. **The runtime `as const` enum arrays.** These must exist at runtime — `i18nEnumKeys.test.ts`
//      iterates them to prove *both* locales carry a string for every variant (`extensibility.md`
//      §4), which a re-exported union cannot do. Each is pinned to its schema union in both
//      directions by `schemaEnumPins` below, so a backend enum that gains or loses a variant fails
//      to compile here rather than rendering a raw i18n key to an operator.
//   3. **Shapes with no backend counterpart** — query-parameter unions, filter bags, documents the
//      WebUI owns end to end, and the payloads Rust serializes as `serde_json::Value`. They are
//      grouped at the bottom of the file under their own banner, each with the reason it survives.
//
// **Never hand-write an `interface` for something the API returns.** If a shape is missing or wrong,
// fix the Rust annotation and regenerate — adding it here recreates the mirror ADR-035 deleted.

// `paths` as well as `components`: a query-parameter bag is described by the operation, not by a
// schema, so a filter type derived from it is still generated rather than hand-written (kind 1
// above, reached through the other door).
import type { components, paths } from '../api/schema';

/** Compile-time equality. Resolves to `true` only when A and B are the same type in *both*
 *  directions, and to `never` otherwise — so assigning `true` to it stops compiling the moment
 *  either side gains or loses a member. */
type AssertEqual<A, B> = [A] extends [B] ? ([B] extends [A] ? true : never) : never;

/** Pins each runtime enum array in this file to the generated schema union it must equal.
 *
 *  Grouped into one object (rather than a const beside each array) because `noUnusedLocals` and
 *  eslint both reject a declaration nothing reads, and `void`-ed rather than exported so this
 *  file's export surface stays exactly the types consumers import. A backend enum that changes
 *  makes exactly one property here fail to compile, naming the enum — which is also the signal
 *  that `i18nEnumKeys.test.ts` is about to demand EN/JA strings for the new variant.
 *
 *  `GROUP_TYPES` is deliberately absent: Rust serializes `group_type` as a bare `String`, so the
 *  document has no union to pin it to. See its declaration below. */
const schemaEnumPins: {
  Severity: AssertEqual<Severity, components['schemas']['Severity']>;
  Role: AssertEqual<Role, components['schemas']['Role']>;
  ScopeLevel: AssertEqual<ScopeLevel, components['schemas']['ScopeLevel']>;
  Direction: AssertEqual<Direction, components['schemas']['Direction']>;
  ForwardSourceKind: AssertEqual<ForwardSourceKind, components['schemas']['SourceKind']>;
  ForwardDestKind: AssertEqual<ForwardDestKind, components['schemas']['DestKind']>;
  ForwardFilterMode: AssertEqual<ForwardFilterMode, components['schemas']['FilterMode']>;
  NodeKind: AssertEqual<NodeKind, components['schemas']['NodeKind']>;
  ReportRunState: AssertEqual<ReportRunState, components['schemas']['ReportRunState']>;
  ReportTrigger: AssertEqual<ReportTrigger, components['schemas']['ReportRunTrigger']>;
  Cadence: AssertEqual<Cadence, components['schemas']['Cadence']>;
  AnalysisScheduleStatus: AssertEqual<AnalysisScheduleStatus, components['schemas']['AnalysisScheduleStatus']>;
  AnalysisJobState: AssertEqual<AnalysisJobState, components['schemas']['AnalysisJobState']>;
  DiscoveryScanState: AssertEqual<DiscoveryScanState, components['schemas']['DiscoveryScanState']>;
  EventKind: AssertEqual<EventKind, components['schemas']['EventKind']>;
  EventAction: AssertEqual<EventAction, components['schemas']['EventAction']>;
  EventMatchKind: AssertEqual<EventMatchKind, components['schemas']['EventMatchKind']>;
  DnsFailureKind: AssertEqual<DnsFailureKind, components['schemas']['DnsFailureKind']>;
  AuditAction: AssertEqual<AuditAction, components['schemas']['AuditAction']>;
  AuditStatusClass: AssertEqual<AuditStatusClass, components['schemas']['AuditStatusClass']>;
  ChannelKind: AssertEqual<ChannelKind, components['schemas']['ChannelKind']>;
} = {
  Severity: true,
  Role: true,
  ScopeLevel: true,
  Direction: true,
  ForwardSourceKind: true,
  ForwardDestKind: true,
  ForwardFilterMode: true,
  NodeKind: true,
  ReportRunState: true,
  ReportTrigger: true,
  Cadence: true,
  AnalysisScheduleStatus: true,
  AnalysisJobState: true,
  DiscoveryScanState: true,
  EventKind: true,
  EventAction: true,
  EventMatchKind: true,
  DnsFailureKind: true,
  AuditAction: true,
  AuditStatusClass: true,
  ChannelKind: true,
};
void schemaEnumPins;

// ── Node state, metrics ─────────────────────────────────────────────────────────────────────────

/** The node/check state machine. The ordered runtime list lives in `lib/nodeState.ts`
 *  (`SEVERITY_ORDER`) — it is a display order, not part of the wire contract. */
export type NodeState = components['schemas']['NodeState'];

/** Every alert severity, ordered info < warning < critical. `Severity` derives from it, so the
 *  union and the runtime list can never disagree — and the i18n key-coverage test iterates the
 *  list, so a new severity without its `format:severity.*` strings fails the suite. */
export const SEVERITIES = ['info', 'warning', 'critical'] as const;

/** Alert severity, ordered info < warning < critical. Pinned to `schemas.Severity`. */
export type Severity = (typeof SEVERITIES)[number];

/** Latest reading for one node metric (`GET /api/v1/nodes/:id/metrics/:metric`). */
export type MetricReading = components['schemas']['MetricReading'];

/** One time-series point: `t` Unix seconds, `v` value. */
export type MetricPoint = components['schemas']['MetricPoint'];

/** A time-series window (`GET /api/v1/nodes/:id/metrics/:metric/range`). */
export type MetricRange = components['schemas']['MetricRange'];

/** One metric a node collects or has data for (`GET /api/v1/nodes/:id/metrics`). */
export type NodeMetricEntry = components['schemas']['NodeMetricEntry'];

/** Whether a metric is configured, has data, or both. See `METRIC_STATUSES` for the runtime list. */
export type MetricStatus = components['schemas']['MetricStatus'];

/** What dimension a metric's series carry, which decides how it can be read.
 *  (`MetricKind` — gauge vs counter — is declared with the collection types further down.) */
export type MetricDimension = components['schemas']['MetricDimension'];

/** The metric statuses, as an array so per-member i18n coverage is testable at runtime
 *  (`extensibility.md` §4 — EN/JA parity cannot prove either locale is complete). */
export const METRIC_STATUSES = ['ok', 'no_data', 'unconfigured'] as const;

/** The metric dimensions, as an array, for the same reason as `METRIC_STATUSES`. */
export const METRIC_DIMENSIONS = ['none', 'interface', 'entity'] as const;

/** One ranked node in a fleet Top-N result (`GET /api/v1/metrics/top`). `name` is joined from
 *  PostgreSQL (TSDB carries only the id); it falls back to the id for a since-deleted node. */
export type TopEntry = components['schemas']['TopEntry'];

/** A Top-N result: the rows, plus whether the list may be **short for this caller**.
 *
 *  Rankings are produced by stores that know nothing about folder groups, so a group-scoped
 *  account's list is over-fetched and then filtered (ADR-014). When the filter eats the over-fetch
 *  the list comes back short — and a short list is indistinguishable from "that is all there is"
 *  unless the response says so. `partial: true` is what widgets render the "may be incomplete"
 *  note from. Always `false` for an unrestricted account. */
export type RankedNodes = components['schemas']['Ranked_TopEntry'];
export type RankedInterfaces = components['schemas']['Ranked_InterfaceTopEntry'];
export type RankedAlertNodes = components['schemas']['Ranked_AlertNodeCount'];

// ── Flow analysis (ADR-031) ─────────────────────────────────────────────────────────────────────

/** A top-talker: one host address with summed traffic (`/nodes/:id/flow/top-talkers`). */
export type FlowTalker = components['schemas']['FlowTalker'];

/** A src→dst conversation with summed traffic (`/nodes/:id/flow/conversations`). `*_asn` 0 = unknown;
 *  `*_as_name` is filled when the IP→ASN table resolves it. */
export type FlowConversation = components['schemas']['FlowConversation'];

/** A destination-port aggregate (`/nodes/:id/flow/top-ports`). */
export type FlowPortAgg = components['schemas']['FlowPortAgg'];

/** A protocol aggregate (`/nodes/:id/flow/protocols`). `proto` is the IP protocol number. */
export type FlowProtoAgg = components['schemas']['FlowProtoAgg'];

/** A trend point: bytes/packets for one protocol at one 5-minute bucket (`/nodes/:id/flow/series`). */
export type FlowPoint = components['schemas']['FlowPoint'];

/** An autonomous-system aggregate (`/nodes/:id/flow/top-as`). `asn` 0 = unknown; `name` when
 *  the IP→ASN table resolves it (else `null`). */
export type FlowAsAgg = components['schemas']['FlowAsAgg'];

/** One ranked interface in a fleet interface Top-N. `value` is bits/sec for throughput metrics,
 *  errors|discards per second otherwise; node/interface names joined from PostgreSQL. */
export type InterfaceTopEntry = components['schemas']['InterfaceTopEntry'];

// ── Alerts ──────────────────────────────────────────────────────────────────────────────────────

/**
 * Inbound acknowledgement state mirrored from an external incident tool (PagerDuty / JSM),
 * shown read-only (ADR-015). Yagra has no ack action of its own — this is reflected in only.
 */
export type AckView = components['schemas']['AckView'];

/** An alert as served by `GET /api/v1/alerts` — the engine's alert plus inbound ack state (the
 *  Rust type is `ActiveAlertView`; `schemas.Alert` is the bare engine alert without `acked`, which
 *  is what `NodeStatus.alerts` carries). */
export type Alert = components['schemas']['ActiveAlertView'];

/** A node row for inventory listings. */
export type NodeSummary = components['schemas']['NodeSummary'];

/** One resolved node id → display name (`POST /api/v1/node-names`). Unresolved ids are omitted
 *  from the response, so the caller keeps the raw id as the fallback (S12). */
export type NodeNameEntry = components['schemas']['NodeNameEntry'];

/** One node-picker search hit (`GET /api/v1/nodes/search`): id + display name + address only —
 *  enough to show and select a node without loading the whole inventory client-side (A-2). */
export type NodeSearchResult = components['schemas']['NodeSearchResult'];

/** One group's direct member nodes (`GET /api/v1/nodes/by-group?group=<id>`; absent group ⇒ the
 *  ungrouped bucket). The inventory tree lazy-loads a group's members only when it is expanded, so
 *  the initial view never pulls the whole fleet (A-3). `truncated` ⇒ the group exceeded the load
 *  backstop (a pathologically large flat group). */
export type GroupNodesResult = components['schemas']['GroupNodes'];

/** Every node-group type, in picker order.
 *
 *  **Not pinned to the schema:** Rust models `GroupSummary.group_type` as a `String` (validated
 *  against `GroupType` at the write edge), so the generated document has no union to compare with.
 *  This array is therefore the only closed statement of the set anywhere in TypeScript — treat it
 *  as unverified and check `yagra_core::groups::GroupType` when changing it. */
export const GROUP_TYPES = ['site', 'region', 'device_type', 'service', 'generic'] as const;

/** A node-group type (snake_case) — drives the tree icon. */
export type GroupType = (typeof GROUP_TYPES)[number];

/** One node group (folder) in the hierarchical inventory tree (`GET /api/v1/node-groups`). */
export type NodeGroup = components['schemas']['GroupSummary'];

/** One node's live status (`GET /api/v1/nodes/:id/status`): rolled-up display state plus the
 *  alerts currently attributed to it (bare engine alerts — no ack state). */
export type NodeStatus = components['schemas']['NodeStatus'];

/** Credential metadata (never the secret value). */
export type CredentialSummary = components['schemas']['CredentialSummary'];

/** A page of the keyset-paginated node list (`GET /api/v1/nodes`). */
export type NodePage = components['schemas']['NodePage'];

// ── Identity, roles, tokens ─────────────────────────────────────────────────────────────────────

/** Current principal (`GET /api/v1/auth/me`). */
export type AuthMe = components['schemas']['AuthMe'];

/** Every predefined role, least → most privileged — the order role pickers offer them in.
 *  `Role` is derived from it, so the union and the list can never disagree. (Users, API tokens and
 *  the OIDC group mapping each used to carry their own literal copy.) */
export const ROLES = ['viewer', 'operator', 'admin'] as const;

/** A predefined role (snake_case), ordered least → most privileged. Pinned to `schemas.Role`. */
export type Role = (typeof ROLES)[number];

/** A discrete capability the API edge checks. Generated from the Rust `Permission` enum, so a
 *  permission this UI names is one the server has (ADR-056 Inc.2) — the alternative, a hand-typed
 *  string, fails *closed*: a typo hides the control from everyone including an admin. */
export type Permission = components['schemas']['Permission'];

/** One capability in the role/privilege matrix (`GET /api/v1/roles`). */
export type PermissionInfo = components['schemas']['PermissionInfo'];

/** One role in the matrix: metadata plus the permission keys it grants. `builtin` roles are
 *  fixed today (custom roles are not yet configurable). */
export type RoleInfo = components['schemas']['RoleInfo'];

/** The role-vs-privilege matrix (`GET /api/v1/roles`): the permission catalogue + per-role grants. */
export type RoleMatrix = components['schemas']['RolesMatrix'];

/** A user account row (`GET /api/v1/users`). Never includes the password hash. `created_at` is
 *  RFC 3339 text; `last_login_at` is RFC 3339 text or null (the account has never logged in). */
export type UserSummary = components['schemas']['UserSummary'];

/** How an account authenticates. `service` is a machine account: no password, no IdP subject, and
 *  therefore no way to sign in — it exists to own API tokens so an unattended integration outlives
 *  the person who set it up. `oidc` and `ldap` accounts are provisioned by signing in, never created
 *  by hand: the row has to carry the directory's own identifier, which only a login reveals. */
export const USER_KINDS = ['local', 'oidc', 'ldap', 'service'] as const;

/** How an account authenticates (snake_case). Pinned to `schemas.UserKind`. */
export type UserKind = (typeof USER_KINDS)[number];

/** What an account may see: `"All"`, or the node groups it is limited to (`{ Groups: [id, …] }`).
 *  Entries are `node_groups.id` UUIDs, never group names — a name is editable and not unique, so
 *  one would silently widen or void the scope the day somebody renames a folder. */
export type Scope = components['schemas']['Scope'];

/** A configured OIDC provider (`GET /api/v1/settings/oidc`). The client_secret is write-only and
 *  never returned; `has_secret` signals one is stored. */
export type OidcProviderSummary = components['schemas']['OidcProviderSummary'];

/** Create/update payload for an OIDC provider. `client_secret` is write-only: omit (or empty) on
 *  update to keep the stored secret. */
export type OidcProviderInput = components['schemas']['OidcProviderInput'];

/** Which IdP product a provider was configured for. `generic` is last on purpose: it is the
 *  escape hatch for anything not in the list, and the order here is the order of the picker. */
export const OIDC_PROVIDER_KINDS = ['entra', 'okta', 'google', 'generic'] as const;

/** An IdP product (snake_case). Pinned to `schemas.OidcProviderKind`. */
export type OidcProviderKind = (typeof OIDC_PROVIDER_KINDS)[number];

/** The configured LDAP/AD directory (`GET /api/v1/settings/ldap`). The bind password is write-only
 *  and never returned; `has_bind_password` signals one is stored. `ca_cert` is a certificate, not a
 *  secret, so it does round-trip. */
export type LdapConfigView = components['schemas']['LdapConfigView'];

/** Save payload for the directory. `bind_password` is write-only: **omit** it to keep the stored
 *  one. Unlike the LLM credential there is no "clear" value — a blank bind password is an anonymous
 *  bind, so the server rejects it. */
export type LdapConfigInput = components['schemas']['LdapConfigInput'];

/** How the directory connection is protected. Both values are TLS; there is deliberately no
 *  plaintext option. */
export type LdapSecurity = components['schemas']['LdapSecurity'];

/** The staged result of `POST /api/v1/settings/ldap/test`. `role: null` means the user tested would
 *  be **denied** — the commonest misconfiguration, and one the login form cannot distinguish from a
 *  wrong password. */
export type LdapTestResult = components['schemas']['LdapTestResult'];

/** An API token (`GET /api/v1/api-tokens`) — a long-lived credential for unattended clients
 *  (ADR-028). The raw token is write-only: shown once on create, never returned again; only
 *  metadata is listed. */
export type ApiTokenSummary = components['schemas']['ApiTokenInfo'];

/** The surfaces a token can be presented at. A token names its own, and one issued before this
 *  existed carries `mcp` alone — which is why upgrading cannot turn an assistant's credential into
 *  one that reconfigures monitoring. */
export const TOKEN_SURFACES = ['mcp', 'rest'] as const;

/** An auth surface (snake_case). Pinned to `schemas.TokenSurface`. */
export type TokenSurface = (typeof TOKEN_SURFACES)[number];

/** Create payload for an API token. `scope` is omitted for global (`"All"`) visibility — the only
 *  scope either surface accepts today. */
export type ApiTokenInput = components['schemas']['CreateApiTokenBody'];

/** Create response: the raw `token` is shown once and never returned again. */
export type CreatedApiToken = components['schemas']['CreatedApiToken'];

// ── Forwarding ("tee", ADR-034) — relay received syslog/traps to external collectors ────────────

/** Every received stream a destination can tee. */
export const FORWARD_SOURCE_KINDS = ['syslog', 'trap', 'flow'] as const;

/** Which received stream a destination tees. Pinned to `schemas.SourceKind`. */
export type ForwardSourceKind = (typeof FORWARD_SOURCE_KINDS)[number];

/** Every way a destination can be spoken to. */
export const FORWARD_DEST_KINDS = [
  'syslog_udp',
  'syslog_tcp',
  'syslog_tls',
  'snmp_trap_udp',
  'flow_udp',
  'bigquery',
] as const;

/** How a destination is spoken to. Pinned to `schemas.DestKind`. */
export type ForwardDestKind = (typeof FORWARD_DEST_KINDS)[number];

/** A filter condition's subject. For flow, `source_ip` is the exporting device and `kind` is the
 *  wire format (`netflow`/`sflow`); the record's own endpoints are `src_addr`/`dst_addr`. */
export type ForwardFilterField = components['schemas']['FilterField'];

/** A filter condition's comparison. */
export type ForwardFilterOp = components['schemas']['FilterOp'];

/** How a filter's conditions combine. Runtime array so `i18nEnumKeys.test.ts` can demand the
 *  `filter.mode.*` strings the destinations table interpolates. */
export const FORWARD_FILTER_MODES = ['all', 'any'] as const;

/** How a filter's conditions combine. Pinned to `schemas.FilterMode`. */
export type ForwardFilterMode = (typeof FORWARD_FILTER_MODES)[number];

/** One `field op value` test. `value` is always a string; core parses it per field type. */
export type ForwardCondition = components['schemas']['Condition'];

/** A forwarding destination (`GET /api/v1/forwarding/destinations`). `has_secret` reports whether a
 *  sealed secret (the SNMP community, or the BigQuery service-account key) is stored — the value
 *  itself is never returned. On a BigQuery destination, `false` means Workload Identity. */
export type ForwardDestination = components['schemas']['ForwardDestination'];

/** Create/update payload. Omitting `community` / `service_account_json` on update keeps the stored
 *  value; `ca_cert` is not a secret and is always sent, so clearing it removes the pinned CA. */
export type ForwardDestinationInput = components['schemas']['ForwardDestinationBody'];

/** One destination's live counters. `rendered` counts messages that were sent re-rendered despite
 *  `verbatim`, because no original payload was available. */
export type ForwardDestStatus = components['schemas']['DestStatus'];

/** `GET /api/v1/forwarding/status`. `sending` is false on an HA standby, whose dispatcher is idle. */
export type ForwardStatus = components['schemas']['ForwardingStatus'];

/** `POST /api/v1/forwarding/destinations/:id/test` — the transport result, not an HTTP error. */
export type ForwardTestResult = components['schemas']['TestResult'];

// ── Profiles, thresholds, scopes ────────────────────────────────────────────────────────────────

/** A device-class profile (`GET /api/v1/profiles`). Split by functional `category` (role) ×
 *  `vendor`-NOS family; `category` is the kebab-case `ProfileCategory` token. */
export type ProfileSummary = components['schemas']['ProfileSummary'];

/** Create/update-profile request body (`POST`/`PUT /api/v1/profiles`). */
export type ProfileInput = components['schemas']['ProfileBody'];

/** Every threshold scope level, broadest → most specific. */
export const SCOPE_LEVELS = ['profile', 'group', 'node'] as const;

/** Threshold scope level (snake_case). Most-specific wins. Pinned to `schemas.ScopeLevel`. */
export type ScopeLevel = (typeof SCOPE_LEVELS)[number];

/** Maintenance-window scope. The threshold scopes plus `group_id` — a hierarchical folder group
 *  (the All Nodes tree), resolved recursively incl. subgroups (ADR-022). Distinct from the legacy
 *  tag-based `group` scope (`scope_id` is a group UUID, not a tag value). */
export type MaintenanceScopeLevel = components['schemas']['WindowScope'];

/** Every breach direction. */
export const DIRECTIONS = ['above', 'below'] as const;

/** Breach direction (snake_case). Pinned to `schemas.Direction`. */
export type Direction = (typeof DIRECTIONS)[number];

/** A stored threshold rule (`GET /api/v1/thresholds`). The rule fields are flattened onto the row.
 *  The GET shape uses `scope_level`, matching the POST body. */
export type StoredThreshold = components['schemas']['StoredThreshold'];

/** A capped page of threshold rules (`GET /api/v1/thresholds`).
 *
 *  Thresholds are the one configuration table that grows with the fleet — a node-level override is
 *  per (node × metric) — so the list is capped server-side. `total` is the whole ruleset's size,
 *  and `truncated` says outright that `items` is a prefix, rather than leaving the UI to infer it
 *  from `items.length` (which is wrong when the ruleset is exactly the cap). */
export type ThresholdPage = components['schemas']['ThresholdPage'];

/** The query `GET /api/v1/thresholds` accepts, derived from the operation rather than hand-listed
 *  — the client passes it through whole, so a filter cannot be dropped on the way (the bug
 *  `listAlertHistory` shipped with). */
export type ThresholdQuery = NonNullable<
  paths['/api/v1/thresholds']['get']['parameters']['query']
>;

/** One alert-history row (`GET /api/v1/alerts/history`) — the stored row plus inbound ack state
 *  (the Rust type is `AlertHistoryView`). `metric` is `null` for rows recorded before it was
 *  captured ⇒ render "—"; `observed_value` / `threshold_value` / `direction` are threshold checks
 *  only. The keyset cursor is the **pair** `(recorded_at, id)` — see `pages/historyCursor.ts`. */
export type AlertHistoryRow = components['schemas']['AlertHistoryView'];

/** Query params for alert history — the cursor pair plus the History toolbar's filters. Derived
 *  from the operation rather than hand-listed: a hand-written parameter list here silently dropped
 *  every filter a caller passed, because excess-property checking does not reach a value that came
 *  out of a function. */
export type AlertHistoryQuery = NonNullable<
  paths['/api/v1/alerts/history']['get']['parameters']['query']
>;

/** A chronic-offender row (`GET /api/v1/alerts/top-nodes`). */
export type AlertNodeCount = components['schemas']['AlertNodeCount'];

/** One weekday×hour heatmap cell (`GET /api/v1/alerts/calendar`); `dow` 0=Sun…6=Sat, hour 0–23 (UTC). */
export type CalendarBucket = components['schemas']['CalendarBucket'];

/** A recent up/down transition (`GET /api/v1/alerts/transitions`). `resolved` = recovery to ok. */
export type AlertTransition = components['schemas']['AlertTransition'];

// ── Notifications ───────────────────────────────────────────────────────────────────────────────

/** Every notification channel kind, in the order the UI offers them. An `as const` array rather
 *  than a bare union because a union the compiler knows and nothing can enumerate at runtime is
 *  exactly what `extensibility.md` §4 is about.
 *
 *  ⚠️ This comment used to say "the Routing screen's kind filter iterates it", and **nothing did** —
 *  the array was referenced by no other file while `RoutingPage.tsx` spelled the four kinds out as
 *  `<option>` literals. `lib/channelKinds.ts` is what consumes it now, and it holds the labels. */
export const CHANNEL_KINDS = ['webhook', 'email', 'pagerduty', 'jsm'] as const;

/** A notification channel kind. Pinned to `schemas.ChannelKind`. */
export type ChannelKind = (typeof CHANNEL_KINDS)[number];

/** Notification channel metadata (`GET /api/v1/notification-channels`) — the secret
 *  connection config is sealed server-side and never returned. */
export type NotificationChannel = components['schemas']['ChannelSummary'];

/** Channel connection config supplied on create (sealed server-side; tagged by `kind`).
 *  PagerDuty is the Events API v2 (`routing_key` is the integration key; `api_url` overrides the
 *  default US endpoint — EU is https://events.eu.pagerduty.com/v2/enqueue). JSM is Alerts
 *  (Opsgenie-compatible): `api_url` is the integration base, `api_key` the GenieKey. */
export type ChannelConfigInput = components['schemas']['ChannelConfig'];

/** A routing rule (`GET /api/v1/routing-rules`): alerts of `severity` (null = any) fan out
 *  to `channel_ids`. */
export type RoutingRule = components['schemas']['RoutingRule'];

/** One name a notification template may reference (ADR-039). The catalogue is served rather than
 *  transcribed, so the editor's palette and the renderer's context cannot disagree. */
export type TemplateVariable = components['schemas']['TemplateVariable'];

/** Which point in an alert's life a template renders for. */
export type NotifyEvent = components['schemas']['NotifyEvent'];

/** What a template would send (`POST /api/v1/notification-channels/preview`). A field that could
 *  not be rendered comes back as the built-in text plus an entry in `problems` — the same
 *  fallback delivery applies, so the preview shows what an alert would actually produce. */
export type TemplatePreview = components['schemas']['PreviewResult'];

// ── Collection (metrics to gather) ──────────────────────────────────────────────────────────────

/** How a collection item is gathered. */
export type CollectionKind = components['schemas']['CollectionKind'];

/** Whether a metric is a gauge or a raw counter. */
export type MetricKind = components['schemas']['MetricKind'];

/** The metric kinds, as an array, so a rule that must hold for every kind can be asserted over
 *  them at runtime rather than re-spelled at each call site (`extensibility.md` §4). */
export const METRIC_KINDS = ['gauge', 'counter'] as const;

/** One thing to collect: a stable metric name, the OID, how to collect it, and its kind. */
export type CollectionItem = components['schemas']['CollectionItem'];

/** A reusable collection template (Rust `TemplateSummary`): a named metric bundle that device
 *  profiles attach. `item_count` is how many metrics it carries. */
export type CollectionTemplate = components['schemas']['TemplateSummary'];

// ── Discovery & classification ──────────────────────────────────────────────────────────────────

/** One device a discovery scan found. `suggested_profile_id` is resolved server-side via the
 *  classification rules (by sysObjectID prefix, else sysDescr regex, else Generic SNMP) — an id,
 *  not a name, so the row pre-selects robustly even if the profile was renamed. */
export type DiscoveryCandidate = components['schemas']['Candidate'];

/** A device-classification rule (`GET /api/v1/classification-rules`). Maps a discovered device's
 *  SNMP signature to a profile; evaluated by ascending priority. */
export type ClassificationRule = components['schemas']['ClassificationRule'];

/** Create/update body for a classification rule. */
export type ClassificationRuleInput = components['schemas']['ClassificationRuleBody'];

/** A discovery scan's status (`GET /api/v1/discovery/scan/:id`). */
export type DiscoveryScan = components['schemas']['ScanStatus'];

/** One curated OID-catalog entry (`GET /api/v1/mib-catalog`). A reference metric_name → (oid, kind)
 *  so the collection editor can pick by name. */
export type MibCatalogEntry = components['schemas']['MibEntry'];

// ── Maintenance & mutes ─────────────────────────────────────────────────────────────────────────

/** A maintenance window (`GET /api/v1/maintenance-windows`, Rust `StoredWindow`). Nodes covered by
 *  an active window observe `maintenance` — no alerts fire, existing ones resolve. Scoped like
 *  thresholds; times are RFC 3339. `active` = covers "now". */
export type MaintenanceWindow = components['schemas']['StoredWindow'];

/** A mute (`GET /api/v1/mutes`, Rust `StoredMute`): notifications are silenced until `until_at`
 *  — the alert still shows in the UI/history. A `node` mute targets one node (optionally one
 *  `metric_name`); a `group` mute targets every node under a folder group (recursive). Exactly
 *  one of `node_id` / `group_id` is set, per `scope_kind`. */
export type Mute = components['schemas']['StoredMute'];

/** A node released from a suppression it only *inherited* (`GET /api/v1/suppression-exemptions`,
 *  Rust `StoredExemption`). Its group's window or mute still stands for everyone else; this node
 *  alerts normally until `until_at`, which the server sizes to the coverage it was carved out of.
 *
 *  ⚠️ Not `Node.suppression_opt_out`, which is about derived *dependency* suppression (ADR-043). */
export type SuppressionExemption = components['schemas']['StoredExemption'];

// ── URL & DNS monitors ──────────────────────────────────────────────────────────────────────────

/** Which HTTP status codes count as "up" for a URL monitor (a tagged object). */
export type ExpectedStatus = components['schemas']['ExpectedStatus'];

/** A node's URL-monitor configuration (1:1 with the node). `url` is the only required field on
 *  create; the rest default server-side. */
export type UrlCheckConfig = components['schemas']['UrlCheckConfig'];

/** A URL monitor's response-body keyword rule. Absent ⇒ the probe never reads the body at all. */
export type BodyMatch = components['schemas']['BodyMatch'];

/** One number a URL monitor lifts out of a JSON response body and records under its own name. */
export type JsonExtract = components['schemas']['JsonExtract'];

/** Which record type a DNS monitor resolves to. */
export type DnsRecordType = components['schemas']['DnsRecordType'];

/** One record in a resolution answer set (a tagged object). */
export type DnsRecord = components['schemas']['DnsRecord'];

/** One answer plus its TTL. The TTL is display-only — it is not part of the change key. */
export type DnsAnswer = components['schemas']['DnsAnswer'];

/** One hop of a resolution chain: the name queried and the answers for it (canonical order). */
export type DnsHop = components['schemas']['DnsHop'];

/** Why a resolution did not reach a terminal record set. */
export type DnsFailure = components['schemas']['DnsFailure'];

/** One observed resolution chain — the artifact of a DNS check. `resolver` is where we asked
 *  (`10.0.0.53:53`, or `system`): provenance, not content. Hop order is significant, and a `null`
 *  `failure` means the chain resolved. */
export type DnsChain = components['schemas']['DnsChain'];

/** A node's DNS-monitor configuration (1:1 with the node). `name` is the only required field on
 *  create; the rest default server-side, and `resolver: null` ⇒ the poller's system resolver. */
export type DnsCheckConfig = components['schemas']['DnsCheckConfig'];

/** The current chain plus how long it has held (`GET /api/v1/nodes/:id/dns-chain`). */
export type DnsChainCurrent = components['schemas']['DnsChainCurrent'];

/** One append-on-change history row (`GET /api/v1/nodes/:id/dns-chain/history`).
 *  `prev_chain_key` is `null` on the first observation for this node. */
export type DnsChainChange = components['schemas']['DnsChainChange'];

/** A page of DNS chain history with its keyset cursor (`null` ⇒ no more rows). */
export type DnsChainHistoryPage = components['schemas']['DnsChainHistory'];

// ── CDP/LLDP adjacency (ADR-038) ────────────────────────────────────────────────────────────────

/** One observed adjacency: which local port faces which remote chassis/port, plus payload. */
export type Neighbor = components['schemas']['Neighbor'];

/** Every adjacency a node reported on one observation. `truncated` means the per-node cap was hit
 *  and rows were dropped — surfaced rather than swallowed, because a partial view that looks
 *  complete is worse than none. */
export type NeighborSet = components['schemas']['NeighborSet'];

/** The current adjacency plus how long it has held (`GET /api/v1/nodes/:id/neighbors`). */
export type CurrentNeighbors = components['schemas']['CurrentNeighbors'];

/** One append-on-change adjacency history row. `prev_neighbor_key` is `null` on the first
 *  observation ever recorded for this node. */
export type NeighborChange = components['schemas']['NeighborChange'];

/** A page of adjacency history with its keyset cursor (`null` ⇒ no more rows). */
export type NeighborHistoryPage = components['schemas']['NeighborHistory'];

/** Whether this deployment runs each discovery walk and how often
 *  (`GET /api/v1/settings/neighbors`). Named for adjacency because that walk shipped first; it now
 *  carries the interface-address and ARP pairs too. */
export type NeighborConfig = components['schemas']['NeighborConfig'];

/** One address a monitored router resolved on the wire that Yagra does not monitor (ADR-043 I3). */
export type DiscoveredEndpoint = components['schemas']['DiscoveredEndpointRow'];

/** A page of discovered endpoints with its keyset cursor and the fleet coverage line. */
export type DiscoveredEndpointPage = components['schemas']['DiscoveredEndpointPage'];

/** How many nodes an import created — shared by the scan import and the endpoint promotion. */
export type ImportResult = components['schemas']['ImportResult'];

/** Which protocol reported an adjacency. Hand-written as an `as const` because the UI iterates it
 *  at runtime (a generated `type` union does not exist at runtime — extensibility §4). */
export const NEIGHBOR_PROTOS = ['lldp', 'cdp'] as const;
export type NeighborProto = (typeof NEIGHBOR_PROTOS)[number];

/** What a peer says it is, normalized across LLDP's `BITS` and CDP's bitmask. Same reason as
 *  above: the legend renders by iterating this. */
export const NEIGHBOR_CAPABILITIES = [
  'router',
  'bridge',
  'switch',
  'wlan_ap',
  'phone',
  'host',
  'repeater',
  'cable_device',
  'igmp',
  'other',
] as const;
export type NeighborCapability = (typeof NEIGHBOR_CAPABILITIES)[number];

// ── Node detail ─────────────────────────────────────────────────────────────────────────────────

/** One node's configuration detail incl. bindings (`GET /api/v1/nodes/:id`).
 *
 *  `pool` is deliberately the raw stored value so the edit form can tell explicit from inherited —
 *  for the *effective* pool and the current poller use `getNodeAssignment`. Branch on `kind`, not
 *  on which of the three config rows below is non-null: a node can carry more than one row (the
 *  API refuses new ones, but older rows exist) and only one of them wins. */
export type NodeDetail = components['schemas']['NodeDetail'];

/** A node's monitoring kind, in the backend's precedence order. `device` is the fallthrough:
 *  ICMP liveness plus SNMP when configured. */
export const NODE_KINDS = ['meraki', 'url', 'dns', 'device'] as const;

/** Pinned to `schemas.NodeKind`. */
export type NodeKind = (typeof NODE_KINDS)[number];

/** A node's Cisco Meraki binding (1:1; part of `NodeDetail`). */
export type MerakiDeviceConfig = components['schemas']['MerakiDeviceConfig'];

/** A configured Meraki organization (`GET /api/v1/meraki/orgs`). */
export type MerakiOrg = components['schemas']['MerakiOrgView'];

/** An organization the API key can access (from `POST /api/v1/meraki/orgs/discover`). */
export type MerakiOrgOption = components['schemas']['MerakiOrgOption'];

/** A network within an org, with its monitored (watch/skip) flag. */
export type MerakiNetwork = components['schemas']['MerakiNetworkView'];

/** An import candidate device (from `POST /api/v1/meraki/orgs/:id/enumerate`). */
export type MerakiCandidate = components['schemas']['MerakiCandidate'];

/** The enumerate response: the org's networks + import candidates. */
export type MerakiEnumeration = components['schemas']['MerakiEnumeration'];

/** One interface row for the node-detail Interfaces tab (`GET /api/v1/nodes/:id/interfaces`).
 *  Rates/utilization are derived at query time; `null` when there's no data or no known speed. */
export type InterfaceRow = components['schemas']['InterfaceRow'];

/** Per-interface time-series for the detail pane (`GET /nodes/:id/interfaces/:ifindex/series`).
 *  All arrays share the `timestamps` x-axis; `null` is a gap. `*_bps` are bits/sec (rate of
 *  the octet counters × 8); `*_errors` are errors/sec. */
export type InterfaceSeries = components['schemas']['InterfaceSeries'];

// ── Fleet, audit, self-observability ────────────────────────────────────────────────────────────

/** One audit-log row (`GET /api/v1/audit`). Admin-only; `at` is RFC 3339. `action` is one of
 *  `METHOD /api/v1/...` (a mutating request), `auth.login[.ldap…]` (a sign-in, by any method) or
 *  `mcp.<tool> key=value` (an action taken through the MCP surface, which does not pass through the
 *  REST audit middleware). */
export type AuditRow = components['schemas']['AuditRow'];

/** The audit log's filterable action kinds. Iterated by the toolbar and by `i18nEnumKeys.test.ts`,
 *  so it must exist at runtime; pinned to the generated union by `schemaEnumPins`.
 *
 *  ⚠️ These are **prefix** classes, not exact action strings — `login` covers `auth.login.ldap` and
 *  its siblings, `mcp` covers every tool. The backend owns the prefixes
 *  (`yagra_core::audit::AuditAction::sql_prefix`); nothing here re-derives them from `action`. */
export const AUDIT_ACTIONS = ['post', 'put', 'patch', 'delete', 'login', 'mcp'] as const;
export type AuditAction = (typeof AUDIT_ACTIONS)[number];

/** How an audit entry's response turned out. Boundaries are decided server-side
 *  (`AuditStatusClass::bounds`); `lib/format.ts::httpStatusTone` only picks the badge colour. */
export const AUDIT_STATUS_CLASSES = ['ok', 'client', 'server'] as const;
export type AuditStatusClass = (typeof AUDIT_STATUS_CLASSES)[number];

/** Query params for the audit log — keyset-paged on `before`, filtered by the rest. */
export type AuditQuery = NonNullable<paths['/api/v1/audit']['get']['parameters']['query']>;

/** Fleet state-count timeline (`GET /api/v1/fleet/state-history`): per-state series aligned to a
 *  shared `timestamps` axis (Unix seconds). `series` is keyed by node state. */
export type StateHistory = components['schemas']['FleetStateHistory'];

/** Fleet aggregate throughput over time (`GET /api/v1/metrics/throughput-range`); bits/sec, `null` = gap. */
export type ThroughputRange = components['schemas']['ThroughputRange'];

/** Busiest-links × time throughput heatmap (`GET /api/v1/metrics/interface-heatmap`). `values`
 *  is rows (`links`) × cols (`timestamps`), each cell bits/sec. */
export type InterfaceHeatmap = components['schemas']['InterfaceHeatmap'];

/** Poll-loop self-monitoring (`GET /api/v1/poller-health`, Rust `SchedulerStatsSnapshot`). */
export type PollerHealth = components['schemas']['SchedulerStatsSnapshot'];

/** One registered poller in the distributed pool (`GET /api/v1/pollers`, ADR-009/020). A merge of
 *  the live registry (current status/telemetry) and the durable inventory, so an offline poller
 *  still lists. `last_seen`/`first_seen`/`version` are null for a live poller not yet persisted,
 *  and the host gauges are null when offline / on an N-1 build. */
export type PollerInfo = components['schemas']['PollerInfo'];

/** Per-pool summary in the `GET /api/v1/pollers` response: node count vs. live pollers, dispatch
 *  mode (`working_set` when a live poller serves it, else the `legacy` per-job fallback), and
 *  `warning: "nodes_without_live_poller"` when the pool has nodes but no live poller — they would
 *  go unmonitored. */
export type PoolSummary = components['schemas']['PoolSummary'];

/** The `GET /api/v1/pollers` body: the registered poller fleet + the per-pool summary. */
export type PollersResponse = components['schemas']['PollersResponse'];

/** Where a node's effective pool came from. */
export type PoolSource = components['schemas']['PoolSource'];

/** One pool offered by the assignment picker (`GET /api/v1/pools`). `live` is whether a live poller
 *  currently serves it — a pool with none takes the legacy per-job path onto a subject nothing
 *  subscribes to, so assigning to it leaves the node unmonitored, and the picker warns rather than
 *  presenting it as an equivalent choice. */
export type PoolOption = components['schemas']['PoolOption'];

/** The `GET /api/v1/pools` body. */
export type PoolsResponse = components['schemas']['PoolOptions'];

/** Which poller currently polls a node.
 *  - `assigned` — it is in that poller's published working set (`poller_id` set).
 *  - `pending` — the pool has a live poller but this node isn't in anyone's set yet (added since
 *    the last sweep, or it builds no checks at all).
 *  - `legacy_fanout` — the pool has NO live poller, so jobs go to `yagra.jobs.{pool}` with nothing
 *    subscribed: no owner, and probably unmonitored.
 *  - `meraki` — core's org collector polls it, not a pool poller.
 *  - `unknown` — this core is an HA standby and runs no coordinator. */
export type PolledBy = components['schemas']['PolledBy'];

/** `GET /api/v1/nodes/:id/assignment` — effective pool (the node's own, else the nearest ancestor
 *  folder's, else `default`), its provenance, and the current poller. */
export type NodeAssignment = components['schemas']['NodeAssignment'];

/** One node in the poller drill-down. */
export type PollerNodeRef = components['schemas']['PollerNodeRef'];

/** `GET /api/v1/pollers/:id/nodes` — the nodes a poller currently holds. Read from the coordinator's
 *  published working set, so it is the same data the node detail's "Polled by" reports. `state` is
 *  `assigned` (live), `offline` (unknown / not beating) or `unknown` (this core is a standby). */
export type PollerNodesResponse = components['schemas']['PollerNodesResponse'];

/** One core↔poller visibility outage (`GET /api/v1/monitoring-gaps`, Phase 3 store-and-forward): a
 *  span during which core could not hear from the poller. If the poller stayed alive but partitioned,
 *  its local buffer backfills the metrics for the window on reconnect; alerts are not backfilled. */
export type MonitoringGap = components['schemas']['MonitoringGapRow'];

/** Reachability of one Yagra backing dependency (no secrets — just a flag + label). */
export type DependencyHealth = components['schemas']['DependencyHealth'];

/** Yagra self-health: backing-service reachability (`GET /api/v1/system-health`). `bus` is an
 *  indirect signal inferred from poll-loop activity, not a direct NATS ping. `logs` (VictoriaLogs,
 *  ADR-024) and `flow` (ClickHouse, ADR-031) report reachable when not configured — events then
 *  live in PostgreSQL and flow is simply off — so an unconfigured store never degrades `overall`. */
export type SystemHealth = components['schemas']['SystemHealth'];

/** Running product version (`GET /api/v1/version`). `core` is the live `yagra-core` crate version
 *  (= the workspace version). Public — no secrets. The WebUI pairs this with its own build version. */
export type VersionInfo = components['schemas']['VersionInfo'];

/** What this deployment is running and how far back it can be taken (`GET /api/v1/system/upgrade`,
 *  ADR-050). `ManageConfig` — it answers with build provenance and, once the updater sidecar lands,
 *  the registry and resolved digests, which is deployment configuration rather than a health
 *  counter. */
export type UpgradeStatus = components['schemas']['UpgradeStatusResponse'];

/** The oldest core version that can still run this database, and which migration decided it.
 *  `null` on `SchemaState.compat` means every applied migration is reversible. */
export type CompatFloor = components['schemas']['CompatFloor'];

/** The accepted upgrade run (`POST /api/v1/system/upgrade`). */
export type UpgradeRunAccepted = components['schemas']['RunAccepted'];

// ── Host self-observability (Yagra monitoring its own core + pollers) ──────────

/** Usage of one watched filesystem (or a store-size proxy). `size_bytes === 0` ⇒ capacity unknown
 *  (a bare growing size, e.g. the PostgreSQL `database` proxy) — show used bytes, not a %. */
export type DiskUsage = components['schemas']['DiskUsage'];

/** Current host resources for one self-monitored instance (`GET /api/v1/system/hosts`): core
 *  (`role: 'core'`, `instance: 'core'`) or a poller (`role: 'poller'`, `instance` = poller id;
 *  `pool` is null for core). Metrics are null until the first sample. */
export type HostInfo = components['schemas']['HostInfo'];

/** The `GET /api/v1/system/hosts` body: core plus every poller reporting host telemetry. */
export type SystemHostsResponse = components['schemas']['SystemHostsResponse'];

/** A per-mount filesystem trend within a `HostMetricRange`. Derive % from `used_bytes`/`size_bytes`
 *  aligned on timestamps, or show a bare-bytes trend when `size_bytes` is empty/zero. */
export type HostDiskRange = components['schemas']['HostDiskRange'];

/** Host CPU/load/mem/disk trends for one instance over a window
 *  (`GET /api/v1/system/hosts/:instance/metrics/range`). One round trip per instance/range. */
export type HostMetricRange = components['schemas']['HostMetricRange'];

/** Fleet data-coverage summary (`GET /api/v1/fleet/coverage`). */
export type FleetCoverage = components['schemas']['FleetCoverage'];

/** Fleet-wide status summary (`GET /api/v1/fleet/summary`): total node count + a per-state tally,
 *  computed server-side so the dashboard's status/health/down widgets are correct over the whole
 *  fleet (not a paged slice). Every state key is present and the counts sum to `total`. */
export type FleetSummary = components['schemas']['FleetSummary'];

/** Per-group health rollup (`GET /api/v1/fleet/group-summary`): for each group id, the state tally
 *  of its **direct** member nodes, computed server-side over the whole fleet (not a paged slice) so
 *  the site-matrix / region-rollup / geo-map widgets stay correct past the first 100 nodes (A-1).
 *  Every state key is present; ungrouped nodes are omitted (no widget rolls them up), and the client
 *  sums descendants for the region rollup. */
export type FleetGroupSummary = components['schemas']['FleetGroupSummary'];

/** One node in the dependency/topology graph (`GET /api/v1/topology`). */
export type TopologyNode = components['schemas']['TopologyNode'];

/** One undirected link in the derived connectivity graph (`GET /api/v1/topology/links`). */
export type TopologyLink = components['schemas']['TopologyLink'];

/** What the last derivation run observed but did not turn into a link. */
export type TopologyLinkSummary = components['schemas']['TopologyLinkSummary'];

/** What kind of evidence produced a link.
 *
 *  An `as const` array, not just the generated union: ~150 call sites build an i18n key at runtime
 *  (`` t(`map.source.${l.source}`) ``), and EN/JA parity cannot prove either locale is complete —
 *  a new backend variant is missing from *both*, so parity passes while the map renders a raw key.
 *  `i18nEnumKeys.test.ts` iterates this, which only works if the union exists at runtime
 *  (extensibility.md §4). Ordered strongest-first, matching the server's rank. */
export const LINK_SOURCES = [
  'manual',
  'lldp',
  'cdp',
  'ospf',
  'route',
  'bgp',
  'l3_subnet',
] as const;
export type LinkSource = (typeof LINK_SOURCES)[number];

/** An operator decision recorded against a link (`GET /api/v1/topology/link-overrides`). */
export type LinkOverrideRow = components['schemas']['LinkOverrideRow'];

/** What an operator decided about a link. `as const` for the same reason as `LINK_SOURCES`. */
export const LINK_OVERRIDE_ACTIONS = ['pin', 'hide', 'direction'] as const;
export type LinkOverrideAction = (typeof LINK_OVERRIDE_ACTIONS)[number];

/** Which end of a link is upstream. Endpoints are stored in a canonical order, so direction cannot
 *  ride on which column an endpoint landed in — hence an explicit value. */
export const LINK_DIRECTIONS = ['a_parent', 'b_parent'] as const;
export type LinkDirection = (typeof LINK_DIRECTIONS)[number];

/** Which dependency graph drives alert suppression (`PUT /api/v1/settings/topology`).
 *
 *  `manual` uses each node's hand-authored parent; `shadow` changes nothing about alerting and only
 *  makes the comparison meaningful; `derived` hands suppression to the derived graph. */
export const TOPOLOGY_MODES = ['manual', 'shadow', 'derived'] as const;
export type TopologyMode = (typeof TOPOLOGY_MODES)[number];

/** The manual-vs-derived comparison (`GET /api/v1/topology/shadow`) — the review surface for
 *  enabling derived suppression. */
export type TopologyShadow = components['schemas']['TopologyShadow'];

/** The fixed error envelope (ADR-019). */
export type ApiErrorBody = components['schemas']['ApiErrorBody'];

// ── Troubleshoot analysis jobs (ADR-022) ─────────────────────────────────────

/** An analysis job row / SSE event (`/api/v1/analysis/jobs`). Timestamps are epoch-millis.
 *  `params` is a `serde_json::Value` server-side — see the UI-only section for why it is narrowed. */
export type AnalysisJob = components['schemas']['AnalysisJob'];

/** One finding from an analysis (`GET /api/v1/analysis/jobs/:id/findings`). `detail` is a
 *  tool-specific payload the backend serializes opaquely — cast it per tool (`AnomalyDetail`, …). */
export type AnalysisFinding = components['schemas']['AnalysisFinding'];

/** Request body to launch an analysis (`POST /api/v1/analysis/jobs`). */
export type AnalysisJobInput = components['schemas']['CreateAnalysisJob'];

/** One row of the cross-run findings search (`GET /api/v1/analysis/findings`). Carries the run it
 *  came from, so the row can link to that run's report; no `detail` — open the report for that. */
export type SavedFinding = components['schemas']['SavedFinding'];

/** Query params for the cross-run findings search — keyset-paged on `(before, before_id)`. */
export type SavedFindingsQuery = NonNullable<
  paths['/api/v1/analysis/findings']['get']['parameters']['query']
>;

/** A recurring analysis (`GET /api/v1/analysis/schedules`). Timestamps are epoch-millis. */
export type AnalysisSchedule = components['schemas']['AnalysisSchedule'];

/** Create/update body for a recurring analysis — the launch spec plus the cadence. */
export type AnalysisScheduleInput = components['schemas']['AnalysisScheduleBody'];

/** Outcome of a schedule's last firing attempt. `busy` is the one with no report-schedule
 *  equivalent: the analysis runner has admission control, so a fire can be refused, and a refused
 *  schedule stays due rather than skipping its period. */
export const ANALYSIS_SCHEDULE_STATUSES = ['queued', 'busy', 'error', 'unknown'] as const;
export type AnalysisScheduleStatus = (typeof ANALYSIS_SCHEDULE_STATUSES)[number];

// ── Reports (Dashboard → Reports) ──
// Definitions are reusable templates (opaque `spec` the frontend owns); schedules fire them on a
// preset cadence; runs are saved generated reports. Shared resource: everyone reads, admins write.

/** A report-section type from the catalog (`GET /api/v1/reports/sections`). */
export type ReportSectionDef = components['schemas']['SectionDef'];

/** A saved report definition (template). `spec` is opaque to the backend — see the UI-only
 *  section for `ReportSpec`. */
export type ReportDefinition = components['schemas']['ReportDefinition'];

/** Create/update body for a report definition. */
export type ReportDefinitionInput = components['schemas']['ReportDefinitionBody'];

/** A report schedule (joined with its definition's name). */
export type ReportSchedule = components['schemas']['ReportSchedule'];

/** Create/update body for a report schedule. */
export type ReportScheduleInput = components['schemas']['ReportScheduleBody'];

/** A saved report run (the saved-reports list; no heavy payloads). */
export type ReportRun = components['schemas']['ReportRun'];

/** A run plus its rendered result (the viewer). */
export type ReportRunDetail = components['schemas']['ReportRunDetail'];

// ── Passive events (syslog / SNMP traps / webhooks) ──

/** A webhook ingest source (`GET /api/v1/event-sources`) — the bearer token is shown
 *  once on create/rotate and stored only as a hash. */
export type EventSource = components['schemas']['EventSourceView'];

/** An event match rule (`GET /api/v1/event-rules`). severity 'info' records the match but
 *  raises no alert. `ttl_secs` auto-closes; `clear_pattern` resolves early; `min_count`
 *  matches inside `window_secs` gate the fire. */
export type EventRule = components['schemas']['StoredEventRule'];

/** Create/update body for an event rule (`POST/PUT /api/v1/event-rules`). */
export type EventRuleInput = components['schemas']['EventRuleBody'];

/** Result of the interactive rule tester (`POST /api/v1/event-rules/test`). */
export type EventRuleTestResult = components['schemas']['RuleTestResult'];

/** One received event (`GET /api/v1/events`).
 *
 *  `at_unix_ms` is **event time** — when the device says it happened. The log is filtered, ordered
 *  and *paged* by this; `eventCursor()` turns it into the `before` parameter. `recorded_at` is
 *  ingest time and informational only: it is not the paging cursor and not what `start`/`end`
 *  filter, because the VictoriaLogs path has no true ingest time to report, so only event time can
 *  agree across both backends. `trap_name` is the well-known MIB name for `trap_oid`, resolved
 *  server-side; null for syslog/webhook events or an OID outside the curated set. */
export type EventRow = components['schemas']['EventRow'];

/** One categorical `/events/stats` bucket. `key` is stable for the list key + fallback display;
 *  `label` is a server-resolved display hint (e.g. a trap's MIB name); `node_id` is set for source
 *  grouping when the source maps to an inventory node (resolve its name via `useEntityNames`, per
 *  the no-raw-UUID rule). */
export type EventStatBucket = components['schemas']['EventStatBucket'];

/** One time bucket for the `/events/stats?group_by=time` volume series. `by_kind` is present only
 *  when `split=kind` was requested. */
export type EventTimeBucket = components['schemas']['EventTimeBucket'];

// ── AI-assisted RCA (ADR-029: Settings ▸ AI, and "Explain this incident") ──────────────────────

/** One selectable LLM vendor (`GET /api/v1/llm/config` → `providers`).
 *
 *  Served by the backend rather than hardcoded here so the egress warning the form shows reads the
 *  same value the adapters obey — a copy in the WebUI would drift the day a provider changed.
 *  `leaves_operator_boundary` is true when picking it sends hostnames, addresses, topology and
 *  syslog outside the operator's own cloud; `needs_project` when it wants a GCP project + region
 *  rather than just an API key; `credential_optional` when the credential may be omitted (Vertex on
 *  GKE/GCE uses Workload Identity instead). The `suggested_*` values are hints, not an allowlist —
 *  model ids turn over faster than releases. */
export type LlmProviderChoice = components['schemas']['ProviderChoice'];

/** The stored provider configuration. Never carries the credential — only `has_api_key`. */
export type LlmConfigView = components['schemas']['LlmConfigView'];

/** `GET /api/v1/llm/config`. `config` is null until something has been configured — the normal
 *  state of a fresh install, not an error. */
export type LlmConfigResponse = components['schemas']['LlmConfigResponse'];

/** `PUT /api/v1/llm/config`. `api_key` is write-only and three-valued: omit to keep the stored
 *  credential, `''` to clear it (which is how Vertex moves onto Workload Identity), a value to
 *  replace it. */
export type LlmConfigInput = components['schemas']['LlmConfigInput'];

/** `POST /api/v1/llm/test`. A provider failure comes back as `{ ok: false, error }` on HTTP 200 —
 *  the configuration is the caller's, not a server fault. */
export type LlmTestResult = components['schemas']['LlmTestResult'];

/** A generated (or cached) report (`POST /api/v1/rca`, `GET /api/v1/rca/:id`). `node_id` is the
 *  node the report is filed under — the root cause, after any roll-up hop — and `cached` says
 *  whether this delivery came from the cache rather than a fresh (billed) call. */
export type RcaReport = components['schemas']['RcaReport'];

/** `POST /api/v1/rca`. */
export type RcaRequestInput = components['schemas']['RcaBody'];

// ════════════════════════════════════════════════════════════════════════════════════════════════
// NO BACKEND COUNTERPART — hand-written on purpose
//
// Everything below is hand-written because there is nothing in `web/src/api/schema.d.ts` to
// re-export. Two distinct reasons, kept apart because they call for different follow-ups:
//
//   (a) the WebUI invented the shape (query-parameter unions, filter bags, documents the backend
//       stores opaquely) — nothing to generate, and nothing to fix;
//   (b) the backend *has* the concept but types it as `String` or `serde_json::Value`, so the
//       generated document says `string` / `unknown`. Those are unguarded: they can drift exactly
//       the way the whole file used to. Each one names the Rust definition to check.
//
// Do not add to either group to dodge a regeneration. If the API returns it, generate it.
// ════════════════════════════════════════════════════════════════════════════════════════════════

// ── (a) Shapes the WebUI invents ────────────────────────────────────────────────────────────────

/** Optional server-side aggregation for a node metric read. `max` collapses a per-entity
 *  table gauge (e.g. CPU% per entPhysicalIndex) into one node-level value. A query-parameter
 *  value, not a body shape — the document types the `agg` parameter as a bare `string`. */
export type MetricAgg = 'max';

/** Aggregation for the fleet Top-N endpoint: `now` = most recent value; `max_1h` = hourly peak.
 *  A query-parameter value (`?agg=`), typed `string` in the document. */
export type MetricTopAgg = 'now' | 'max_1h';

/** Interface Top-N dimension (`GET /api/v1/metrics/interface-top?metric=`). A query-parameter
 *  value, typed `string` in the document. */
export type InterfaceTopMetric = 'throughput' | 'in_bps' | 'out_bps' | 'errors' | 'discards';

/** Optional drill-down filters shared by the flow endpoints (protocol / destination port / peer /
 *  AS). A bag the client spreads into a query string — the endpoints take these as individual
 *  query parameters, so there is no request body schema for it.
 *
 *  **Comma-separated sets since ADR-053 Inc.8**, and therefore `string` rather than `number`: one
 *  value is still one value (`proto=6`), several are `proto=6,17`, and the endpoint 400s on a token
 *  it cannot parse rather than silently returning the unfiltered top-N. Building the string is
 *  `components/NodeDetail/flowTabFilters.ts`'s job, which is also where each column's parser
 *  lives — a raw string assembled anywhere else would bypass the validation. */
export interface FlowFilters {
  proto?: string;
  port?: string;
  peer?: string;
  asn?: string;
}

/** One placed section instance in a report definition's spec. Part of `ReportSpec` — see below. */
export interface ReportSectionInstance {
  id: string;
  kind: string;
  settings: Record<string, unknown>;
}

/** The report document (definition `spec`). The backend stores it as an opaque `serde_json::Value`
 *  and never interprets it — the frontend owns and migrates this shape (same contract as the
 *  dashboard layout document), so it is genuinely a WebUI type, not an un-generated Rust one.
 *  `reports/types.ts` holds the sanitizer that tolerates older documents. */
export interface ReportSpec {
  version: number;
  params: { range_secs: number };
  sections: ReportSectionInstance[];
}

/** Anomaly finding detail — the report chart's real series. The WebUI's reading of one shape of
 *  `AnalysisFinding.detail`, which the backend serializes opaquely per tool. */
export interface AnomalyDetail {
  points: { t: number; v: number; recent: boolean }[];
  mean: number;
  sigma: number;
  recent_from: number;
}

// ── (b) Backend concepts the generated document does not describe ───────────────────────────────
// Each of these is a real backend value that Rust types as `String` (or hides inside a
// `serde_json::Value`). Nothing checks them; changing the Rust side changes nothing here.

/** Which diagnostic a job runs. Rust: `yagra_core::analysis::AnalysisTool`, serialized into
 *  `AnalysisJob.tool` as a `String`, so the document offers no union to pin this to. */
export type AnalysisToolKey =
  | 'anomaly'
  | 'correlation'
  | 'capacity'
  | 'flap'
  // Passive monitoring (events, ADR-024)
  | 'event_storm'
  | 'event_flap'
  | 'severity_shift'
  | 'rule_gap'
  | 'auth_probe'
  // Flow monitoring (ClickHouse, ADR-031)
  | 'traffic_anomaly'
  | 'talker_shift'
  | 'new_destination'
  | 'flow_scan'
  // Cross-store
  | 'saturation'
  | 'incident_correlate';

/** The severity buckets a finding carries, most severe first. Rust:
 *  `yagra_core::analysis::FINDING_SEVERITIES`, serialized into `AnalysisFinding.severity` /
 *  `SavedFinding.severity` as a `String`, so — like `AnalysisToolKey` — the document offers no
 *  union to pin this to. An array rather than a bare union because the Saved-findings filter
 *  iterates it and `i18nEnumKeys.test.ts` needs it at runtime (`extensibility.md` §4). */
export const FINDING_SEVERITIES = ['crit', 'warn', 'info'] as const;
export type FindingSeverity = (typeof FINDING_SEVERITIES)[number];

/** Schedule cadence presets. `unknown` is a *storage* degradation the backend emits when a row was
 *  written by a newer core — never something an operator may pick, which is why the schedule form
 *  offers a deliberate subset (see `reports/runStatus.ts`). */
export const CADENCES = ['daily', 'weekly', 'monthly', 'unknown'] as const;
export type Cadence = (typeof CADENCES)[number];

/** Lifecycle of a generated report run. */
export const REPORT_RUN_STATES = [
  'queued',
  'running',
  'succeeded',
  'failed',
  'unknown',
] as const;
export type ReportRunState = (typeof REPORT_RUN_STATES)[number];

/** What started a run. */
export const REPORT_TRIGGERS = ['manual', 'scheduled', 'unknown'] as const;
export type ReportTrigger = (typeof REPORT_TRIGGERS)[number];

/** Lifecycle of a Troubleshoot analysis run.
 *
 *  ⚠️ **Not the same vocabulary as a report run**, and the difference has already cost something:
 *  a finished analysis is `done`, a finished report is `succeeded`. While this was a bare `string`
 *  the completion toast compared against `'succeeded'`, so every successful analysis announced
 *  itself as a failure. Both are unions now, and the comparison is a compile error. */
export const ANALYSIS_JOB_STATES = [
  'queued',
  'running',
  'done',
  'failed',
  'cancelled',
  'unknown',
] as const;
export type AnalysisJobState = (typeof ANALYSIS_JOB_STATES)[number];

/** Where a discovery sweep is in its life (ADR-068).
 *
 *  ⚠️ **No `unknown` member, unlike the report/analysis unions above.** Those carry one because the
 *  backend degrades a storage token a newer core wrote; a scan's state is held in memory and only
 *  ever travels outward, so the backend has no such value to emit. The defensiveness that belongs
 *  on this side lives in `pages/discoveryScans.ts::scanState`, which narrows the wire value and
 *  hands anything unrecognised to a neutral label rather than a red one.
 *
 *  `cancelling`/`cancelled` are declared ahead of the cancel path (ADR-068 Increment 2) so the
 *  exhaustive label map and both locales exist before the state can occur — the same reason
 *  `ANALYSIS_JOB_STATES` carries `queued`. Until then they are never rendered. */
export const DISCOVERY_SCAN_STATES = [
  'queued',
  'running',
  'cancelling',
  'cancelled',
  'done',
] as const;
export type DiscoveryScanState = (typeof DISCOVERY_SCAN_STATES)[number];

/** One row of the discovery scan list. */
export type DiscoveryScanSummary = components['schemas']['ScanSummary'];

/** What asking a sweep to stop returned. Named for what it is: a request, not a confirmation. */
export type CancelRequested = components['schemas']['CancelRequested'];

/** What kind of passive event a source produces. */
export const EVENT_KINDS = ['syslog', 'trap', 'webhook'] as const;
export type EventKind = (typeof EVENT_KINDS)[number];

/** What the pipeline did with an event, least → most consequential (the order the backend's
 *  derived `Ord` uses to pick a winner when several rules match one event). */
export const EVENT_ACTIONS = [
  'none',
  'info',
  'suppressed',
  'cleared',
  'refreshed',
  'fired',
] as const;
export type EventAction = (typeof EVENT_ACTIONS)[number];

/** How a rule's pattern is matched. `unknown` is a storage degradation, not a selectable option. */
export const EVENT_MATCH_KINDS = ['substring', 'regex', 'unknown'] as const;
export type EventMatchKind = (typeof EVENT_MATCH_KINDS)[number];

/** Why a DNS chain did not reach a terminal record set — the payload-free projection of
 *  `DnsFailure`, which is what the `failure_kind` column stores. */
export const DNS_FAILURE_KINDS = [
  'nx_domain',
  'no_data',
  'serv_fail',
  'refused',
  'other_rcode',
  'timeout',
  'loop_detected',
  'depth_exceeded',
  'malformed',
] as const;
export type DnsFailureKind = (typeof DNS_FAILURE_KINDS)[number];

/** A stored collection item with its id/scope/enabled flag.
 *
 *  One TypeScript type covering **two** endpoints with different shapes: `GET /nodes/:id/collection`
 *  answers Rust's `StoredCollectionItem`, which carries `scope_level`/`scope_id` (required), while
 *  `GET /collection-templates/:id/items` answers `TemplateItem`, which has neither. `CollectionEditor`
 *  holds both in one `useState`, so the scope pair is optional here — that union is a WebUI decision,
 *  not something the document says, which is why this stays hand-written on top of the generated
 *  `TemplateItem` rather than re-exporting either one. Splitting the editor's state is the fix;
 *  until then, do not assume `scope_level` is present. Rust: `yagra_core::collection`. */
export type StoredCollectionItem = components['schemas']['TemplateItem'] & {
  scope_level?: ScopeLevel;
  scope_id?: string;
};

/** The model's parsed explanation. Every field is plain text and MUST be rendered as text — never
 *  as HTML or markdown (the model was itself reading untrusted device output). `raw` is set when
 *  the reply was not parseable JSON — its presence is what tells the UI to render one prose block
 *  instead of empty sections. */
export type RcaAnswer = components['schemas']['RcaAnswer'];

/** How sure the model says it is. `unknown` covers both "it said something else" and "it said
 *  nothing" — the backend normalizes the model's untrusted output into this set. */
export const RCA_CONFIDENCES = ['high', 'medium', 'low', 'unknown'] as const;
export type RcaConfidence = (typeof RCA_CONFIDENCES)[number];

/** One node's facts as the model saw them. */
export type RcaNodeFacts = components['schemas']['NodeFacts'];

/** The evidence the answer was grounded in — stored beside it so a reader can check the
 *  explanation rather than trust it (ADR-029's display rule). */
export type RcaEvidence = components['schemas']['IncidentContext'];

/** The `body` of a stored report: the answer, its evidence, and the language it was asked for.
 *
 *  These five types were transcribed by hand while `RcaReport.body` was a bare `serde_json::Value`
 *  — the contract said `unknown`, so `RcaModal` asserted the shape where it read it. The Rust side
 *  now *declares* the shape (`#[schema(value_type = ReportBody)]` over a column that stays loose so
 *  an older row still reads back), which is what retired the mirror. */
export type RcaReportBody = components['schemas']['ReportBody'];

/** The data-retention policy (ADR-040): the operator-editable windows plus the whole table,
 *  including the two rows a store's own start flag owns and the audit log that is never pruned. */
export type RetentionPolicy = components['schemas']['RetentionPolicy'];

/** One line of the retention table. `tunable` decides whether it renders as a control. */
export type RetentionRow = components['schemas']['RetentionRow'];

/** The four windows `PUT /api/v1/settings/retention` accepts. */
export type RetentionValues = components['schemas']['RetentionValues'];

/** Whether every stored credential still decrypts under the loaded KEK — the assertion a database
 *  restore cannot make on its own, since rows can come back whole while the key did not. */
export type CredentialHealth = components['schemas']['CredentialHealth'];

/** A portable configuration bundle (ADR-040 decision 3): the monitoring configuration of one
 *  deployment, for moving into another. Carries **no secrets** — credentials, notification-channel
 *  configs and ingest tokens stay where they were sealed, and only ids cross. It is not a backup;
 *  disaster recovery is a database dump (`scripts/yagra-backup.sh`). */
export type ConfigBundle = components['schemas']['ConfigBundle'];

/** What an import did: per-table counts plus what it skipped or changed and why. */
export type ImportReport = components['schemas']['ImportReport'];

/** One thing the export or the import did that the operator would not otherwise see. */
export type BundleNote = components['schemas']['BundleNote'];

/** Why a row was skipped or changed. A silent drop is the failure that matters here — a bundle
 *  that quietly left half the rules behind imports without error and is discovered mid-incident —
 *  so every one of these gets its own sentence in both locales. */
export const BUNDLE_NOTE_CODES = [
  'skipped_builtin',
  'skipped_missing_reference',
  'reference_dropped',
  'secret_dropped_imported_disabled',
  'webhook_token_reset',
  'schedule_next_run_recomputed',
] as const;

// ── Bus certificate + remote-poller acceptance (ADR-065) ────────────────────────────────────────

/** The certificate NATS serves on the core⇄poller bus (`GET /api/v1/settings/bus`). This is the
 *  file a remote site pins as its `YAGRA_BUS_CA_FILE`; it never carries the key. */
export type BusTlsView = components['schemas']['BusTlsView'];

/** The response wrapper: the certificate, whether the bus is TLS, and whether the switch works. */
export type BusStatus = components['schemas']['BusResponse'];

/** What flipping the switch returns. `poller_secret` is shown once and never again. */
export type BusRemoteAccepted = components['schemas']['BusRemoteAccepted'];

// ── WebUI TLS certificate (ADR-044) ─────────────────────────────────────────────────────────────

/** The certificate the WebUI is serving (`GET /api/v1/settings/tls`). Never carries the key. */
export type WebTlsView = components['schemas']['WebTlsView'];

/** The response wrapper: the certificate plus whether core's plaintext API port is still open. */
export type WebTlsStatus = components['schemas']['WebTlsResponse'];

/** Where a certificate came from, which is what decides whether Yagra may replace it on its own.
 *
 *  An `as const` array rather than only the generated union, for the reason in `LINK_SOURCES`: the
 *  settings page builds its label key at runtime (`` t(`source.${source}`) ``), and EN/JA parity
 *  cannot prove either locale is complete — a variant added later would be missing from both, so
 *  parity would pass while the page rendered a raw key. `i18nEnumKeys.test.ts` iterates this. */
export const TLS_CERT_SOURCES = ['self_signed', 'imported', 'unknown'] as const;
export type TlsCertSource = (typeof TLS_CERT_SOURCES)[number];
