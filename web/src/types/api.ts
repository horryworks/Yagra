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

/** The fixed error envelope (ADR-019). */
export interface ApiErrorBody {
  error: { code: string; message: string; details?: unknown };
}
