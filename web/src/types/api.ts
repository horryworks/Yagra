// SPDX-License-Identifier: AGPL-3.0-only
// API types — mirror the Rust serde shapes from `yagra-common` / the `/api/v1` router.
// Keep these in sync when an endpoint or a shared type changes (coding-conventions).

/** The node/check state machine (yagra-common `NodeState`). */
export type NodeState =
  | 'ok'
  | 'warning'
  | 'critical'
  | 'unknown'
  | 'unreachable'
  | 'maintenance';

/** Alert severity (yagra-common `Severity`), ordered info < warning < critical. */
export type Severity = 'info' | 'warning' | 'critical';

/** Optional server-side aggregation for a node metric read. `max` collapses a per-entity
 *  table gauge (e.g. CPU% per entPhysicalIndex) into one node-level value. */
export type MetricAgg = 'max';

/** Latest reading for one node metric (`GET /api/v1/nodes/:id/metrics/:metric`). */
export interface MetricReading {
  node_id: string;
  metric: string;
  value: number;
}

/** One time-series point: `t` Unix seconds, `v` value. */
export interface MetricPoint {
  t: number;
  v: number;
}

/** A time-series window (`GET /api/v1/nodes/:id/metrics/:metric/range`). */
export interface MetricRange {
  node_id: string;
  metric: string;
  points: MetricPoint[];
}

/** Aggregation for the fleet Top-N endpoint: `now` = most recent value; `max_1h` = hourly peak. */
export type MetricTopAgg = 'now' | 'max_1h';

/** One ranked node in a fleet Top-N result (`GET /api/v1/metrics/top`). `name` is joined from
 *  PostgreSQL (TSDB carries only the id); it falls back to the id for a since-deleted node. */
export interface TopEntry {
  node_id: string;
  name: string;
  value: number;
}

// ── Flow analysis (ADR-031) — mirror the Rust `flowstore` DTOs ──
/** A top-talker: one host address with summed traffic (`/nodes/:id/flow/top-talkers`). */
export interface FlowTalker {
  addr: string;
  bytes: number;
  packets: number;
  flows: number;
}

/** A src→dst conversation with summed traffic (`/nodes/:id/flow/conversations`). `*_asn` 0 = unknown;
 *  `*_as_name` is filled when the IP→ASN table resolves it. Optional for N-1 tolerance. */
export interface FlowConversation {
  src: string;
  dst: string;
  src_asn?: number;
  dst_asn?: number;
  src_as_name?: string | null;
  dst_as_name?: string | null;
  bytes: number;
  packets: number;
  flows: number;
}

/** A destination-port aggregate (`/nodes/:id/flow/top-ports`). */
export interface FlowPortAgg {
  port: number;
  bytes: number;
  packets: number;
  flows: number;
}

/** A protocol aggregate (`/nodes/:id/flow/protocols`). `proto` is the IP protocol number. */
export interface FlowProtoAgg {
  proto: number;
  bytes: number;
  packets: number;
  flows: number;
}

/** A trend point: bytes/packets for one protocol at one 5-minute bucket (`/nodes/:id/flow/series`). */
export interface FlowPoint {
  ts_unix_ms: number;
  proto: number;
  bytes: number;
  packets: number;
}

/** An autonomous-system aggregate (`/nodes/:id/flow/top-as`). `asn` 0 = unknown; `name` when
 *  the IP→ASN table resolves it (else `null`). */
export interface FlowAsAgg {
  asn: number;
  name?: string | null;
  bytes: number;
  packets: number;
  flows: number;
}

/** Optional drill-down filters shared by the flow endpoints (protocol / destination port / peer / AS). */
export interface FlowFilters {
  proto?: number;
  port?: number;
  peer?: string;
  asn?: number;
}

/** Interface Top-N dimension (`GET /api/v1/metrics/interface-top?metric=`). */
export type InterfaceTopMetric = 'throughput' | 'in_bps' | 'out_bps' | 'errors' | 'discards';

/** One ranked interface in a fleet interface Top-N. `value` is bits/sec for throughput metrics,
 *  errors|discards per second otherwise; node/interface names joined from PostgreSQL. */
export interface InterfaceTopEntry {
  node_id: string;
  node_name: string;
  ifindex: number;
  if_name: string | null;
  if_alias: string | null;
  if_speed_bps: number | null;
  value: number;
}

/**
 * Inbound acknowledgement state mirrored from an external incident tool (PagerDuty / JSM),
 * shown read-only (ADR-015). Yagra has no ack action of its own — this is reflected in only.
 */
export interface AckView {
  /** When the external tool recorded the ack (Unix ms, UTC). */
  at_unix_ms: number;
  /** External actor reference (id / handle). */
  by: string;
  /** Originating tool: 'pagerduty' | 'jsm' | 'manual' | … */
  source: string;
  /** Optional free-text note from the external tool. */
  note?: string;
}

/** An alert as produced by the engine (`yagra_alert::Alert`), plus inbound ack state. */
export interface Alert {
  node: string;
  check: string;
  severity: Severity;
  state: NodeState;
  at_unix_ms: number;
  root_cause: string | null;
  flapping: boolean;
  /** Read-only ack mirrored from the external tool (absent ⇒ not acknowledged). */
  acked?: AckView | null;
}

/** A node row for inventory listings. */
export interface NodeSummary {
  id: string;
  name: string;
  address: string;
  state: NodeState;
  /** Descriptive maker/model for the "name (addr) (vendor) (model)" display. */
  vendor: string | null;
  model: string | null;
  /** The group this node belongs to (inventory tree); `null` ⇒ ungrouped. */
  group_id: string | null;
  /** Manual order within the group (tree sorts members by this, then by name). */
  sort_order: number;
  /** How this node is monitored, for the tree badge: `"meraki"` or `"device"` (default). */
  source?: 'device' | 'meraki';
}

/** One resolved node id → display name (`POST /api/v1/node-names`). Unresolved ids are omitted
 *  from the response, so the caller keeps the raw id as the fallback (S12). */
export interface NodeNameEntry {
  id: string;
  name: string;
}

/** One node-picker search hit (`GET /api/v1/nodes/search`): id + display name + address only —
 *  enough to show and select a node without loading the whole inventory client-side (A-2). */
