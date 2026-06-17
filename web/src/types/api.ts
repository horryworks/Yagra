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

/** Address family. */
export type AddressFamily = 'v4' | 'v6';

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

/** An alert as produced by the engine (`yagra_alert::Alert`). */
export interface Alert {
  node: string;
  check: string;
  severity: Severity;
  state: NodeState;
  at_unix_ms: number;
  root_cause: string | null;
  flapping: boolean;
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
}

/** A device-class profile (`GET /api/v1/profiles`, repo `ProfileSummary`). Split by functional
 *  `category` (role) × `vendor`-NOS family; `category` is the kebab-case `ProfileCategory` token. */
export interface ProfileSummary {
  id: string;
  name: string;
  category: string;
  vendor: string | null;
}

/** Create/update-profile request body (`POST`/`PUT /api/v1/profiles`). */
export interface ProfileInput {
  name: string;
  category?: string;
  vendor?: string | null;
}

/** Threshold scope level (yagra-common `ScopeLevel`, snake_case). Most-specific wins. */
export type ScopeLevel = 'profile' | 'group' | 'node';

/** Breach direction (yagra-common `Direction`, snake_case). */
export type Direction = 'above' | 'below';

/** A stored threshold rule (`GET /api/v1/thresholds`, core `StoredThreshold`). The rule
 *  fields are flattened onto the row. Note: the GET shape names the scope `level`, while the
 *  POST body names it `scope_level`. */
export interface StoredThreshold {
  id: string;
  level: ScopeLevel;
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
export type ChannelKind = 'webhook' | 'email';

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
    };

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
  level: ScopeLevel;
  scope_id: string;
  starts_at: string;
  ends_at: string;
  enabled: boolean;
  active: boolean;
}

/** A mute (`GET /api/v1/mutes`, core `StoredMute`): notifications for one node (optionally
 *  one check) are silenced until `until_at` — the alert still shows in the UI/history. */
export interface Mute {
  id: string;
  node_id: string;
  check_name: string | null;
  until_at: string;
  reason: string | null;
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

/** Fleet data-coverage summary (`GET /api/v1/fleet/coverage`). */
export interface FleetCoverage {
  total: number;
  fresh: number;
  coverage_pct: number;
  stale: { node_id: string; name: string }[];
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
