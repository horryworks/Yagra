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
}

/** A page of the keyset-paginated node list (`GET /api/v1/nodes`). */
export interface NodePage {
  nodes: NodeSummary[];
  next_cursor: string | null;
}

/** Current principal (`GET /api/v1/auth/me`). */
export interface AuthMe {
  role: string;
}

/** A predefined role (yagra-common `Role`, snake_case), ordered least → most privileged. */
export type Role = 'viewer' | 'operator' | 'admin';

/** A user account row (`GET /api/v1/users`, core `UserSummary`). Never includes the
 *  password hash. `created_at` is RFC 3339 text. */
export interface UserSummary {
  id: string;
  username: string;
  role: Role;
  created_at: string;
}

/** A device-class profile (`GET /api/v1/profiles`, repo `ProfileSummary`). */
export interface ProfileSummary {
  id: string;
  name: string;
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

/** One node's configuration detail incl. bindings (`GET /api/v1/nodes/:id`). */
export interface NodeDetail {
  id: string;
  name: string;
  address: string;
  profile_id: string | null;
  credential_id: string | null;
  parent_id: string | null;
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

/** The fixed error envelope (ADR-019). */
export interface ApiErrorBody {
  error: { code: string; message: string; details?: unknown };
}