export interface NodeSearchResult {
  id: string;
  name: string;
  address: string;
}

/** One group's direct member nodes (`GET /api/v1/nodes/by-group?group=<id>`; absent group ⇒ the
 *  ungrouped bucket). The inventory tree lazy-loads a group's members only when it is expanded, so
 *  the initial view never pulls the whole fleet (A-3). `truncated` ⇒ the group exceeded the load
 *  backstop (a pathologically large flat group). */
export interface GroupNodesResult {
  nodes: NodeSummary[];
  truncated: boolean;
}

/** A node-group type (yagra-core `GroupType`, snake_case) — drives the tree icon. */
export type GroupType = 'site' | 'region' | 'device_type' | 'service' | 'generic';

/** One node group (folder) in the hierarchical inventory tree (`GET /api/v1/node-groups`). */
export interface NodeGroup {
  id: string;
  name: string;
  group_type: GroupType;
  /** Parent group; `null` ⇒ a top-level group. */
  parent_id: string | null;
  /** Manual order within the parent scope (tree sorts siblings by this, then by name). */
  sort_order: number;
  /** Optional geo coordinates for the dashboard map (both set ⇒ plotted). */
  latitude: number | null;
  longitude: number | null;
}

/** One node's live status (`GET /api/v1/nodes/:id/status`): rolled-up display state plus the
 *  alerts currently attributed to it. */
export interface NodeStatus {
  node_id: string;
  state: NodeState;
  alerts: Alert[];
}

/** Credential metadata (never the secret value). */
export interface CredentialSummary {
  id: string;
  name: string;
  kind: string;
  /** How many nodes reference this credential (0 ⇒ unused, safe to delete). */
  used_by: number;
}

/** A page of the keyset-paginated node list (`GET /api/v1/nodes`). */
export interface NodePage {
  nodes: NodeSummary[];
  next_cursor: string | null;
}

/** Current principal (`GET /api/v1/auth/me`). */
export interface AuthMe {
  role: string;
  /** The signed-in account's username — lets the UI mark "you" in the user list. */
  username: string;
}

/** A predefined role (yagra-common `Role`, snake_case), ordered least → most privileged. */
export type Role = 'viewer' | 'operator' | 'admin';

/** One capability in the role/privilege matrix (`GET /api/v1/roles`). */
export interface PermissionInfo {
  key: string;
  label: string;
  description: string;
}

/** One role in the matrix: metadata plus the permission keys it grants. `builtin` roles are
 *  fixed today (custom roles are not yet configurable). */
export interface RoleInfo {
  key: string;
  label: string;
  description: string;
  builtin: boolean;
  permissions: string[];
}

/** The role-vs-privilege matrix (`GET /api/v1/roles`): the permission catalogue + per-role grants. */
export interface RoleMatrix {
  permissions: PermissionInfo[];
  roles: RoleInfo[];
}

/** A user account row (`GET /api/v1/users`, core `UserSummary`). Never includes the
 *  password hash. `created_at` is RFC 3339 text; `last_login_at` is RFC 3339 text or null
 *  (the account has never logged in). */
export interface UserSummary {
  id: string;
  username: string;
  role: Role;
  created_at: string;
  last_login_at: string | null;
  /** Account status: a disabled account is kept for the record but cannot authenticate. */
  enabled: boolean;
  /** How the account authenticates: `local` (password) or `oidc` (external IdP). */
  auth_source: 'local' | 'oidc';
}

/** A configured OIDC provider (`GET /api/v1/settings/oidc`, core `OidcProviderSummary`). The
 *  client_secret is write-only and never returned; `has_secret` signals one is stored. */
export interface OidcProviderSummary {
  id: string;
  name: string;
  issuer: string;
  client_id: string;
  redirect_uri: string;
  scopes: string;
  groups_claim: string;
  /** IdP group name → Yagra role. */
  role_map: Record<string, Role>;
  default_role: Role | null;
  enabled: boolean;
  has_secret: boolean;
}

/** Create/update payload for an OIDC provider. `client_secret` is write-only: omit (or empty) on
 *  update to keep the stored secret. */
export interface OidcProviderInput {
  name: string;
  issuer: string;
  client_id: string;
  client_secret?: string;
  redirect_uri: string;
  scopes: string;
  groups_claim: string;
  role_map: Record<string, Role>;
  default_role?: Role | null;
  enabled: boolean;
}

/** An API token (`GET /api/v1/api-tokens`, core `ApiTokenInfo`) — a long-lived credential for
 *  non-browser/MCP clients (ADR-028). The raw token is write-only: shown once on create, never
 *  returned again; only metadata is listed. `scope` is the serde JSON of the core `Scope`
 *  (`"All"` or `{ "Groups": [...] }`) rendered as a string. */
export interface ApiTokenSummary {
  id: string;
  name: string;
  role: Role;
  scope: unknown;
  created_by: string;
  created_at: string;
  last_used_at: string | null;
  revoked_at: string | null;
}

/** Create payload for an API token. `scope` is omitted for global (All) visibility — the only
 *  scope the MCP tool surface accepts in Increment 1. */
export interface ApiTokenInput {
  name: string;
  role: Role;
  scope?: string;
}

/** Create response: the raw `token` is shown once and never returned again. */
export interface CreatedApiToken {
  id: string;
  name: string;
  role: Role;
  token: string;
}

// ── Forwarding ("tee", ADR-034) — relay received syslog/traps to external collectors ────────────

/** Which received stream a destination tees (core `yagra_forward::SourceKind`). */
export type ForwardSourceKind = 'syslog' | 'trap' | 'flow';

/** How a destination is spoken to (core `yagra_forward::DestKind`). */
export type ForwardDestKind =
  | 'syslog_udp'
  | 'syslog_tcp'
  | 'syslog_tls'
  | 'snmp_trap_udp'
  | 'flow_udp';

/** A filter condition's subject (core `yagra_forward::FilterField`). For flow, `source_ip` is the
 *  exporting device and `kind` is the wire format (`netflow`/`sflow`); the record's own endpoints
 *  are `src_addr`/`dst_addr`. */
