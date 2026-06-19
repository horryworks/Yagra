// The single typed boundary to the Yagra-core northbound API (coding-conventions: never
// scatter raw fetch across components). All calls go through `api`; errors surface as a
// typed `ApiError` decoded from the fixed error envelope (ADR-019).

import type {
  Alert,
  AlertHistoryRow,
  AlertNodeCount,
  AlertTransition,
  AnalysisFinding,
  AnalysisJob,
  AnalysisJobInput,
  ApiErrorBody,
  CalendarBucket,
  AuditRow,
  AuthMe,
  ChannelConfigInput,
  ClassificationRule,
  ClassificationRuleInput,
  CollectionKind,
  CollectionTemplate,
  CredentialSummary,
  Direction,
  DiscoveryCandidate,
  DiscoveryScan,
  FleetCoverage,
  GroupType,
  InterfaceHeatmap,
  InterfaceRow,
  InterfaceTopEntry,
  InterfaceTopMetric,
  InterfaceSeries,
  MaintenanceWindow,
  MetricAgg,
  MetricKind,
  MetricRange,
  MetricReading,
  MetricTopAgg,
  MibCatalogEntry,
  Mute,
  NodeDetail,
  NodeGroup,
  NodePage,
  NodeStatus,
  NodeSummary,
  NotificationChannel,
  PollerHealth,
  ProfileSummary,
  ProfileInput,
  Role,
  RoleMatrix,
  RoutingRule,
  ScopeLevel,
  Severity,
  StateHistory,
  ThroughputRange,
  StoredCollectionItem,
  StoredThreshold,
  TopEntry,
  TopologyNode,
  UserSummary,
} from '../types/api';

/** Request body to create a collection item (scalar or table). */
export interface CollectionItemInput {
  metric_name: string;
  oid: string;
  collection: CollectionKind;
  metric_kind: MetricKind;
  enabled?: boolean;
}

const BASE = (import.meta.env.VITE_API_BASE as string | undefined) ?? '/api/v1';
const TOKEN_KEY = 'yagra_token';

let authToken: string | null =
  typeof localStorage !== 'undefined' ? localStorage.getItem(TOKEN_KEY) : null;

/** Set (or clear) the bearer token, persisting it across reloads. */
export function setToken(token: string | null): void {
  authToken = token;
  if (typeof localStorage === 'undefined') return;
  if (token) localStorage.setItem(TOKEN_KEY, token);
  else localStorage.removeItem(TOKEN_KEY);
}

/** The current bearer token, if logged in. */
export function getToken(): string | null {
  return authToken;
}

// Invoked when a request fails auth with a token already attached — i.e. the stored token
// has gone stale (most commonly: yagra-core restarted and dropped its in-memory sessions).
// The app registers a handler to flip auth state off and prompt a fresh sign-in, instead of
// surfacing a raw 401 from a write the user thought they were authorized for.
let onUnauthorized: (() => void) | null = null;
export function setUnauthorizedHandler(handler: (() => void) | null): void {
  onUnauthorized = handler;
}

// Drop a stale session and trigger the app's re-sign-in prompt. `request` handles its own 401s
// inline; the SSE client (services/sse.ts) handles its 401s out of band and calls this so both
// paths share one source of truth for "the stored token is no longer valid".
export function notifyAuthFailure(): void {
  if (!authToken) return;
  setToken(null);
  onUnauthorized?.();
}

/** A decoded API error carrying the stable machine-readable `code`. */
export class ApiError extends Error {
  constructor(
    public readonly code: string,
    message: string,
    public readonly status: number,
  ) {
    super(message);
    this.name = 'ApiError';
  }
}

