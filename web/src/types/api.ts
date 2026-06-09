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

/** Credential metadata (never the secret value). */
export interface CredentialSummary {
  id: string;
  name: string;
  kind: string;
}

/** The fixed error envelope (ADR-019). */
export interface ApiErrorBody {
  error: { code: string; message: string; details?: unknown };
}