export type ForwardFilterField =
  | 'source_ip'
  | 'pool'
  | 'kind'
  | 'facility'
  | 'severity'
  | 'hostname'
  | 'app_name'
  | 'message'
  | 'trap_oid'
  | 'varbind'
  | 'src_addr'
  | 'dst_addr'
  | 'proto'
  | 'src_port'
  | 'dst_port'
  | 'src_as'
  | 'dst_as';

/** A filter condition's comparison (core `yagra_forward::FilterOp`). */
export type ForwardFilterOp =
  | 'eq'
  | 'ne'
  | 'contains'
  | 'not_contains'
  | 'prefix'
  | 'regex'
  | 'not_regex'
  | 'in_list'
  | 'in_cidr'
  | 'not_in_cidr'
  | 'lte'
  | 'gte';

/** One `field op value` test. `value` is always a string; core parses it per field type. */
export interface ForwardCondition {
  field: ForwardFilterField;
  op: ForwardFilterOp;
  value: string;
}

/** A destination's filter (core `yagra_forward::FilterExpr`). No conditions = forward everything. */
export interface ForwardFilter {
  mode: 'all' | 'any';
  conditions: ForwardCondition[];
}

/** A forwarding destination (`GET /api/v1/forwarding/destinations`, core `ForwardDestination`).
 *  `has_secret` reports whether a sealed secret (the SNMP community) is stored — the value itself
 *  is never returned. */
export interface ForwardDestination {
  id: string;
  name: string;
  enabled: boolean;
  source_kind: ForwardSourceKind;
  dest_kind: ForwardDestKind;
  target: string;
  pool: string | null;
  verbatim: boolean;
  filter: ForwardFilter;
  rate_limit_per_sec: number | null;
  /** Extra PEM trust anchor for a TLS destination. Not a secret, so it round-trips. */
  ca_cert: string | null;
  has_secret: boolean;
}

/** Create/update payload. Omitting `community` on update keeps the stored value; `ca_cert` is not a
 *  secret and is always sent, so clearing it removes the pinned CA. */
export interface ForwardDestinationInput {
  name: string;
  enabled: boolean;
  source_kind: ForwardSourceKind;
  dest_kind: ForwardDestKind;
  target: string;
  pool?: string | null;
  verbatim: boolean;
  filter: ForwardFilter;
  rate_limit_per_sec?: number | null;
  ca_cert?: string | null;
  community?: string;
}

/** One destination's live counters (core `forward::DestStatus`). `rendered` counts messages that
 *  were sent re-rendered despite `verbatim`, because no original payload was available. */
export interface ForwardDestStatus {
  id: string;
  name: string;
  sent: number;
  filtered: number;
  dropped: number;
  errors: number;
  rendered: number;
  queue_depth: number;
  circuit_open: boolean;
  last_success_unix_ms: number | null;
  last_error: string | null;
}

/** `GET /api/v1/forwarding/status`. `sending` is false on an HA standby, whose dispatcher is idle. */
export interface ForwardStatus {
  destinations: ForwardDestStatus[];
  /** Online pollers that attach no original bytes to events — byte-exact silently degrades. */
  pollers_without_raw_capture: string[];
  /** Online pollers that relay no flow datagrams at all — a flow destination receives *nothing*
   *  from them, which is a different failure from degraded fidelity. */
  pollers_without_flow_relay: string[];
  sending: boolean;
}

/** `POST /api/v1/forwarding/destinations/:id/test` — the transport result, not an HTTP error. */
export interface ForwardTestResult {
  delivered: boolean;
  error?: string;
}

/** A device-class profile (`GET /api/v1/profiles`, repo `ProfileSummary`). Split by functional
 *  `category` (role) × `vendor`-NOS family; `category` is the kebab-case `ProfileCategory` token. */
export interface ProfileSummary {
  id: string;
  name: string;
  category: string;
  vendor: string | null;
  /** Per-profile polling-interval override (seconds); `null` ⇒ inherit the system default. */
  poll_interval_secs: number | null;
}

/** Create/update-profile request body (`POST`/`PUT /api/v1/profiles`). */
export interface ProfileInput {
  name: string;
  category?: string;
  vendor?: string | null;
  /** Optional polling-interval override (seconds); omit/`null` to inherit the system default. */
  poll_interval_secs?: number | null;
}

/** Threshold scope level (yagra-common `ScopeLevel`, snake_case). Most-specific wins. */
export type ScopeLevel = 'profile' | 'group' | 'node';

/** Maintenance-window scope. The threshold scopes plus `group_id` — a hierarchical folder group
 *  (the All Nodes tree), resolved recursively incl. subgroups (ADR-022). Distinct from the legacy
 *  tag-based `group` scope (`scope_id` is a group UUID, not a tag value). */
export type MaintenanceScopeLevel = ScopeLevel | 'group_id';

/** Breach direction (yagra-common `Direction`, snake_case). */
export type Direction = 'above' | 'below';

/** A stored threshold rule (`GET /api/v1/thresholds`, core `StoredThreshold`). The rule
 *  fields are flattened onto the row. The GET shape uses `scope_level`, matching the POST body. */
export interface StoredThreshold {
  id: string;
  scope_level: ScopeLevel;
  scope_id: string;
  metric: string;
  direction: Direction;
  warning: number | null;
  critical: number | null;
  dwell_samples: number;
}

/** One alert-history row (`GET /api/v1/alerts/history`, core `AlertHistoryRow`). */
export interface AlertHistoryRow {
  node: string;
  check: string;
  severity: Severity;
  state: NodeState;
  at_unix_ms: number;
  resolved: boolean;
  /** Metric the check measured (e.g. `icmp_rtt_ms`, or the liveness sentinel `__liveness__`).
   *  `null` for rows recorded before this was captured ⇒ render "—". */
  metric?: string | null;
  /** Observed sample value that committed the transition (threshold checks only). */
  observed_value?: number | null;
  /** The bound crossed for the committed severity (threshold checks only). */
  threshold_value?: number | null;
  /** Breach direction (threshold checks only). */
  direction?: Direction | null;
  /** Insertion time (RFC 3339). The keyset cursor for paging: pass the last row's `recorded_at`
   *  as `before` to fetch the next (older) page. */
  recorded_at: string;
  /** Read-only ack mirrored from the external tool, keyed by incident (absent ⇒ not acked). */
  acked?: AckView | null;
}