async function request<T>(path: string, init?: RequestInit): Promise<T> {
  // Attach the bearer token when logged in; otherwise keep the single-arg call shape
  // for plain GETs (tests assert on it).
  let finalInit = init;
  if (authToken) {
    const headers = new Headers(init?.headers);
    headers.set('Authorization', `Bearer ${authToken}`);
    finalInit = { ...init, headers };
  }
  const res = finalInit ? await fetch(`${BASE}${path}`, finalInit) : await fetch(`${BASE}${path}`);
  if (!res.ok) {
    let code = 'http_error';
    let message = `request failed with status ${res.status}`;
    try {
      const body = (await res.json()) as ApiErrorBody;
      if (body?.error) {
        code = body.error.code;
        message = body.error.message;
      }
    } catch {
      // Non-JSON error body — keep the generic message.
    }
    // A 401 while a token was attached means the stored token is no longer valid; drop it
    // and let the app re-prompt for sign-in. (Bad-credentials at /auth/login never hits this:
    // no token is attached yet during login.)
    if (res.status === 401 && authToken) {
      setToken(null);
      onUnauthorized?.();
    }
    throw new ApiError(code, message, res.status);
  }
  // 204 No Content: only ever returned by endpoints typed `Promise<void>` (DELETEs, PUT
  // toggles), so the `undefined as T` is sound. A body-returning endpoint must not 204.
  if (res.status === 204) return undefined as T;
  return (await res.json()) as T;
}

/** JSON request body init for a mutating call. */
function jsonBody(method: string, body: unknown): RequestInit {
  return {
    method,
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify(body),
  };
}

/** Public client bootstrap config (no secrets). */
export interface ClientConfig {
  public_dashboard: boolean;
  auth_available: boolean;
  /** Global default polling interval (seconds); per-profile overrides take precedence. */
  default_poll_interval_secs: number;
}

