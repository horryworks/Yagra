// The single typed boundary to the Yagra-core northbound API (coding-conventions: never
// scatter raw fetch across components). All calls go through `api`; errors surface as a
// typed `ApiError` decoded from the fixed error envelope (ADR-019).

import type {
  Alert,
  AlertHistoryRow,
  ApiErrorBody,
  AuthMe,
  ChannelConfigInput,
  CollectionKind,
  CollectionTemplate,
  CredentialSummary,
  Direction,
  DiscoveryScan,
  InterfaceRow,
  InterfaceSeries,
  MaintenanceWindow,
  MetricAgg,
  MetricKind,
  MetricRange,
  MetricReading,
  MibCatalogEntry,
  Mute,
  NodeDetail,
  NodePage,
  NodeStatus,
  NodeSummary,
  NotificationChannel,
  ProfileSummary,
  Role,
  RoutingRule,
  ScopeLevel,
  Severity,
  StoredCollectionItem,
  StoredThreshold,
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
}

export const api = {
  /** Public bootstrap config: whether reads are open and login is available. */
  getConfig: (): Promise<ClientConfig> => request('/config'),

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

  /** Create a node. Optional profile/credential/parent bindings. */
  createNode: (body: {
    name: string;
    address: string;
    pool?: string;
    profile_id?: string;
    credential_id?: string;
    parent_id?: string;
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

  /** Set or clear a node's device-profile + bound credential. */
  setNodeBindings: (
    id: string,
    body: { profile_id?: string | null; credential_id?: string | null },
  ): Promise<void> =>
    request(`/nodes/${encodeURIComponent(id)}/bindings`, jsonBody('PUT', body)),

  /** Device-class profiles. */
  listProfiles: (): Promise<ProfileSummary[]> => request('/profiles'),

  /** Create a profile. */
  createProfile: (name: string): Promise<{ id: string }> =>
    request('/profiles', jsonBody('POST', { name })),

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

  /** Start a discovery sweep over explicit target IPs (the UI expands a CIDR). */
  startDiscoveryScan: (body: {
    targets: string[];
    communities: string[];
  }): Promise<{ scan_id: string }> => request('/discovery/scan', jsonBody('POST', body)),

  /** Poll a discovery scan's status + candidates. */
  getDiscoveryScan: (id: string): Promise<DiscoveryScan> =>
    request(`/discovery/scan/${encodeURIComponent(id)}`),

  /** Import selected discovered devices as nodes. */
  importDiscovered: (
    nodes: { address: string; name: string; profile_id?: string; credential_id?: string }[],
  ): Promise<{ created: number }> => request('/discovery/import', jsonBody('POST', { nodes })),

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

  /** Delete a credential. */
  deleteCredential: (id: string): Promise<void> =>
    request(`/credentials/${encodeURIComponent(id)}`, { method: 'DELETE' }),

  /** Active alerts. */
  listAlerts: (): Promise<Alert[]> => request('/alerts'),

  /** Recent alert history (default 100 rows). */
  listAlertHistory: (limit?: number): Promise<AlertHistoryRow[]> =>
    request(limit != null ? `/alerts/history?limit=${limit}` : '/alerts/history'),

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

  /** Create a mute. `check` omitted ⇒ the whole node; `until` is RFC 3339. */
  createMute: (body: {
    node_id: string;
    check?: string;
    until: string;
    reason?: string;
  }): Promise<{ id: string }> => request('/mutes', jsonBody('POST', body)),

  /** Delete (lift) a mute. */
  deleteMute: (id: string): Promise<void> =>
    request(`/mutes/${encodeURIComponent(id)}`, { method: 'DELETE' }),

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

  /** Reset a user's password (hashed server-side; never echoed back). */
  setUserPassword: (id: string, password: string): Promise<void> =>
    request(`/users/${encodeURIComponent(id)}/password`, jsonBody('PUT', { password })),

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