/** A chronic-offender row (`GET /api/v1/alerts/top-nodes`). */
export interface AlertNodeCount {
  node_id: string;
  name: string;
  count: number;
}

/** One weekday×hour heatmap cell (`GET /api/v1/alerts/calendar`); `dow` 0=Sun…6=Sat, hour 0–23 (UTC). */
export interface CalendarBucket {
  dow: number;
  hour: number;
  count: number;
}

/** A recent up/down transition (`GET /api/v1/alerts/transitions`). `resolved` = recovery to ok. */
export interface AlertTransition {
  node_id: string;
  name: string;
  state: NodeState;
  severity: Severity;
  resolved: boolean;
  at_unix_ms: number;
}

/** A notification channel kind (yagra-core `ChannelKind`). */
export type ChannelKind = 'webhook' | 'email' | 'pagerduty' | 'jsm';

/** Notification channel metadata (`GET /api/v1/notification-channels`) — the secret
 *  connection config is sealed server-side and never returned. */
export interface NotificationChannel {
  id: string;
  name: string;
  kind: ChannelKind;
  enabled: boolean;
}

/** Channel connection config supplied on create (sealed server-side; tagged by `kind`). */
export type ChannelConfigInput =
  | { kind: 'webhook'; url: string }
  | {
      kind: 'email';
      host: string;
      port?: number;
      from: string;
      to: string;
      user?: string;
      pass?: string;
    }
  // PagerDuty Events API v2 — routing_key is the integration key; api_url overrides the
  // default US endpoint (EU: https://events.eu.pagerduty.com/v2/enqueue).
  | { kind: 'pagerduty'; routing_key: string; api_url?: string }
  // JSM Alerts (Opsgenie-compatible) — api_url is the integration base, api_key the GenieKey.
  | { kind: 'jsm'; api_url: string; api_key: string };

/** A routing rule (`GET /api/v1/routing-rules`): alerts of `severity` (null = any) fan out
 *  to `channel_ids`. */
export interface RoutingRule {
  id: string;
  name: string;
  enabled: boolean;
  severity: Severity | null;
  channel_ids: string[];
}

/** How a collection item is gathered (yagra-common `CollectionKind`). */
export type CollectionKind = 'scalar' | 'table';

/** Whether a metric is a gauge or a raw counter (yagra-common `MetricKind`). */
export type MetricKind = 'gauge' | 'counter';

/** One thing to collect: a stable metric name, the OID, how to collect it, and its kind
 *  (`yagra-common::CollectionItem`). */
export interface CollectionItem {
  metric_name: string;
  oid: string;
  kind: CollectionKind;
  metric_kind: MetricKind;
}

/** A stored collection item with its id/scope/enabled flag (core `StoredCollectionItem`,
 *  the item fields flattened on). Also the shape of a template item (core `TemplateItem`,
 *  without the scope fields). */
export interface StoredCollectionItem extends CollectionItem {
  id: string;
  scope_level?: ScopeLevel;
  scope_id?: string;
  enabled: boolean;
}

/** A reusable collection template (core `TemplateSummary`): a named metric bundle that
 *  device profiles attach. `item_count` is how many metrics it carries. */
export interface CollectionTemplate {
  id: string;
  name: string;
  description: string | null;
  item_count: number;
}

/** One device a discovery scan found (`core::discovery::Candidate`). */
export interface DiscoveryCandidate {
  address: string;
  reachable: boolean;
  sysdescr: string | null;
  sysname: string | null;
  /** `sysObjectID` (dotted) if it answered SNMP — the authoritative device-type signal. */
  sysobjectid: string | null;
  /** Suggested profile **id**, resolved server-side via the classification rules (by
   *  sysObjectID prefix, else sysDescr regex, else Generic SNMP). An id, not a name, so the
   *  row pre-selects robustly even if the profile was renamed. */
  suggested_profile_id: string | null;
  /** Maker/model (rule-pinned or best-effort from sysDescr) — pre-fills the import row. */
  vendor: string | null;
  model: string | null;
  /** The stored credential that answered SNMP, by id — preselected on import. */
  matched_credential_id: string | null;
}

/** A device-classification rule (`GET /api/v1/classification-rules`, `yagra_common::ClassificationRule`).
 *  Maps a discovered device's SNMP signature to a profile; evaluated by ascending priority. */
export interface ClassificationRule {
  id: string;
  priority: number;
  /** Dotted-OID prefix matched against sysObjectID (authoritative), e.g. `1.3.6.1.4.1.9.`. */
  sysobjectid_prefix: string | null;
  /** Regex matched against sysDescr (fallback). */
  sysdescr_regex: string | null;
  /** Profile this rule suggests. */
  profile_id: string;
  vendor: string | null;
  model: string | null;
  enabled: boolean;
}

/** Create/update body for a classification rule. */
export interface ClassificationRuleInput {
  priority: number;
  sysobjectid_prefix?: string | null;
  sysdescr_regex?: string | null;
  profile_id: string;
  vendor?: string | null;
  model?: string | null;
  enabled: boolean;
}

/** A discovery scan's status (`GET /api/v1/discovery/scan/:id`). */
export interface DiscoveryScan {
  scan_id: string;
  done: boolean;
  /** Targets probed so far / total targets in the sweep. */
  probed: number;
  total: number;
  /** The address the sweep is currently at, while running. */
  scanning: string | null;
  candidates: DiscoveryCandidate[];
}

/** One curated OID-catalog entry (`GET /api/v1/mib-catalog`, core `MibEntry`). A reference
 *  metric_name → (oid, kind) so the collection editor can pick by name. */
export interface MibCatalogEntry {
  id: string;
  metric_name: string;
  oid: string;
  collection: CollectionKind;
  metric_kind: MetricKind;
  vendor: string | null;
  description: string | null;
}

/** A maintenance window (`GET /api/v1/maintenance-windows`, core `StoredWindow`). Nodes
 *  covered by an active window observe `maintenance` — no alerts fire, existing ones
 *  resolve. Scoped like thresholds; times are RFC 3339. `active` = covers "now". */
export interface MaintenanceWindow {
  id: string;
  name: string;
  scope_level: MaintenanceScopeLevel;
  scope_id: string;
  starts_at: string;
  ends_at: string;
  enabled: boolean;
  active: boolean;
}