export const api = {
  /** Public bootstrap config: whether reads are open, login availability, default poll interval. */
  getConfig: (): Promise<ClientConfig> => request('/config'),

  /** Update the global default polling interval (seconds). ManageConfig-gated. */
  updateConfig: (body: { default_poll_interval_secs: number }): Promise<void> =>
    request('/config', jsonBody('PUT', body)),

  /** Latest reading for one node metric. */
  getNodeMetric: (
    nodeId: string,
    metric: string,
    opts?: { agg?: MetricAgg },
  ): Promise<MetricReading> => {
    const path = `/nodes/${encodeURIComponent(nodeId)}/metrics/${encodeURIComponent(metric)}`;
    return request(opts?.agg ? `${path}?agg=${opts.agg}` : path);
  },

  /** Time-series window for one node metric (defaults: last hour, 60s step). */
  getNodeMetricRange: (
    nodeId: string,
    metric: string,
    opts?: { from?: number; to?: number; step?: number; agg?: MetricAgg },
  ): Promise<MetricRange> => {
    const params = new URLSearchParams();
    if (opts?.from != null) params.set('from', String(opts.from));
    if (opts?.to != null) params.set('to', String(opts.to));
    if (opts?.step != null) params.set('step', String(opts.step));
    if (opts?.agg) params.set('agg', opts.agg);
    const qs = params.toString();
    const path = `/nodes/${encodeURIComponent(nodeId)}/metrics/${encodeURIComponent(metric)}/range`;
    return request(qs ? `${path}?${qs}` : path);
  },

  /** Fleet-wide Top-N for a metric: the highest-value nodes now (`agg: 'now'`, default) or by
   *  trailing-hour peak (`agg: 'max_1h'`). Powers the dashboard Top RTT/CPU/… widgets. */
  getTopMetrics: (
    metric: string,
    opts?: { agg?: MetricTopAgg; limit?: number },
  ): Promise<TopEntry[]> => {
    const params = new URLSearchParams({ metric });
    if (opts?.agg) params.set('agg', opts.agg);
    if (opts?.limit != null) params.set('limit', String(opts.limit));
    return request(`/metrics/top?${params.toString()}`);
  },

  /** Fleet-wide interface Top-N (busiest links / most errors). `metric` selects the dimension. */
  getInterfaceTop: (
    metric: InterfaceTopMetric,
    opts?: { agg?: MetricTopAgg; limit?: number },
  ): Promise<InterfaceTopEntry[]> => {
    const params = new URLSearchParams({ metric });
    if (opts?.agg) params.set('agg', opts.agg);
    if (opts?.limit != null) params.set('limit', String(opts.limit));
    return request(`/metrics/interface-top?${params.toString()}`);
  },

  /** Interfaces whose throughput moved the most vs `window`s ago — `up` (spikes) / `down` (drops).
   *  `value` is the signed delta in bits/sec. */
  getInterfaceDelta: (
    direction: 'up' | 'down',
    opts?: { window?: number; limit?: number },
  ): Promise<InterfaceTopEntry[]> => {
    const params = new URLSearchParams({ direction });
    if (opts?.window != null) params.set('window', String(opts.window));
    if (opts?.limit != null) params.set('limit', String(opts.limit));
    return request(`/metrics/interface-delta?${params.toString()}`);
  },

  /** Inventory listing (first page; the response is keyset-paginated). */
  listNodes: (): Promise<NodeSummary[]> =>
    request<NodePage>('/nodes').then((r) => r.nodes),

  /** One keyset page of the inventory (for the virtualized node table). Pass the previous
   *  page's `next_cursor` to fetch the next page; `next_cursor: null` ⇒ last page. */
  listNodesPage: (opts?: { cursor?: string; limit?: number }): Promise<NodePage> => {
    const params = new URLSearchParams();
    if (opts?.cursor) params.set('cursor', opts.cursor);
    if (opts?.limit != null) params.set('limit', String(opts.limit));
    const qs = params.toString();
    return request(qs ? `/nodes?${qs}` : '/nodes');
  },

  /** Create a node. Optional profile/credential/parent bindings + descriptive maker/model. */
  createNode: (body: {
    name: string;
    address: string;
    pool?: string;
    profile_id?: string;
    credential_id?: string;
    parent_id?: string;
    vendor?: string;
    model?: string;
  }): Promise<{ id: string }> => request('/nodes', jsonBody('POST', body)),

  /** One node's live status: rolled-up display state + active alerts attributed to it. */
  getNodeStatus: (id: string): Promise<NodeStatus> =>
    request(`/nodes/${encodeURIComponent(id)}/status`),

  /** One node's config detail incl. bindings (profile/credential/parent). */
  getNode: (id: string): Promise<NodeDetail> => request(`/nodes/${encodeURIComponent(id)}`),

  /** Interfaces discovered on a node, with query-time utilization. Empty in skeleton mode. */
  listNodeInterfaces: (id: string): Promise<InterfaceRow[]> =>
    request(`/nodes/${encodeURIComponent(id)}/interfaces`),

  /** Per-interface throughput + error time-series for the detail charts (defaults: last hour). */
  getInterfaceSeries: (
    nodeId: string,
    ifindex: number,
    opts?: { from?: number; to?: number; step?: number },
  ): Promise<InterfaceSeries> => {
    const params = new URLSearchParams();
    if (opts?.from != null) params.set('from', String(opts.from));
    if (opts?.to != null) params.set('to', String(opts.to));
    if (opts?.step != null) params.set('step', String(opts.step));
    const qs = params.toString();
    const path = `/nodes/${encodeURIComponent(nodeId)}/interfaces/${ifindex}/series`;
    return request(qs ? `${path}?${qs}` : path);
  },

  /** Delete a node. */
  deleteNode: (id: string): Promise<void> =>
    request(`/nodes/${encodeURIComponent(id)}`, { method: 'DELETE' }),

  /** Trigger an immediate poll of a node (ICMP + its configured SNMP set), bypassing the
   *  scheduler interval. Returns how many jobs were dispatched; results arrive asynchronously
   *  on the normal path, so the caller refreshes its readings shortly after. */
  pollNode: (id: string): Promise<{ dispatched: number }> =>
    request(`/nodes/${encodeURIComponent(id)}/poll`, { method: 'POST' }),

  /** Set or clear a node's device-profile + bound credential and its maker/model. The node-edit
   *  UI loads the current values and resends them, so an unchanged field is preserved. */
  setNodeBindings: (
    id: string,
    body: {
      profile_id?: string | null;
      credential_id?: string | null;
      vendor?: string | null;
      model?: string | null;
    },
  ): Promise<void> =>
    request(`/nodes/${encodeURIComponent(id)}/bindings`, jsonBody('PUT', body)),

  /** Move a node into a group (or `null` to ungroup it), appending it to the end — used by the
   *  "Move to…" picker and a drop directly onto a group. */
  setNodeGroup: (id: string, groupId: string | null): Promise<void> =>
    request(`/nodes/${encodeURIComponent(id)}/group`, jsonBody('PUT', { group_id: groupId })),

  /** Drag-reorder a node: place it in `group_id` (`null` ⇒ ungrouped) next to a sibling node.
   *  `before`/`after` name the sibling (at most one; omit both to append). */
  placeNode: (
    id: string,
    body: { group_id: string | null; before?: string; after?: string },
  ): Promise<void> =>
    request(`/nodes/${encodeURIComponent(id)}/placement`, jsonBody('PUT', body)),

  /** The node groups (the inventory folder tree; flat list with parent links). */
  listNodeGroups: (): Promise<NodeGroup[]> => request('/node-groups'),

  // ── Troubleshoot analysis jobs (ADR-022) ──
  /** Recent analysis jobs (the runs list), newest first. */
  listAnalysisJobs: (limit?: number): Promise<AnalysisJob[]> =>
    request(limit != null ? `/analysis/jobs?limit=${limit}` : '/analysis/jobs'),

  /** One analysis job by id. */
  getAnalysisJob: (id: string): Promise<AnalysisJob> =>
    request(`/analysis/jobs/${encodeURIComponent(id)}`),

  /** A job's findings (the report list), highest score first. */
  getAnalysisFindings: (id: string): Promise<AnalysisFinding[]> =>
    request(`/analysis/jobs/${encodeURIComponent(id)}/findings`),

  /** Launch a background analysis job; the returned row progresses over SSE. */
  createAnalysisJob: (body: AnalysisJobInput): Promise<AnalysisJob> =>
    request('/analysis/jobs', jsonBody('POST', body)),

  /** Cancel a running analysis job. */
  cancelAnalysisJob: (id: string): Promise<{ cancelled: boolean }> =>
    request(`/analysis/jobs/${encodeURIComponent(id)}/cancel`, jsonBody('POST', {})),

  /** Create a node group. `parent_id` nests it under another group. */
  createNodeGroup: (body: {
    name: string;
    group_type: GroupType;
    parent_id?: string | null;
  }): Promise<{ id: string }> => request('/node-groups', jsonBody('POST', body)),

  /** Rename / re-type / re-parent (move) a node group. */
  updateNodeGroup: (
    id: string,
    body: { name: string; group_type: GroupType; parent_id?: string | null },
  ): Promise<void> =>
    request(`/node-groups/${encodeURIComponent(id)}`, jsonBody('PUT', body)),

  /** Drag-reorder a group: re-parent it under `parent_id` (`null` ⇒ top level) next to a sibling
   *  group. `before`/`after` name the sibling (at most one; omit both to append). Cycle-guarded. */
  placeNodeGroup: (
    id: string,
    body: { parent_id: string | null; before?: string; after?: string },
  ): Promise<void> =>
    request(`/node-groups/${encodeURIComponent(id)}/placement`, jsonBody('PUT', body)),

  /** Set (or clear, with both `null`) a group's geo coordinates for the dashboard map. */
  setNodeGroupGeo: (
    id: string,
    body: { latitude: number | null; longitude: number | null },
  ): Promise<void> =>
    request(`/node-groups/${encodeURIComponent(id)}/geo`, jsonBody('PUT', body)),

  /** Delete a node group. Its child groups + member nodes re-parent up; nodes are never deleted. */
  deleteNodeGroup: (id: string): Promise<void> =>
    request(`/node-groups/${encodeURIComponent(id)}`, { method: 'DELETE' }),

  /** Device-class profiles. */
  listProfiles: (): Promise<ProfileSummary[]> => request('/profiles'),

  /** Create a profile (name + optional category/vendor). */
  createProfile: (body: ProfileInput): Promise<{ id: string }> =>
    request('/profiles', jsonBody('POST', body)),

  /** Update a profile's name / category / vendor. */
  updateProfile: (id: string, body: ProfileInput): Promise<void> =>
    request(`/profiles/${encodeURIComponent(id)}`, jsonBody('PUT', body)),

  /** Delete a profile. */
  deleteProfile: (id: string): Promise<void> =>
    request(`/profiles/${encodeURIComponent(id)}`, { method: 'DELETE' }),

  /** Threshold rules (hierarchical overrides; most-specific scope wins). */
  listThresholds: (): Promise<StoredThreshold[]> => request('/thresholds'),

  /** Create a threshold rule. */
  createThreshold: (body: {
    scope_level: ScopeLevel;
    scope_id: string;
    metric: string;
    direction: Direction;
    warning?: number;
    critical?: number;
    dwell_samples?: number;
  }): Promise<{ id: string }> => request('/thresholds', jsonBody('POST', body)),

  /** Delete a threshold rule. */
  deleteThreshold: (id: string): Promise<void> =>
    request(`/thresholds/${encodeURIComponent(id)}`, { method: 'DELETE' }),

  /** A node's collection items. `resolved` returns the effective set (the profile's
   *  templates overridden by node-level items) rather than just the node-level overrides. */
  listNodeCollection: (id: string, resolved = false): Promise<StoredCollectionItem[]> => {
    const path = `/nodes/${encodeURIComponent(id)}/collection`;
    return request(resolved ? `${path}?resolved=true` : path);
  },

  /** Add (or update) a collection item on a node (overrides the profile/template default). */
  addNodeCollection: (id: string, body: CollectionItemInput): Promise<{ id: string }> =>
    request(`/nodes/${encodeURIComponent(id)}/collection`, jsonBody('POST', body)),

  /** Delete a node-scope collection item by id. */
  deleteCollectionItem: (itemId: string): Promise<void> =>
    request(`/collection/${encodeURIComponent(itemId)}`, { method: 'DELETE' }),

  /** Reusable collection templates (named metric bundles profiles attach). */
  listCollectionTemplates: (): Promise<CollectionTemplate[]> => request('/collection-templates'),

  /** Create a template. 409 `template_name_taken` if the name is in use. */
  createCollectionTemplate: (body: {
    name: string;
    description?: string;
  }): Promise<{ id: string }> => request('/collection-templates', jsonBody('POST', body)),

  /** Delete a template (also detaches it from every profile). */
  deleteCollectionTemplate: (id: string): Promise<void> =>
    request(`/collection-templates/${encodeURIComponent(id)}`, { method: 'DELETE' }),

  /** The curated OID catalog (MIB repository), optionally filtered by a substring. */
  listMibCatalog: (q?: string): Promise<MibCatalogEntry[]> =>
    request(q ? `/mib-catalog?q=${encodeURIComponent(q)}` : '/mib-catalog'),

  /** Add a catalog entry. 409 `metric_name_taken` if the name is in use. */
  createMibEntry: (body: {
    metric_name: string;
    oid: string;
    collection: CollectionKind;
    metric_kind: MetricKind;
    vendor?: string;
    description?: string;
  }): Promise<{ id: string }> => request('/mib-catalog', jsonBody('POST', body)),

  /** Delete a catalog entry. */
  deleteMibEntry: (id: string): Promise<void> =>
    request(`/mib-catalog/${encodeURIComponent(id)}`, { method: 'DELETE' }),

  /** Start a discovery sweep over explicit target IPs (the UI expands a CIDR). Stored
   *  credentials go by id (resolved server-side). The WebUI scans with stored credentials
   *  only; `communities` remains an optional ad-hoc extra for external automation. */
  startDiscoveryScan: (body: {
    targets: string[];
    communities?: string[];
    credential_ids: string[];
  }): Promise<{ scan_id: string }> => request('/discovery/scan', jsonBody('POST', body)),

  /** Poll a discovery scan's status + candidates. */
  getDiscoveryScan: (id: string): Promise<DiscoveryScan> =>
    request(`/discovery/scan/${encodeURIComponent(id)}`),

  /** Recent discovered (unclassified) devices across scans — the dashboard discovery queue. */
  getDiscoveryCandidates: (limit?: number): Promise<DiscoveryCandidate[]> =>
    request(limit != null ? `/discovery/candidates?limit=${limit}` : '/discovery/candidates'),

  /** Poll-loop self-monitoring (last sweep / jobs per round / results total). */
  getPollerHealth: (): Promise<PollerHealth> => request('/poller-health'),

  /** Import selected discovered devices as nodes. */
  importDiscovered: (
    nodes: {
      address: string;
      name: string;
      profile_id?: string;
      credential_id?: string;
      vendor?: string;
      model?: string;
    }[],
  ): Promise<{ created: number }> => request('/discovery/import', jsonBody('POST', { nodes })),

  /** Device-classification rules (discovery → suggested profile), ascending by priority. */
  listClassificationRules: (): Promise<ClassificationRule[]> => request('/classification-rules'),

  /** Create a classification rule. 400s on bad regex/prefix/profile. */
  createClassificationRule: (body: ClassificationRuleInput): Promise<{ id: string }> =>
    request('/classification-rules', jsonBody('POST', body)),

  /** Update a classification rule in place. */
  updateClassificationRule: (id: string, body: ClassificationRuleInput): Promise<void> =>
    request(`/classification-rules/${encodeURIComponent(id)}`, jsonBody('PUT', body)),

  /** Delete a classification rule. */
  deleteClassificationRule: (id: string): Promise<void> =>
    request(`/classification-rules/${encodeURIComponent(id)}`, { method: 'DELETE' }),

  /** The metrics in a template. */
  listTemplateItems: (id: string): Promise<StoredCollectionItem[]> =>
    request(`/collection-templates/${encodeURIComponent(id)}/items`),

  /** Add (or update) a metric in a template. */
  addTemplateItem: (id: string, body: CollectionItemInput): Promise<{ id: string }> =>
    request(`/collection-templates/${encodeURIComponent(id)}/items`, jsonBody('POST', body)),

  /** Delete a metric from a template. */
  deleteTemplateItem: (templateId: string, itemId: string): Promise<void> =>
    request(
      `/collection-templates/${encodeURIComponent(templateId)}/items/${encodeURIComponent(itemId)}`,
      { method: 'DELETE' },
    ),

  /** The templates a profile attaches. */
  listProfileTemplates: (id: string): Promise<CollectionTemplate[]> =>
    request(`/profiles/${encodeURIComponent(id)}/templates`),

  /** Replace the set of templates a profile attaches. */
  setProfileTemplates: (id: string, templateIds: string[]): Promise<void> =>
    request(`/profiles/${encodeURIComponent(id)}/templates`, jsonBody('PUT', {
      template_ids: templateIds,
    })),

  /** Credential metadata listing (never includes secret values). */
  listCredentials: (): Promise<CredentialSummary[]> => request('/credentials'),

  /** Store a new encrypted credential. */
  createCredential: (body: {
    name: string;
    kind: string;
    secret: string;
  }): Promise<{ id: string }> => request('/credentials', jsonBody('POST', body)),

  /** Update a credential. `name` is required; pass `secret` (with its `kind`) only to replace the
   *  stored secret — omit it to rename in place (the secret is never returned, so editing keeps
   *  the existing one unless you re-enter it). */
  updateCredential: (
    id: string,
    body: { name: string; kind?: string; secret?: string },
  ): Promise<void> =>
    request(`/credentials/${encodeURIComponent(id)}`, jsonBody('PUT', body)),

  /** Delete a credential. */
  deleteCredential: (id: string): Promise<void> =>
    request(`/credentials/${encodeURIComponent(id)}`, { method: 'DELETE' }),

  /** Active alerts. */
  listAlerts: (): Promise<Alert[]> => request('/alerts'),

  /** Recent alert history (default 100 rows). */
  listAlertHistory: (limit?: number): Promise<AlertHistoryRow[]> =>
    request(limit != null ? `/alerts/history?limit=${limit}` : '/alerts/history'),

  /** Nodes generating the most alert fires over a trailing window (chronic offenders). */
  getAlertTopNodes: (opts?: { window?: number; limit?: number }): Promise<AlertNodeCount[]> => {
    const params = new URLSearchParams();
    if (opts?.window != null) params.set('window', String(opts.window));
    if (opts?.limit != null) params.set('limit', String(opts.limit));
    const qs = params.toString();
    return request(qs ? `/alerts/top-nodes?${qs}` : '/alerts/top-nodes');
  },

  /** Alert fires bucketed weekday×hour over the last `days` (heatmap). */
  getAlertCalendar: (days?: number): Promise<CalendarBucket[]> =>
    request(days != null ? `/alerts/calendar?days=${days}` : '/alerts/calendar'),

  /** Recent up/down transitions (latest fires + recoveries). */
  getAlertTransitions: (limit?: number): Promise<AlertTransition[]> =>
    request(limit != null ? `/alerts/transitions?limit=${limit}` : '/alerts/transitions'),

  /** The dependency graph: nodes + parent edges + state + active root-cause attribution. */
  getTopology: (): Promise<{ nodes: TopologyNode[] }> => request('/topology'),

  /** Fleet data coverage + the stale-data watchlist (silent/blind-spot nodes). */
  getFleetCoverage: (): Promise<FleetCoverage> => request('/fleet/coverage'),

  /** Node-state counts over time (fleet health timeline; default last 24h). */
  getStateHistory: (opts?: { from?: number; to?: number }): Promise<StateHistory> => {
    const params = new URLSearchParams();
    if (opts?.from != null) params.set('from', String(opts.from));
    if (opts?.to != null) params.set('to', String(opts.to));
    const qs = params.toString();
    return request(qs ? `/fleet/state-history?${qs}` : '/fleet/state-history');
  },

  /** Fleet aggregate ingress/egress (bits/sec) over time (default last 24h). */
  getThroughputRange: (opts?: {
    from?: number;
    to?: number;
    step?: number;
  }): Promise<ThroughputRange> => {
    const params = new URLSearchParams();
    if (opts?.from != null) params.set('from', String(opts.from));
    if (opts?.to != null) params.set('to', String(opts.to));
    if (opts?.step != null) params.set('step', String(opts.step));
    const qs = params.toString();
    return request(qs ? `/metrics/throughput-range?${qs}` : '/metrics/throughput-range');
  },

  /** Busiest-links × time throughput heatmap (default top 8 over last 6h). */
  getInterfaceHeatmap: (opts?: {
    limit?: number;
    from?: number;
    to?: number;
    step?: number;
  }): Promise<InterfaceHeatmap> => {
    const params = new URLSearchParams();
    if (opts?.limit != null) params.set('limit', String(opts.limit));
    if (opts?.from != null) params.set('from', String(opts.from));
    if (opts?.to != null) params.set('to', String(opts.to));
    if (opts?.step != null) params.set('step', String(opts.step));
    const qs = params.toString();
    return request(qs ? `/metrics/interface-heatmap?${qs}` : '/metrics/interface-heatmap');
  },

  /** Notification channels (metadata only; the secret config is never returned). */
  listNotificationChannels: (): Promise<NotificationChannel[]> =>
    request('/notification-channels'),

  /** Create a notification channel (name + secret config, sealed server-side). */
  createNotificationChannel: (body: {
    name: string;
    config: ChannelConfigInput;
  }): Promise<{ id: string }> => request('/notification-channels', jsonBody('POST', body)),

  /** Enable/disable a channel. */
  setNotificationChannelEnabled: (id: string, enabled: boolean): Promise<void> =>
    request(`/notification-channels/${encodeURIComponent(id)}`, jsonBody('PUT', { enabled })),

  /** Delete a channel (and drop it from any rule). */
  deleteNotificationChannel: (id: string): Promise<void> =>
    request(`/notification-channels/${encodeURIComponent(id)}`, { method: 'DELETE' }),

  /** Routing rules (which alerts, by severity, fan out to which channels). */
  listRoutingRules: (): Promise<RoutingRule[]> => request('/routing-rules'),

  /** Create a routing rule. `severity` null ⇒ matches all severities. */
  createRoutingRule: (body: {
    name: string;
    severity: Severity | null;
    channel_ids: string[];
  }): Promise<{ id: string }> => request('/routing-rules', jsonBody('POST', body)),

  /** Enable/disable a rule. */
  setRoutingRuleEnabled: (id: string, enabled: boolean): Promise<void> =>
    request(`/routing-rules/${encodeURIComponent(id)}`, jsonBody('PUT', { enabled })),

  /** Delete a routing rule. */
  deleteRoutingRule: (id: string): Promise<void> =>
    request(`/routing-rules/${encodeURIComponent(id)}`, { method: 'DELETE' }),

  /** Maintenance windows (nodes covered by an active one are in `maintenance` state). */
  listMaintenanceWindows: (): Promise<MaintenanceWindow[]> => request('/maintenance-windows'),

  /** Create a maintenance window. Times are RFC 3339; scope mirrors thresholds. */
  createMaintenanceWindow: (body: {
    name: string;
    scope_level: ScopeLevel;
    scope_id: string;
    starts_at: string;
    ends_at: string;
  }): Promise<{ id: string }> => request('/maintenance-windows', jsonBody('POST', body)),

  /** Enable/disable a maintenance window. */
  setMaintenanceWindowEnabled: (id: string, enabled: boolean): Promise<void> =>
    request(`/maintenance-windows/${encodeURIComponent(id)}`, jsonBody('PUT', { enabled })),

  /** Delete a maintenance window. */
  deleteMaintenanceWindow: (id: string): Promise<void> =>
    request(`/maintenance-windows/${encodeURIComponent(id)}`, { method: 'DELETE' }),

  /** Unexpired mutes (notification silences; alerts still show in the UI/history). */
  listMutes: (): Promise<Mute[]> => request('/mutes'),

  /** Create a mute. `metric_name` omitted ⇒ the whole node; `until` is RFC 3339. */
  createMute: (body: {
    node_id: string;
    metric_name?: string;
    until: string;
    reason?: string;
  }): Promise<{ id: string }> => request('/mutes', jsonBody('POST', body)),

  /** Delete (lift) a mute. */
  deleteMute: (id: string): Promise<void> =>
    request(`/mutes/${encodeURIComponent(id)}`, { method: 'DELETE' }),

  /** The role-vs-privilege matrix: which permissions each role grants. Read-only (View). */
  listRoles: (): Promise<RoleMatrix> => request('/roles'),

  /** User accounts (metadata only; never the password hash). Requires admin (ManageUsers). */
  listUsers: (): Promise<UserSummary[]> => request('/users'),

  /** Create a user account. The password is hashed server-side and never returned. */
  createUser: (body: {
    username: string;
    password: string;
    role: Role;
  }): Promise<{ id: string }> => request('/users', jsonBody('POST', body)),

  /** Delete a user account. Refused (409 `last_admin`) for the last admin. */
  deleteUser: (id: string): Promise<void> =>
    request(`/users/${encodeURIComponent(id)}`, { method: 'DELETE' }),

  /** Change a user's role. Refused (409 `last_admin`) when demoting the last admin. */
  setUserRole: (id: string, role: Role): Promise<void> =>
    request(`/users/${encodeURIComponent(id)}/role`, jsonBody('PUT', { role })),

  /** Enable or disable a user account. Refused (409 `last_admin`) when disabling the last
   *  admin that can still log in. A disabled account is kept for the audit trail. */
  setUserEnabled: (id: string, enabled: boolean): Promise<void> =>
    request(`/users/${encodeURIComponent(id)}/enabled`, jsonBody('PUT', { enabled })),

  /** Reset a user's password (hashed server-side; never echoed back). */
  setUserPassword: (id: string, password: string): Promise<void> =>
    request(`/users/${encodeURIComponent(id)}/password`, jsonBody('PUT', { password })),

  /** Audit log page, newest first (admin-only). `before` is the keyset cursor: pass the
   *  last row's `at` to fetch the next (older) page. */
  listAudit: (opts?: { limit?: number; before?: string }): Promise<AuditRow[]> => {
    const params = new URLSearchParams();
    if (opts?.limit != null) params.set('limit', String(opts.limit));
    if (opts?.before) params.set('before', opts.before);
    const qs = params.toString();
    return request(qs ? `/audit?${qs}` : '/audit');
  },

  /** The caller's saved My Dashboard layout, or `null` if none saved yet (the client then uses
   *  its default). The body is opaque JSON — the client owns and migrates the widget shape
   *  (types in `dashboard/types`); the server stores it verbatim, scoped to the logged-in user. */
  getDashboard: (): Promise<unknown> => request('/dashboard'),

  /** Save (replace) the caller's My Dashboard layout. */
  putDashboard: (layout: unknown): Promise<{ ok: boolean }> =>
    request('/dashboard', jsonBody('PUT', layout)),

  /** The current principal (role). Requires a valid session. */
  me: (): Promise<AuthMe> => request('/auth/me'),

  /** Log in; stores the bearer token on success. */
  login: async (username: string, password: string): Promise<{ token: string; role: string }> => {
    const res = await request<{ token: string; role: string }>(
      '/auth/login',
      jsonBody('POST', { username, password }),
    );
    setToken(res.token);
    return res;
  },

  /** Forget the stored token. */
  logout: (): void => setToken(null),
};