/** A mute (`GET /api/v1/mutes`, core `StoredMute`): notifications are silenced until `until_at`
 *  — the alert still shows in the UI/history. A `node` mute targets one node (optionally one
 *  `metric_name`); a `group` mute targets every node under a folder group (recursive). Exactly
 *  one of `node_id` / `group_id` is set, per `scope_kind`. */
export interface Mute {
  id: string;
  scope_kind: 'node' | 'group';
  node_id: string | null;
  group_id: string | null;
  metric_name: string | null;
  until_at: string;
  reason: string | null;
}

/** Which HTTP status codes count as "up" for a URL monitor (mirrors yagra_common::ExpectedStatus,
 *  a tagged object). */
export type ExpectedStatus =
  | { kind: 'two_xx' }
  | { kind: 'exact'; codes: number[] }
  | { kind: 'range'; lo: number; hi: number };

/** A node's URL-monitor configuration (1:1 with the node). `url` is the only required field on
 *  create; the rest default server-side. */
export interface UrlCheckConfig {
  url: string;
  method: 'GET' | 'HEAD' | 'POST';
  expected_status: ExpectedStatus;
  verify_tls: boolean;
  follow_redirects: boolean;
  timeout_ms: number;
  /** Reserved for Basic/Bearer/Header auth; unused in the MVP. */
  credential: string | null;
}

/** Which record type a DNS monitor resolves to (mirrors yagra_common::DnsRecordType). */
export type DnsRecordType = 'A' | 'AAAA' | 'CNAME';

/** One record in a resolution answer set (mirrors yagra_common::DnsRecord, a tagged object). */
export type DnsRecord =
  | { kind: 'cname'; target: string }
  | { kind: 'a'; addr: string }
  | { kind: 'aaaa'; addr: string };

/** One answer plus its TTL. The TTL is display-only — it is not part of the change key. */
export interface DnsAnswer {
  record: DnsRecord;
  ttl: number;
}

/** One hop of a resolution chain: the name queried and the answers for it (canonical order). */
export interface DnsHop {
  name: string;
  answers: DnsAnswer[];
}

/** Why a resolution did not reach a terminal record set (mirrors yagra_common::DnsFailure). */
export type DnsFailure =
  | { kind: 'nx_domain' }
  | { kind: 'no_data' }
  | { kind: 'serv_fail' }
  | { kind: 'refused' }
  | { kind: 'other_rcode'; rcode: number }
  | { kind: 'timeout' }
  | { kind: 'loop_detected'; at: string }
  | { kind: 'depth_exceeded'; max_depth: number }
  | { kind: 'malformed' };

/** One observed resolution chain — the artifact of a DNS check. */
export interface DnsChain {
  query: string;
  record_type: DnsRecordType;
  /** Where we asked (`10.0.0.53:53`, or `system`). Provenance, not content. */
  resolver: string;
  /** Hops in walk order; the order is significant. */
  hops: DnsHop[];
  /** `null` ⇒ the chain resolved. */
  failure: DnsFailure | null;
  resolve_ms: number;
}

/** A node's DNS-monitor configuration (1:1 with the node). `name` is the only required field on
 *  create; the rest default server-side. */
export interface DnsCheckConfig {
  name: string;
  record_type: DnsRecordType;
  /** `null` ⇒ the poller's system resolver. */
  resolver: string | null;
  resolver_port: number;
  max_depth: number;
  timeout_ms: number;
}

/** The current chain plus how long it has held (`GET /api/v1/nodes/:id/dns-chain`). */
export interface DnsChainCurrent {
  chain: DnsChain;
  resolved: boolean;
  failure_kind: string | null;
  first_seen: string;
  last_seen: string;
}

/** One append-on-change history row (`GET /api/v1/nodes/:id/dns-chain/history`). */
export interface DnsChainChange {
  id: number;
  at: string;
  chain: DnsChain;
  resolved: boolean;
  failure_kind: string | null;
  /** `null` on the first observation for this node. */
  prev_chain_key: string | null;
}

/** A page of DNS chain history with its keyset cursor (`null` ⇒ no more rows). */
export interface DnsChainHistoryPage {
  changes: DnsChainChange[];
  next: { at: string; id: number } | null;
}

/** One node's configuration detail incl. bindings (`GET /api/v1/nodes/:id`). */
export interface NodeDetail {
  id: string;
  name: string;
  address: string;
  profile_id: string | null;
  credential_id: string | null;
  parent_id: string | null;
  /** Descriptive maker/model, editable from the node detail. */
  vendor: string | null;
  model: string | null;
  /** The group this node belongs to (inventory tree); `null` ⇒ ungrouped. */
  group_id: string | null;
  /** URL-monitor config when this node is a URL monitor; `null` otherwise. */
  url_check: UrlCheckConfig | null;
  /** DNS-monitor config when this node is a DNS monitor; `null` otherwise. */
  dns_check: DnsCheckConfig | null;
  /** Cisco Meraki binding when this node is a Meraki device; `null` otherwise. */
  meraki_device: MerakiDeviceConfig | null;
}

/** A node's Cisco Meraki binding (1:1; part of `NodeDetail`). */
export interface MerakiDeviceConfig {
  org_uuid: string;
  org_id: string;
  serial: string;
  network_id: string;
  product_type: string;
  model: string | null;
}

/** A configured Meraki organization (`GET /api/v1/meraki/orgs`). */
export interface MerakiOrg {
  id: string;
  org_id: string;
  name: string;
  base_url: string;
  enabled: boolean;
  availability_secs: number;
  uplink_secs: number;
  traffic_secs: number;
  inventory_secs: number;
  enabled_tiers: string[];
  target_rps: number;
  group_id: string | null;
}

/** An organization the API key can access (from `POST /api/v1/meraki/orgs/discover`). */
export interface MerakiOrgOption {
  id: string;
  name: string;
}

/** A network within an org, with its monitored (watch/skip) flag. */
export interface MerakiNetwork {
  network_id: string;
  name: string;
  monitored: boolean;
}

/** An import candidate device (from `POST /api/v1/meraki/orgs/:id/enumerate`). */
export interface MerakiCandidate {
  serial: string;
  name: string;
  model: string | null;
  product_type: string;
  network_id: string;
  network_name: string;
  lan_ip: string | null;
}

/** The enumerate response: the org's networks + import candidates. */
export interface MerakiEnumeration {
  networks: MerakiNetwork[];
  devices: MerakiCandidate[];
}

/** One interface row for the node-detail Interfaces tab (`GET /api/v1/nodes/:id/interfaces`).
 *  Rates/utilization are derived at query time; `null` when there's no data or no known speed. */
export interface InterfaceRow {
  ifindex: number;
  if_name: string | null;
  if_alias: string | null;
  if_speed_bps: number | null;
  oper_status: number | null;
  in_bps: number | null;
  out_bps: number | null;
  in_util_pct: number | null;
  out_util_pct: number | null;
  last_seen_unix: number | null;
  stale: boolean;
}

/** Per-interface time-series for the detail pane (`GET /nodes/:id/interfaces/:ifindex/series`).
 *  All arrays share the `timestamps` x-axis; `null` is a gap. `*_bps` are bits/sec (rate of
 *  the octet counters × 8); `*_errors` are errors/sec. */
export interface InterfaceSeries {
  timestamps: number[];
  in_bps: (number | null)[];
  out_bps: (number | null)[];
  in_errors: (number | null)[];
  out_errors: (number | null)[];
}

/** One audit-log row (`GET /api/v1/audit`, core `AuditRow`). Admin-only. `action` is
 *  either `METHOD /api/v1/...` (mutating request) or `auth.login`; `at` is RFC 3339. */
export interface AuditRow {
  id: string;
  at: string;
  username: string;
  action: string;
  status: number;
}

/** Fleet state-count timeline (`GET /api/v1/fleet/state-history`): per-state series aligned to a
 *  shared `timestamps` axis (Unix seconds). `series` is keyed by node state. */
export interface StateHistory {
  timestamps: number[];
  series: Record<string, number[]>;
}

/** Fleet aggregate throughput over time (`GET /api/v1/metrics/throughput-range`); bits/sec, `null` = gap. */
export interface ThroughputRange {
  timestamps: number[];
  in_bps: (number | null)[];
  out_bps: (number | null)[];
}

/** Busiest-links × time throughput heatmap (`GET /api/v1/metrics/interface-heatmap`). `values`
 *  is rows (`links`) × cols (`timestamps`), each cell bits/sec. */
export interface InterfaceHeatmap {
  links: string[];
  timestamps: number[];
  values: number[][];
}

/** Poll-loop self-monitoring (`GET /api/v1/poller-health`). */
export interface PollerHealth {
  last_sweep_unix_ms: number | null;
  jobs_last_round: number;
  results_total: number;
}

/** One registered poller in the distributed pool (`GET /api/v1/pollers`, ADR-009/020). A merge of
 *  the live registry (current status/telemetry) and the durable inventory, so an offline poller
 *  still lists. `last_seen`/`first_seen`/`version` are null for a live poller not yet persisted. */
export interface PollerInfo {
  /** Operator-chosen, subject-safe id (stable across restarts). */
  id: string;
  /** Pool it serves (`default` when unassigned). */
  pool: string;
  status: 'online' | 'offline';
  /** Last durably-recorded contact (RFC 3339), or null when not yet persisted. */
  last_seen: string | null;
  /** First durably-recorded contact (RFC 3339), or null when not yet persisted. */
  first_seen: string | null;
  /** Build version from its latest heartbeat (or the durable row when offline), or null. */
  version: string | null;
  /** Working-set node count it last reported (0 when offline / never reported). */
  working_set_nodes: number;
  /** Working-set spec count it last reported. */
  working_set_specs: number;
  /** Poll results core has consumed from it since core started. */
  results_total: number;
  /** Current host CPU % (0–100) from its latest heartbeat; null when offline / on an N-1 build. */
  cpu_pct: number | null;
  /** Current host memory-used % (0–100); null when unavailable. */
  mem_used_pct: number | null;
  /** Highest watched-filesystem used % (0–100); null when unavailable. */
  disk_used_pct: number | null;
}

/** Per-pool summary in the `GET /api/v1/pollers` response: node count vs. live pollers, dispatch
 *  mode, and a warning when the pool has nodes but no live poller (they would go unmonitored). */
export interface PoolSummary {
  pool: string;
  /** Non-Meraki nodes assigned to this pool. */
  nodes: number;
  /** Live (online) pollers serving this pool. */
  live_pollers: number;
  /** `working_set` when a live poller serves it, else `legacy` (per-job fallback). */
  mode: 'working_set' | 'legacy';
  /** `nodes_without_live_poller` when the pool has nodes but no live poller, else null. */
  warning: 'nodes_without_live_poller' | null;
}

/** The `GET /api/v1/pollers` body: the registered poller fleet + the per-pool summary. */
export interface PollersResponse {
  pollers: PollerInfo[];
  pools: PoolSummary[];
}

/** One core↔poller visibility outage (`GET /api/v1/monitoring-gaps`, Phase 3 store-and-forward): a
 *  span during which core could not hear from the poller. If the poller stayed alive but partitioned,
 *  its local buffer backfills the metrics for the window on reconnect; alerts are not backfilled. */
export interface MonitoringGap {
  id: string;
  /** The poller whose visibility lapsed. */
  poller_id: string;
  /** Pool it serves. */
  pool: string;
  /** Start of the gap window (RFC 3339 — core's last contact before the outage). */
  started_at: string;
  /** End of the gap window (RFC 3339 — core heard from it again). */
  ended_at: string;
  /** Gap length in seconds. */
  duration_secs: number;
  /** When core recorded the gap (RFC 3339). */
  recorded_at: string;
}

/** Reachability of one Yagra backing dependency (no secrets — just a flag + label). */
export interface DependencyHealth {
  reachable: boolean;
  detail: string;
}

/** Yagra self-health: backing-service reachability (`GET /api/v1/system-health`). `bus` is an
 *  indirect signal inferred from poll-loop activity, not a direct NATS ping. */
export interface SystemHealth {
  overall: 'ok' | 'degraded';
  postgres: DependencyHealth;
  tsdb: DependencyHealth;
  /** VictoriaLogs event log store (ADR-024). Reported reachable when not configured (events then
   *  live in PostgreSQL), so an unconfigured log store never degrades `overall`. */
  logs: DependencyHealth;
  bus: DependencyHealth;
}

/** Running product version (`GET /api/v1/version`). `core` is the live `yagra-core` crate version
 *  (= the workspace version). Public — no secrets. The WebUI pairs this with its own build version. */
export interface VersionInfo {
  core: string;
}

// ── Host self-observability (Yagra monitoring its own core + pollers) ──────────

/** Usage of one watched filesystem (or a store-size proxy). `size_bytes === 0` ⇒ capacity unknown
 *  (a bare growing size, e.g. the PostgreSQL `database` proxy) — show used bytes, not a %. */
export interface DiskUsage {
  mount: string;
  used_bytes: number;
  size_bytes: number;
}

/** Current host resources for one self-monitored instance (`GET /api/v1/system/hosts`): core
 *  (`role: 'core'`) or a poller (`role: 'poller'`). Metrics are null until the first sample. */
export interface HostInfo {
  /** `core` or the poller id. */
  instance: string;
  role: 'core' | 'poller';
  /** Pool a poller serves; null for core. */
  pool: string | null;
  online: boolean;
  cpu_pct: number | null;
  load1: number | null;
  mem_used_bytes: number | null;
  mem_total_bytes: number | null;
  mem_used_pct: number | null;
  disk_used_pct: number | null;
  disks: DiskUsage[];
}

/** The `GET /api/v1/system/hosts` body: core plus every poller reporting host telemetry. */
export interface SystemHostsResponse {
  hosts: HostInfo[];
}

/** A per-mount filesystem trend within a `HostMetricRange`. Derive % from `used_bytes`/`size_bytes`
 *  aligned on timestamps, or show a bare-bytes trend when `size_bytes` is empty/zero. */
export interface HostDiskRange {
  mount: string;
  used_bytes: MetricPoint[];
  size_bytes: MetricPoint[];
}

/** Host CPU/load/mem/disk trends for one instance over a window
 *  (`GET /api/v1/system/hosts/:instance/metrics/range`). One round trip per instance/range. */
export interface HostMetricRange {
  instance: string;
  cpu_pct: MetricPoint[];
  load1: MetricPoint[];
  load5: MetricPoint[];
  load15: MetricPoint[];
  mem_used_bytes: MetricPoint[];
  mem_total_bytes: MetricPoint[];
  disks: HostDiskRange[];
}

/** Fleet data-coverage summary (`GET /api/v1/fleet/coverage`). */
export interface FleetCoverage {
  total: number;
  fresh: number;
  coverage_pct: number;
  stale: { node_id: string; name: string }[];
}

/** Fleet-wide status summary (`GET /api/v1/fleet/summary`): total node count + a per-state tally,
 *  computed server-side so the dashboard's status/health/down widgets are correct over the whole
 *  fleet (not a paged slice). Every state key is present and the counts sum to `total`. */
export interface FleetSummary {
  total: number;
  states: Record<NodeState, number>;
}

/** Per-group health rollup (`GET /api/v1/fleet/group-summary`): for each group id, the state tally
 *  of its **direct** member nodes, computed server-side over the whole fleet (not a paged slice) so
 *  the site-matrix / region-rollup / geo-map widgets stay correct past the first 100 nodes (A-1).
 *  Every state key is present; ungrouped nodes are omitted (no widget rolls them up), and the client
 *  sums descendants for the region rollup. */
export interface FleetGroupSummary {
  groups: Record<string, Record<NodeState, number>>;
}

/** One node in the dependency/topology graph (`GET /api/v1/topology`). */
export interface TopologyNode {
  id: string;
  name: string;
  parent_id: string | null;
  state: NodeState;
  /** Upstream node currently attributed as this node's root cause (dependency suppression). */
  root_cause: string | null;
}

/** The fixed error envelope (ADR-019). */
export interface ApiErrorBody {
  error: { code: string; message: string; details?: unknown };
}

// ── Troubleshoot analysis jobs (ADR-022) ─────────────────────────────────────

/** Which diagnostic a job runs (mirrors the Rust `AnalysisTool`). */
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

/** An analysis job row / SSE event (`/api/v1/analysis/jobs`). Timestamps are epoch-millis. */
export interface AnalysisJob {
  id: string;
  tool: AnalysisToolKey;
  scope_kind: 'all' | 'group' | 'node';
  scope_id: string | null;
  scope_label: string;
  params: Record<string, unknown>;
  state: 'running' | 'done' | 'failed' | 'cancelled';
  pct: number;
  phase: string | null;
  finding_count: number;
  summary: string | null;
  error: string | null;
  created_ms: number;
  started_ms: number | null;
  finished_ms: number | null;
}

/** One finding from an analysis (`GET /api/v1/analysis/jobs/:id/findings`). */
export interface AnalysisFinding {
  id: string;
  score: number;
  /** crit | warn | info (derived from score). */
  severity: 'crit' | 'warn' | 'info';
  node_id: string | null;
  node_name: string;
  metric: string;
  /** Anomaly shape / correlation / capacity / flap kind. */
  kind: string;
  when_label: string;
  duration: string;
  /** Tool-specific payload (anomaly chart series: points/mean/sigma/recent_from). */
  detail: AnomalyDetail | Record<string, unknown>;
}

/** Anomaly finding detail — the report chart's real series. */
export interface AnomalyDetail {
  points: { t: number; v: number; recent: boolean }[];
  mean: number;
  sigma: number;
  recent_from: number;
}

/** Request body to launch an analysis (`POST /api/v1/analysis/jobs`). */
export interface AnalysisJobInput {
  tool: AnalysisToolKey;
  scope_kind: 'all' | 'group' | 'node';
  scope_id?: string | null;
  scope_label: string;
  window_secs: number;
  baseline_secs?: number;
  sensitivity?: number;
  depth?: string;
  family?: string;
  notify?: boolean;
}

// ── Reports (Dashboard → Reports) ──
// Definitions are reusable templates (opaque `spec` the frontend owns); schedules fire them on a
// preset cadence; runs are saved generated reports. Shared resource: everyone reads, admins write.

/** One choice for a `select` section setting. */
export interface ReportSettingOption {
  value: string;
  label: string;
}

/** A configurable setting on a report section (drives the builder controls). */
export interface ReportSectionSetting {
  key: string;
  label: string;
  kind: 'number' | 'select';
  default: unknown;
  options?: ReportSettingOption[];
}

/** A report-section type from the catalog (`GET /api/v1/reports/sections`). */
export interface ReportSectionDef {
  kind: string;
  title: string;
  blurb: string;
  group: string;
  settings: ReportSectionSetting[];
}

/** One placed section instance in a report definition's spec. */
export interface ReportSectionInstance {
  id: string;
  kind: string;
  settings: Record<string, unknown>;
}

/** The report document (definition `spec`) — the frontend owns and migrates this shape. */
export interface ReportSpec {
  version: number;
  params: { range_secs: number };
  sections: ReportSectionInstance[];
}

/** A saved report definition (template). */
export interface ReportDefinition {
  id: string;
  name: string;
  description: string | null;
  spec: ReportSpec;
  updated_by: string | null;
  created_ms: number;
  updated_ms: number;
}

/** Create/update body for a report definition. */
export interface ReportDefinitionInput {
  name: string;
  description?: string;
  spec: ReportSpec;
}

/** Schedule cadence preset. */
export type ReportFrequency = 'daily' | 'weekly' | 'monthly';

/** A report schedule (joined with its definition's name). */
export interface ReportSchedule {
  id: string;
  definition_id: string;
  definition_name: string;
  frequency: ReportFrequency;
  day_of_week: number | null;
  day_of_month: number | null;
  at_hour: number;
  at_minute: number;
  enabled: boolean;
  next_run_ms: number;
  last_run_ms: number | null;
  last_status: string | null;
}

/** Create/update body for a report schedule. */
export interface ReportScheduleInput {
  definition_id: string;
  frequency: ReportFrequency;
  day_of_week?: number | null;
  day_of_month?: number | null;
  at_hour: number;
  at_minute: number;
  enabled?: boolean;
}

/** Lifecycle of a generated report run. */
export type ReportRunState = 'queued' | 'running' | 'succeeded' | 'failed';

/** A saved report run (the saved-reports list; no heavy payloads). */
export interface ReportRun {
  id: string;
  definition_id: string | null;
  name: string;
  trigger: 'manual' | 'scheduled';
  state: ReportRunState;
  pct: number;
  error: string | null;
  range_from_ms: number | null;
  range_to_ms: number | null;
  section_count: number;
  created_by: string | null;
  created_ms: number;
  started_ms: number | null;
  finished_ms: number | null;
}

/** A run plus its rendered result (the viewer). */
export interface ReportRunDetail extends ReportRun {
  result_json: unknown | null;
  result_html: string | null;
}

// ── Passive events (syslog / SNMP traps / webhooks) ──

/** What kind of passive event a source produces (yagra-bus `EventKind`). */
export type EventKind = 'syslog' | 'trap' | 'webhook';

/** A webhook ingest source (`GET /api/v1/event-sources`) — the bearer token is shown
 *  once on create/rotate and stored only as a hash. */
export interface EventSource {
  id: string;
  name: string;
  kind: EventKind;
  enabled: boolean;
  node_id: string | null;
  created_at: string;
}

/** An event match rule (`GET /api/v1/event-rules`). severity 'info' records the match but
 *  raises no alert. `ttl_secs` auto-closes; `clear_pattern` resolves early; `min_count`
 *  matches inside `window_secs` gate the fire. */
export interface EventRule {
  id: string;
  name: string;
  enabled: boolean;
  source_kind: EventKind | null;
  source_id: string | null;
  node_id: string | null;
  match_kind: 'substring' | 'regex';
  pattern: string;
  clear_pattern: string | null;
  severity: Severity;
  ttl_secs: number;
  min_count: number;
  window_secs: number;
  created_at: string;
}

/** Create/update body for an event rule (`POST/PUT /api/v1/event-rules`). */
export interface EventRuleInput {
  name: string;
  enabled: boolean;
  source_kind?: EventKind | null;
  source_id?: string | null;
  node_id?: string | null;
  match_kind: 'substring' | 'regex';
  pattern: string;
  clear_pattern?: string | null;
  severity: Severity;
  ttl_secs?: number;
  min_count?: number;
  window_secs?: number;
}

/** Result of the interactive rule tester (`POST /api/v1/event-rules/test`). */
export interface EventRuleTestResult {
  matched: boolean;
  clear_matched: boolean | null;
  error: string | null;
}

/** One received event (`GET /api/v1/events`). */
export interface EventRow {
  id: string;
  kind: EventKind;
  at_unix_ms: number;
  recorded_at: string; // keyset cursor (pass as `before`)
  source_ip: string | null;
  node_id: string | null;
  source_id: string | null;
  pool: string | null;
  facility: number | null;
  syslog_severity: number | null;
  hostname: string | null;
  app_name: string | null;
  trap_oid: string | null;
  /** Well-known MIB name for `trap_oid` (e.g. `linkDown`), resolved server-side; null for
   *  syslog/webhook events or an OID outside the curated set. */
  trap_name: string | null;
  varbinds: Array<[string, string]> | null;
  message: string;
  matched_rule_id: string | null;
  action: 'none' | 'fired' | 'refreshed' | 'cleared' | 'suppressed' | 'info';
}

/** The categorical event `action` outcomes (pipeline result). */
export type EventAction = 'none' | 'fired' | 'refreshed' | 'cleared' | 'suppressed' | 'info';

/** One categorical `/events/stats` bucket (mirrors the Rust `EventStatBucket`). `key` is stable for
 *  the list key + fallback display; `label` is a server-resolved display hint (e.g. a trap's MIB
 *  name); `node_id` is set for source grouping when the source maps to an inventory node (resolve
 *  its name via `useEntityNames`, per the no-raw-UUID rule). */
export interface EventStatBucket {
  key: string;
  label?: string | null;
  node_id?: string | null;
  count: number;
}

/** One time bucket for the `/events/stats?group_by=time` volume series (mirrors the Rust
 *  `EventTimeBucket`). `by_kind` is present only when `split=kind` was requested. */
export interface EventTimeBucket {
  ts_unix_ms: number;
  count: number;
  by_kind?: Record<string, number> | null;
}
