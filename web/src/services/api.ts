// SPDX-License-Identifier: AGPL-3.0-only
// The single typed boundary to the Yagra-core northbound API (coding-conventions: never
// scatter raw fetch across components). All calls go through `api`; errors surface as a
// typed `ApiError` decoded from the fixed error envelope (ADR-019).
//
// Every method addresses the API by its **contract path** — the template key from the generated
// `api/schema.d.ts` — so the URL, the method, the path/query parameters and the request body are
// all checked against what the Rust handlers actually serve (ADR-035). A renamed endpoint or a
// dropped parameter is now a compile error here instead of a 404 in front of an operator.

import type {
  Alert,
  BusRemoteAccepted,
  BusStatus,
  AlertHistoryQuery,
  AlertHistoryRow,
  RankedAlertNodes,
  AlertTransition,
  AnalysisFinding,
  AnalysisJob,
  AnalysisJobInput,
  AnalysisSchedule,
  AnalysisScheduleInput,
  ApiErrorBody,
  CalendarBucket,
  AuditQuery,
  AuditRow,
  AuthMe,
  ChannelConfigInput,
  ClassificationRule,
  ClassificationRuleInput,
  CollectionKind,
  CollectionTemplate,
  ConfigBundle,
  CredentialSummary,
  Direction,
  DiscoveryCandidate,
  DiscoveryScan,
  EventRow,
  EventRule,
  EventRuleInput,
  EventRuleTestResult,
  EventSource,
  EventStatBucket,
  EventTimeBucket,
  FleetCoverage,
  FleetGroupSummary,
  FleetSummary,
  FlowAsAgg,
  FlowConversation,
  FlowFilters,
  FlowPoint,
  FlowPortAgg,
  FlowProtoAgg,
  FlowTalker,
  GroupNodesResult,
  GroupType,
  ImportReport,
  InterfaceHeatmap,
  InterfaceRow,
  RankedInterfaces,
  InterfaceTopMetric,
  InterfaceSeries,
  MaintenanceScopeLevel,
  MaintenanceWindow,
  MerakiCandidate,
  MerakiEnumeration,
  MerakiNetwork,
  MerakiOrg,
  MerakiOrgOption,
  MetricAgg,
  MetricKind,
  MetricRange,
  MetricReading,
  NodeMetricEntry,
  MetricTopAgg,
  MibCatalogEntry,
  Mute,
  NodeDetail,
  NodeGroup,
  NodeNameEntry,
  NodePage,
  NodeSearchResult,
  NodeStatus,
  MonitoringGap,
  NodeAssignment,
  NotificationChannel,
  PollerHealth,
  PollerNodesResponse,
  PollersResponse,
  PoolsResponse,
  SystemHealth,
  SystemHostsResponse,
  HostMetricRange,
  VersionInfo,
  UpgradeStatus,
  UpgradeRunAccepted,
  ProfileSummary,
  ProfileInput,
  ReportDefinition,
  ReportDefinitionInput,
  ReportRun,
  ReportRunDetail,
  ReportSchedule,
  ReportScheduleInput,
  ReportSectionDef,
  Role,
  RoleMatrix,
  RoutingRule,
  ChannelKind,
  NotifyEvent,
  TemplatePreview,
  TemplateVariable,
  SavedFinding,
  SavedFindingsQuery,
  Scope,
  ScopeLevel,
  Severity,
  StateHistory,
  SuppressionExemption,
  ThroughputRange,
  StoredCollectionItem,
  ThresholdPage,
  ThresholdQuery,
  RankedNodes,
  TopologyNode,
  TopologyMode,
  TopologyShadow,
  LinkOverrideRow,
  LinkOverrideAction,
  LinkDirection,
  TopologyLink,
  TopologyLinkSummary,
  UrlCheckConfig,
  DnsCheckConfig,
  CurrentNeighbors,
  DnsChainCurrent,
  DnsChainHistoryPage,
  NeighborConfig,
  NeighborHistoryPage,
  DiscoveredEndpointPage,
  ImportResult,
  DnsRecordType,
  UserKind,
  UserSummary,
  LdapConfigInput,
  LdapConfigView,
  WebTlsStatus,
  LdapTestResult,
  OidcProviderSummary,
  OidcProviderInput,
  ApiTokenSummary,
  ApiTokenInput,
  CreatedApiToken,
  ForwardDestination,
  ForwardDestinationInput,
  ForwardStatus,
  ForwardTestResult,
  LlmConfigInput,
  LlmConfigResponse,
  LlmTestResult,
  RcaReport,
  RcaRequestInput,
  RetentionPolicy,
  RetentionValues,
  CredentialHealth,
} from '../types/api';
import type { DiscoverySettingsBody } from '../pages/neighborSettings';
import { filenameFromDisposition } from '../lib/download';
import { buildUrl, type Op, type Ok, type OptsArg, type PathsWith } from './typedPaths';

/** Request body to create a collection item (scalar or table). */
export interface CollectionItemInput {
  metric_name: string;
  oid: string;
  collection: CollectionKind;
  metric_kind: MetricKind;
  enabled?: boolean;
}

// Origin prefix, empty by default. The contract paths already carry `/api/v1`, so this exists only
// to point a dev build at a core on another host (`VITE_API_BASE=https://core.example.net`) — it is
// an origin, never a path fragment.
const BASE = (import.meta.env.VITE_API_BASE as string | undefined) ?? '';
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

/** The server's message for a failed call, or a localized fallback for anything else (network
 *  drop, aborted fetch, a thrown non-Error). Every `.catch` that surfaces text to the operator
 *  goes through this, so an unexpected throw can never leak a raw stack string into the UI.
 *  It lives beside {@link ApiError} because that is the only type it inspects — it used to be
 *  copy-pasted, byte-identical, into 29 components and pages. */
export function errMsg(e: unknown, fallback: string): string {
  return e instanceof ApiError ? e.message : fallback;
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
  // …but 204 is not the only body-less success. A 202 may or may not carry one — `POST
  // /system/upgrade/apply` answers with the accepted run, `POST /system/upgrade/check` answers
  // with nothing at all — so the status cannot decide it and neither can `T`, which is erased.
  // Ask the response instead. Getting this wrong is not a type error and not a server error: the
  // request succeeds, the work happens, and `res.json()` throws `Unexpected end of JSON input`
  // over an empty body, so the caller reports a failure that did not occur. That is exactly how
  // the check button first behaved — the updater re-read the registry every time it was pressed
  // while the page said it could not ask.
  // `headers` is optional-chained because the fetch fakes in `api.test.ts` supply only `status`
  // and `json`; a fake without headers keeps the parsing path it has always taken.
  if (res.headers?.get('content-length') === '0') return undefined as T;
  return (await res.json()) as T;
}

/** Pick the arm of a query-dependent response union.
 *
 *  Three endpoints answer with a different shape depending on a query parameter — `?resolved=` on
 *  the node collection set, `?group_by=` on the event summary. OpenAPI keys a response schema off
 *  the status code only, so the contract can say no more than "one of these"; which one is settled
 *  by the argument the wrapper just passed. Stating that here keeps it beside the query that
 *  determines it, instead of pushing a union onto every caller that cannot narrow it either. */
function arm<T>(p: Promise<unknown>): Promise<T> {
  return p as Promise<T>;
}

/** A file download: the bytes plus the name the server gave them.
 *
 *  Two endpoints stream an attachment rather than JSON, so they cannot go through `apiGet` — and
 *  neither can be a plain anchor `href`, because an anchor carries no `Authorization` header and
 *  would 401 on any auth-enabled deployment. This is the one implementation of that; the report
 *  export grew its own first and the support bundle would have been the second copy of the same
 *  22 lines, differing in a URL (extensibility §3). */
async function fetchBlob(
  url: string,
  fallbackCode: string,
  init?: RequestInit,
): Promise<Download> {
  // `init` exists for one caller and the reason is worth stating: issuing a poller token is a POST
  // whose *response* is the archive, because the token exists only at that instant (ADR-065 Inc.4).
  // It is not a download of a resource that could be fetched again.
  const headers: Record<string, string> = { ...(init?.headers as Record<string, string>) };
  if (authToken) headers.Authorization = `Bearer ${authToken}`;
  const res = await fetch(`${BASE}${url}`, { ...init, headers });
  if (!res.ok) {
    let code = fallbackCode;
    let message = `download failed with status ${res.status}`;
    try {
      const body = (await res.json()) as ApiErrorBody;
      if (body?.error) {
        code = body.error.code;
        message = body.error.message;
      }
    } catch {
      // Non-JSON error body — keep the generic message.
    }
    throw new ApiError(code, message, res.status);
  }
  return {
    blob: await res.blob(),
    filename: filenameFromDisposition(res.headers.get('content-disposition')),
  };
}


/** Send a file as a raw request body, reporting progress as it goes.
 *
 *  XHR rather than `fetch` for one reason: no browser reports *upload* progress through `fetch`,
 *  and this is the only call in the app where the body is large enough for that to matter — a
 *  multi-gigabyte image archive over a slow link, with nothing on screen, is indistinguishable
 *  from a hung page. Error decoding matches {@link request} so the same ADR-019 envelope surfaces
 *  as the same {@link ApiError}. */
function uploadRaw<T>(
  url: string,
  file: Blob,
  onProgress?: (fraction: number) => void,
): Promise<T> {
  return new Promise<T>((resolve, reject) => {
    const xhr = new XMLHttpRequest();
    xhr.open('POST', `${BASE}${url}`);
    if (authToken) xhr.setRequestHeader('Authorization', `Bearer ${authToken}`);
    xhr.setRequestHeader('content-type', 'application/octet-stream');
    xhr.upload.onprogress = (e) => {
      if (e.lengthComputable && e.total > 0) onProgress?.(e.loaded / e.total);
    };
    xhr.onerror = () => reject(new ApiError('network_error', 'the upload could not be sent', 0));
    xhr.onabort = () => reject(new ApiError('aborted', 'the upload was cancelled', 0));
    xhr.onload = () => {
      if (xhr.status >= 200 && xhr.status < 300) {
        resolve((xhr.responseText ? JSON.parse(xhr.responseText) : undefined) as T);
        return;
      }
      let code = 'http_error';
      let message = `request failed with status ${xhr.status}`;
      try {
        const body = JSON.parse(xhr.responseText) as ApiErrorBody;
        if (body?.error) {
          code = body.error.code;
          message = body.error.message;
        }
      } catch {
        // Non-JSON error body — keep the generic message.
      }
      if (xhr.status === 401 && authToken) notifyAuthFailure();
      reject(new ApiError(code, message, xhr.status));
    };
    xhr.send(file);
  });
}

/** A downloaded attachment. `filename` is `null` when the server sent no usable one. */
export interface Download {
  blob: Blob;
  filename: string | null;
}

/** JSON request body init for a mutating call. */
function jsonBody(method: string, body: unknown): RequestInit {
  return {
    method,
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify(body),
  };
}

// The four contract-checked entry points. `path` is the template key from `api/schema.d.ts`, and
// everything else — which parameters exist, which are required, the body shape, the success type —
// is derived from it. The `as never` is the one place the generic plumbing needs help: TypeScript
// cannot see through the unresolved conditional types to `buildUrl`'s plain record.

function apiGet<P extends PathsWith<'get'>>(
  path: P,
  ...args: OptsArg<Op<P, 'get'>>
): Promise<Ok<Op<P, 'get'>>> {
  return request(buildUrl(path, args[0] as never));
}

function apiPost<P extends PathsWith<'post'>>(
  path: P,
  ...args: OptsArg<Op<P, 'post'>>
): Promise<Ok<Op<P, 'post'>>> {
  const opts = args[0] as { body?: unknown } | undefined;
  return request(buildUrl(path, args[0] as never), jsonBody('POST', opts?.body ?? {}));
}

function apiPut<P extends PathsWith<'put'>>(
  path: P,
  ...args: OptsArg<Op<P, 'put'>>
): Promise<Ok<Op<P, 'put'>>> {
  const opts = args[0] as { body?: unknown } | undefined;
  return request(buildUrl(path, args[0] as never), jsonBody('PUT', opts?.body));
}

// No DELETE in the contract declares a request body, so this never sends one.
function apiDelete<P extends PathsWith<'delete'>>(
  path: P,
  ...args: OptsArg<Op<P, 'delete'>>
): Promise<Ok<Op<P, 'delete'>>> {
  return request(buildUrl(path, args[0] as never), { method: 'DELETE' });
}

/** The from/to(/limit) + optional-filter query shared by the flow endpoints (ADR-031).
 *  Filters (proto/port/peer) and `dir` are sent only when set; core parses/validates them. */
function flowQuery(
  opts: {
    from: number;
    to: number;
    limit?: number;
    dir?: 'src' | 'dst';
  } & FlowFilters,
) {
  return {
    from: opts.from,
    to: opts.to,
    limit: opts.limit ?? undefined,
    proto: opts.proto != null ? String(opts.proto) : undefined,
    port: opts.port != null ? String(opts.port) : undefined,
    peer: opts.peer || undefined,
    asn: opts.asn != null ? String(opts.asn) : undefined,
    dir: opts.dir || undefined,
  };
}

/** Every filter dimension the event log and its aggregates share (no paging cursor).
 *
 *  One interface for both because they must stay the same set: a facet count that ignored a filter
 *  the list applies would put numbers beside the list that disagree with it. The Rust side asserts
 *  the same thing from its end (`every_query_surface_offers_the_same_event_filter_dimensions`).
 *  Blank fields are dropped; core parses and validates them. */
export interface EventStatsFilter {
  start?: string;
  end?: string;
  /** One kind, or several comma-joined. Commas rather than repeated parameters because `buildUrl`
   *  uses `params.set` and every token is a closed-set `[a-z_]` word, so there is nothing to
   *  collide (ADR-053). */
  kind?: string;
  /** Rule outcomes, comma-joined. */
  action?: string;
  /** Syslog severities (0–7), comma-joined. */
  severity?: string;
  node_id?: string;
  matched?: boolean;
  q?: string;
  regex?: boolean;
  /** Message-only condition, with the same word rules as `q`. */
  msg?: string;
  msg_regex?: boolean;
  msg_not?: boolean;
  /** Source condition: the event's IP or the attributed node's name. No regex form — see the API
   *  docs for why the node-name half makes one mean two different things per deployment. */
  src?: string;
  src_not?: boolean;
}

function eventStatsQuery(f: EventStatsFilter) {
  return {
    start: f.start || undefined,
    end: f.end || undefined,
    kind: f.kind || undefined,
    action: f.action || undefined,
    severity: f.severity || undefined,
    node_id: f.node_id || undefined,
    matched: f.matched ?? undefined,
    q: f.q || undefined,
    regex: f.regex ? true : undefined,
    msg: f.msg || undefined,
    msg_regex: f.msg_regex ? true : undefined,
    msg_not: f.msg_not ? true : undefined,
    src: f.src || undefined,
    src_not: f.src_not ? true : undefined,
  };
}

/** Public client bootstrap config (no secrets). */
export interface ClientConfig {
  public_dashboard: boolean;
  auth_available: boolean;
  /** Whether an OIDC provider is enabled — drives the "Continue with SSO" button. */
  sso_enabled: boolean;
  /** Global default polling interval (seconds); per-profile overrides take precedence. */
  default_poll_interval_secs: number;
  /** Whether an LLM provider is configured and enabled (ADR-029) — gates the "Explain this
   *  incident" affordance so it is never offered on an installation that would 503. Optional so an
   *  older core (which omits the field) reads as off rather than as `undefined`. */
  rca_enabled?: boolean;
  /** Whether this deployment has a traffic-flow store (ADR-031) — decides which analyses may be
   *  *scheduled*, since the backend refuses a scheduled flow analysis with the tier off. Optional
   *  for the same reason as `rca_enabled`: an older core omits it and that must read as off. */
  flow_enabled?: boolean;
}

export const api = {
  /** Public bootstrap config: whether reads are open, login availability, default poll interval. */
  getConfig: (): Promise<ClientConfig> => apiGet('/api/v1/config'),

  /** Update the global default polling interval (seconds). ManageConfig-gated. */
  updateConfig: (body: { default_poll_interval_secs: number }): Promise<void> =>
    apiPut('/api/v1/config', { body }),

  /** The data-retention policy: the editable windows plus every row of the table (ADR-040). */
  getRetention: (): Promise<RetentionPolicy> => apiGet('/api/v1/settings/retention'),

  /** Update the retention windows. ManageConfig-gated; applies without a restart. */
  updateRetention: (body: RetentionValues): Promise<void> =>
    apiPut('/api/v1/settings/retention', { body }),

  /** Whether every stored credential can still be decrypted with the loaded KEK (ADR-040). */
  getCredentialHealth: (): Promise<CredentialHealth> => apiGet('/api/v1/credentials/health'),

  /** A diagnostic archive of this deployment's logs and status (ADR-045), for a site nobody can
   *  reach a shell on. Needs ManageSystem + ManageCredentials + ViewAudit — i.e. Admin. The server
   *  names the file and refuses the whole export if its redaction scan matches, so a failure here
   *  is worth showing verbatim.
   *
   *  `nodeId` adds the `node/` section: one node's inventory row and owning poller, its stored
   *  interface rows, what it is configured to collect and which of those are arriving, and its
   *  alerts. Omitting it is not an error — the rest of the bundle is identical either way. */
  downloadSupportBundle: (sinceHours?: number, nodeId?: string | null): Promise<Download> => {
    const q = new URLSearchParams();
    if (sinceHours) q.set('since_hours', String(sinceHours));
    if (nodeId) q.set('node_id', nodeId);
    const qs = q.toString();
    return fetchBlob(
      `/api/v1/system/support-bundle${qs ? `?${qs}` : ''}`,
      'support_bundle_failed',
    );
  },

  /** This deployment's monitoring configuration as a portable bundle (ADR-040). Admin-only, and it
   *  carries no secrets. */
  exportConfigBundle: (): Promise<ConfigBundle> => apiGet('/api/v1/config/bundle'),

  /** Apply a bundle. Upsert only — nothing is deleted. With `dryRun` the whole import runs inside a
   *  transaction that is rolled back, so the report describes exactly what committing would do. */
  importConfigBundle: (bundle: ConfigBundle, opts?: { dryRun?: boolean }): Promise<ImportReport> =>
    apiPost('/api/v1/config/bundle', {
      query: { dry_run: opts?.dryRun },
      body: bundle,
    }),

  /** Latest reading for one node metric. */
  getNodeMetric: (
    nodeId: string,
    metric: string,
    opts?: { agg?: MetricAgg },
  ): Promise<MetricReading> =>
    apiGet('/api/v1/nodes/{node_id}/metrics/{metric}', {
      path: { node_id: nodeId, metric },
      query: { agg: opts?.agg },
    }),

  /** Time-series window for one node metric (defaults: last hour, 60s step).
   *
   *  `rate: true` returns the per-second rate of a counter instead of its stored values — the only
   *  honest way to chart one (ADR-012). The server refuses `rate` together with `agg`. */
  getNodeMetricRange: (
    nodeId: string,
    metric: string,
    opts?: { from?: number; to?: number; step?: number; agg?: MetricAgg; rate?: boolean },
  ): Promise<MetricRange> =>
    apiGet('/api/v1/nodes/{node_id}/metrics/{metric}/range', {
      path: { node_id: nodeId, metric },
      query: {
        from: opts?.from,
        to: opts?.to,
        step: opts?.step,
        agg: opts?.agg,
        rate: opts?.rate,
      },
    }),

  /** Every metric this node collects or has data for, with the status and dimension of each.
   *
   *  Read permission, unlike `listNodeCollection` — this returns names, kinds and status, never an
   *  OID. It is the only source that sees metrics with no collection item at all (reachability, the
   *  URL/DNS monitors, the neighbour count, extracted JSON values). */
  listNodeMetrics: (nodeId: string, opts?: { withinSecs?: number }): Promise<NodeMetricEntry[]> =>
    apiGet('/api/v1/nodes/{node_id}/metrics', {
      path: { node_id: nodeId },
      query: { within_secs: opts?.withinSecs },
    }),

  /** Fleet-wide Top-N for a metric: the highest-value nodes now (`agg: 'now'`, default) or by
   *  trailing-hour peak (`agg: 'max_1h'`). Powers the dashboard Top RTT/CPU/… widgets. */
  getTopMetrics: (
    metric: string,
    opts?: { agg?: MetricTopAgg; limit?: number },
  ): Promise<RankedNodes> =>
    apiGet('/api/v1/metrics/top', {
      query: { metric, agg: opts?.agg, limit: opts?.limit },
    }),

  /** Fleet-wide interface Top-N (busiest links / most errors). `metric` selects the dimension. */
  getInterfaceTop: (
    metric: InterfaceTopMetric,
    opts?: { agg?: MetricTopAgg; limit?: number },
  ): Promise<RankedInterfaces> =>
    apiGet('/api/v1/metrics/interface-top', {
      query: { metric, agg: opts?.agg, limit: opts?.limit },
    }),

  /** Interfaces whose throughput moved the most vs `window`s ago — `up` (spikes) / `down` (drops).
   *  `value` is the signed delta in bits/sec. */
  getInterfaceDelta: (
    direction: 'up' | 'down',
    opts?: { window?: number; limit?: number },
  ): Promise<RankedInterfaces> =>
    apiGet('/api/v1/metrics/interface-delta', {
      query: { direction, window: opts?.window, limit: opts?.limit },
    }),

  /** Resolve a batch of node ids → display names across the whole fleet (not just the first list
   *  page). Backs the shared `useEntityNames` resolver so a reference to any node — not only the
   *  first 100 — shows its name instead of a raw UUID (S12). Unresolved ids are omitted. */
  getNodeNames: (ids: string[]): Promise<NodeNameEntry[]> =>
    ids.length === 0 ? Promise.resolve([]) : apiPost('/api/v1/node-names', { body: { ids } }),

  /** Server-side node search for the node-picker typeahead (A-2): match name or address by
   *  case-insensitive substring, capped, so the picker never loads the whole inventory into the
   *  browser. Empty `q` returns the first page ordered by name. */
  searchNodes: (q: string, limit = 50): Promise<NodeSearchResult[]> =>
    apiGet('/api/v1/nodes/search', { query: { q: q || undefined, limit } }),

  /** A group's direct member nodes for the inventory tree's per-group lazy load (A-3). Pass a group
   *  id, or omit `group` for the ungrouped bucket. Fetched only when a group is expanded, so the
   *  tree never pulls the whole fleet up front. */
  getGroupNodes: (group: string | null): Promise<GroupNodesResult> =>
    apiGet('/api/v1/nodes/by-group', { query: { group: group || undefined } }),

  /** One keyset page of the inventory (for the virtualized node table). Pass the previous
   *  page's `next_cursor` to fetch the next page; `next_cursor: null` ⇒ last page. A non-empty
   *  `search` switches to server-side name/address search — a single capped page (no cursor) of
   *  full node summaries — so the Nodes tree's filter never full-loads the fleet client-side. */
  /** One page of the inventory. Any of `search` / `state` / `kind` / `pool` puts the endpoint into
   *  filter mode: a single capped page, no cursor, and `truncated` when matches were left out.
   *
   *  ⚠️ `state` / `kind` / `pool` are **comma-joined sets** since ADR-053 Inc.6, so they are typed
   *  `string` rather than the member unions — the value `ok,warning` is not a `NodeState`. The edge
   *  still rejects an unknown state or kind token (400), so this is looser typing here, not a looser
   *  contract there. */
  listNodesPage: (opts?: {
    cursor?: string;
    limit?: number;
    search?: string;
    state?: string;
    kind?: string;
    pool?: string;
  }): Promise<NodePage> =>
    apiGet('/api/v1/nodes', {
      query: {
        cursor: opts?.cursor || undefined,
        limit: opts?.limit,
        search: opts?.search || undefined,
        state: opts?.state || undefined,
        kind: opts?.kind || undefined,
        pool: opts?.pool || undefined,
      },
    }),

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
  }): Promise<{ id: string }> => apiPost('/api/v1/nodes', { body }),

  /** Create a URL monitor in one call: a node bound to the built-in URL/HTTP profile plus its
   *  URL-check config. Only `url`+`name` are required; the rest default server-side. */
  createUrlMonitor: (body: {
    name: string;
    parent_id?: string;
    pool?: string;
    url: string;
    method?: 'GET' | 'HEAD' | 'POST';
    expected_status?: UrlCheckConfig['expected_status'];
    verify_tls?: boolean;
    follow_redirects?: boolean;
    timeout_ms?: number;
  }): Promise<{ id: string }> => apiPost('/api/v1/url-monitors', { body }),

  /** A node's URL-monitor config, or `null` if it isn't a URL monitor (404 → null). */
  getUrlCheck: async (id: string): Promise<UrlCheckConfig | null> => {
    try {
      return await apiGet('/api/v1/nodes/{node_id}/url-check', { path: { node_id: id } });
    } catch (e) {
      if (e instanceof ApiError && e.status === 404) return null;
      throw e;
    }
  },

  /** Create or replace a node's URL-monitor config (the node must already exist). */
  setUrlCheck: (id: string, body: UrlCheckConfig): Promise<void> =>
    apiPut('/api/v1/nodes/{node_id}/url-check', { path: { node_id: id }, body }),

  /** Remove a node's URL-monitor config (the node itself is untouched). */
  deleteUrlCheck: (id: string): Promise<void> =>
    apiDelete('/api/v1/nodes/{node_id}/url-check', { path: { node_id: id } }),

  // ── DNS name-resolution monitoring (ADR-033) ──────────────────────────────────────
  /** Create a DNS monitor in one call: a node bound to the built-in DNS profile plus its
   *  DNS-check config. Only `name` (node) + `name` (DNS) are required; the rest default. */
  createDnsMonitor: (body: {
    /** Display name for the node. */
    name: string;
    /** The DNS name to resolve (separate from the node's label). */
    dns_name: string;
    parent_id?: string;
    pool?: string;
    record_type?: DnsRecordType;
    resolver?: string;
    resolver_port?: number;
    max_depth?: number;
    timeout_ms?: number;
  }): Promise<{ id: string }> => apiPost('/api/v1/dns-monitors', { body }),

  /** A node's DNS-monitor config, or `null` if it isn't a DNS monitor (404 → null). */
  getDnsCheck: async (id: string): Promise<DnsCheckConfig | null> => {
    try {
      return await apiGet('/api/v1/nodes/{node_id}/dns-check', { path: { node_id: id } });
    } catch (e) {
      if (e instanceof ApiError && e.status === 404) return null;
      throw e;
    }
  },

  /** Create or replace a node's DNS-monitor config (the node must already exist). */
  setDnsCheck: (id: string, body: DnsCheckConfig): Promise<void> =>
    apiPut('/api/v1/nodes/{node_id}/dns-check', { path: { node_id: id }, body }),

  /** Remove a node's DNS-monitor config (the node and its recorded chains are untouched). */
  deleteDnsCheck: (id: string): Promise<void> =>
    apiDelete('/api/v1/nodes/{node_id}/dns-check', { path: { node_id: id } }),

  /** The node's current resolution chain, or `null` if nothing has been observed yet. */
  getDnsChain: async (id: string): Promise<DnsChainCurrent | null> => {
    try {
      return await apiGet('/api/v1/nodes/{node_id}/dns-chain', { path: { node_id: id } });
    } catch (e) {
      if (e instanceof ApiError && e.status === 404) return null;
      throw e;
    }
  },

  /** A keyset page of the node's resolution-change history, newest first. */
  listDnsChainHistory: (
    id: string,
    opts: { limit?: number; beforeAt?: string; beforeId?: number } = {},
  ): Promise<DnsChainHistoryPage> => {
    // Both cursor halves or neither — the server rejects a half-specified cursor.
    const cursor = opts.beforeAt != null && opts.beforeId != null;
    return apiGet('/api/v1/nodes/{node_id}/dns-chain/history', {
      path: { node_id: id },
      query: {
        limit: opts.limit,
        before_at: cursor ? opts.beforeAt : undefined,
        before_id: cursor ? opts.beforeId : undefined,
      },
    });
  },

  // ── CDP/LLDP adjacency (ADR-038) ───────────────────────────────────────────────────
  /** The node's current neighbours, or `null` if no walk has recorded anything yet.
   *
   *  `null` is not the same as an empty set: the empty set means the device was walked and reports
   *  no neighbours, which is a real answer worth showing. */
  getNeighbors: async (id: string): Promise<CurrentNeighbors | null> => {
    try {
      return await apiGet('/api/v1/nodes/{node_id}/neighbors', { path: { node_id: id } });
    } catch (e) {
      if (e instanceof ApiError && e.status === 404) return null;
      throw e;
    }
  },

  /** A keyset page of the node's adjacency-change history, newest first. */
  listNeighborHistory: (
    id: string,
    opts: { limit?: number; beforeAt?: string; beforeId?: number } = {},
  ): Promise<NeighborHistoryPage> => {
    // Both cursor halves or neither — the server rejects a half-specified cursor.
    const cursor = opts.beforeAt != null && opts.beforeId != null;
    return apiGet('/api/v1/nodes/{node_id}/neighbors/history', {
      path: { node_id: id },
      query: {
        limit: opts.limit,
        before_at: cursor ? opts.beforeAt : undefined,
        before_id: cursor ? opts.beforeId : undefined,
      },
    });
  },

  /** Whether this deployment collects adjacency, how often, and the accepted cadence range. */
  getNeighborSettings: (): Promise<NeighborConfig> => apiGet('/api/v1/settings/neighbors'),

  /** Change whether and how often the discovery walks run (applies from the next sweep).
   *
   *  Every walk's pair is sent, not just the edited one: the server reads an absent field as "leave
   *  it", which is what keeps an older client from switching a walk it has never heard of off — and
   *  what would make this client's saves half-apply if it sent a subset. */
  setNeighborSettings: (body: DiscoverySettingsBody): Promise<void> =>
    apiPut('/api/v1/settings/neighbors', { body }),

  // ── Endpoints seen on the network (ADR-043 Increment 3) ───────────────────────────
  /** Addresses monitored routers have resolved on the wire that Yagra does **not** monitor.
   *
   *  Empty unless ARP discovery is enabled (Settings ▸ System settings ▸ Discovery walks). Check
   *  `summary.truncated_nodes`: above zero, at least one router's cache exceeded its row budget and
   *  the list is a sample rather than the whole segment. */
  listDiscoveredEndpoints: (
    opts: {
      limit?: number;
      viaNode?: string;
      includePromoted?: boolean;
      beforeLastSeen?: string;
      beforeId?: string;
    } = {},
  ): Promise<DiscoveredEndpointPage> => {
    // Both halves of the cursor or neither — the server refuses a half-specified one rather than
    // ignoring it, so sending one half would be a 400 rather than a silent restart from page one.
    const cursor = opts.beforeLastSeen && opts.beforeId;
    return apiGet('/api/v1/discovered-endpoints', {
      query: {
        limit: opts.limit,
        via_node: opts.viaNode,
        include_promoted: opts.includePromoted,
        before_last_seen: cursor ? opts.beforeLastSeen : undefined,
        before_id: cursor ? opts.beforeId : undefined,
      },
    });
  },

  /** Promote a discovered endpoint to a monitored node. `409` ⇒ the address became a node already. */
  importDiscoveredEndpoint: (
    id: string,
    body: { name?: string; profile_id?: string | null; credential_id?: string | null },
  ): Promise<ImportResult> =>
    apiPost('/api/v1/discovered-endpoints/{id}/import', { path: { id }, body }),

  // ── Cisco Meraki (read-only Dashboard API monitoring) ──────────────────────────────
  /** List the orgs an API key can access (nothing is persisted). Read-only. */
  merakiDiscover: (body: { api_key: string; base_url?: string }): Promise<MerakiOrgOption[]> =>
    apiPost('/api/v1/meraki/orgs/discover', { body }),

  /** The configured Meraki organizations. */
  listMerakiOrgs: (): Promise<MerakiOrg[]> => apiGet('/api/v1/meraki/orgs'),

  /** Onboard one or more orgs under a shared read-only key. */
  createMerakiOrgs: (body: {
    api_key: string;
    base_url?: string;
    org_ids: string[];
  }): Promise<{ created: number }> => apiPost('/api/v1/meraki/orgs', { body }),

  /** Delete an org (removes its device nodes, config, and groups). */
  deleteMerakiOrg: (id: string): Promise<void> =>
    apiDelete('/api/v1/meraki/orgs/{id}', { path: { id } }),

  /** Enable/disable an org (pause collection without losing config). */
  setMerakiOrgEnabled: (id: string, enabled: boolean): Promise<void> =>
    apiPut('/api/v1/meraki/orgs/{id}/enabled', { path: { id }, body: { enabled } }),

  /** Update an org's per-tier cadence, enabled tiers, and rate budget. */
  setMerakiOrgCadence: (
    id: string,
    body: {
      availability_secs: number;
      uplink_secs: number;
      traffic_secs: number;
      inventory_secs: number;
      enabled_tiers: string[];
      target_rps: number;
    },
  ): Promise<void> => apiPut('/api/v1/meraki/orgs/{id}/cadence', { path: { id }, body }),

  /** The org's networks with their monitored (watch/skip) flag. */
  listMerakiNetworks: (id: string): Promise<MerakiNetwork[]> =>
    apiGet('/api/v1/meraki/orgs/{id}/networks', { path: { id } }),

  /** Set the monitored flag for a set of the org's networks. */
  setMerakiNetworksMonitored: (
    id: string,
    network_ids: string[],
    monitored: boolean,
  ): Promise<void> =>
    apiPut('/api/v1/meraki/orgs/{id}/networks', { path: { id }, body: { network_ids, monitored } }),

  /** Enumerate an org's networks + device candidates from the Dashboard API (read-only). */
  enumerateMerakiOrg: (id: string): Promise<MerakiEnumeration> =>
    apiPost('/api/v1/meraki/orgs/{id}/enumerate', { path: { id } }),

  /** Import selected devices as nodes (atomic), setting the chosen networks in scope. */
  importMerakiDevices: (body: {
    org_uuid: string;
    monitored_network_ids: string[];
    devices: MerakiCandidate[];
  }): Promise<{ imported: number }> => apiPost('/api/v1/meraki/import', { body }),

  /** Read the global Meraki polling kill switch. */
  getMerakiPolling: (): Promise<{ enabled: boolean }> => apiGet('/api/v1/meraki/polling'),

  /** Set the global Meraki polling kill switch (safeguard: instantly halt all Meraki polling). */
  setMerakiPolling: (enabled: boolean): Promise<void> =>
    apiPut('/api/v1/meraki/polling', { body: { enabled } }),

  /** One node's live status: rolled-up display state + active alerts attributed to it. */
  getNodeStatus: (id: string): Promise<NodeStatus> =>
    apiGet('/api/v1/nodes/{node_id}/status', { path: { node_id: id } }),

  /** One node's config detail incl. bindings (profile/credential/parent). */
  getNode: (id: string): Promise<NodeDetail> =>
    apiGet('/api/v1/nodes/{node_id}', { path: { node_id: id } }),

  /** Interfaces discovered on a node, with query-time utilization. Empty in skeleton mode. */
  listNodeInterfaces: (id: string): Promise<InterfaceRow[]> =>
    apiGet('/api/v1/nodes/{node_id}/interfaces', { path: { node_id: id } }),

  /** Per-interface throughput + error time-series for the detail charts (defaults: last hour). */
  getInterfaceSeries: (
    nodeId: string,
    ifindex: number,
    opts?: { from?: number; to?: number; step?: number },
  ): Promise<InterfaceSeries> =>
    apiGet('/api/v1/nodes/{node_id}/interfaces/{ifindex}/series', {
      path: { node_id: nodeId, ifindex },
      query: { from: opts?.from, to: opts?.to, step: opts?.step },
    }),

  /** Delete a node. */
  deleteNode: (id: string): Promise<void> =>
    apiDelete('/api/v1/nodes/{node_id}', { path: { node_id: id } }),

  /** Trigger an immediate poll of a node (ICMP + its configured SNMP set), bypassing the
   *  scheduler interval. Returns how many jobs were dispatched and the pool they went to;
   *  results arrive asynchronously on the normal path, so the caller refreshes its readings
   *  shortly after. `pool` is the node's *effective* pool, which may be inherited from its
   *  folder rather than set on the node itself. */
  pollNode: (id: string): Promise<{ dispatched: number; node_id: string; pool: string }> =>
    apiPost('/api/v1/nodes/{node_id}/poll', { path: { node_id: id } }),

  /** Set or clear a node's device-profile + bound credential and its maker/model. The node-edit
   *  UI loads the current values and resends them, so an unchanged field is preserved.
   *
   *  `pool` is deliberately `string | undefined`, NOT `string | null`: server-side it is
   *  three-state — **omitted** leaves the pool unchanged, `''` clears it (inherit from the folder,
   *  else the default pool), any other value moves the node. A JSON `null` deserializes to
   *  "unchanged" and would silently do nothing, so it is not offered here. */
  setNodeBindings: (
    id: string,
    body: {
      profile_id?: string | null;
      credential_id?: string | null;
      vendor?: string | null;
      model?: string | null;
      pool?: string;
    },
  ): Promise<void> => apiPut('/api/v1/nodes/{node_id}/bindings', { path: { node_id: id }, body }),

  /** Which pool a node effectively belongs to (own > folder > default) and which poller currently
   *  polls it. Separate from `getNode` because it reads the live coordinator, not the inventory. */
  getNodeAssignment: (id: string): Promise<NodeAssignment> =>
    apiGet('/api/v1/nodes/{node_id}/assignment', { path: { node_id: id } }),

  /** The pools that exist, for the assignment picker. View-gated; names only.
   *  Separate from `listPollers` (which scans the whole node table to build its per-pool counts). */
  listPools: (): Promise<PoolsResponse> => apiGet('/api/v1/pools'),

  /** Move a node to a poll-pool. `''` clears it back to inherited (folder, else default pool).
   *
   *  Single-field on purpose — do NOT reach for `setNodeBindings({ pool })`: that endpoint
   *  overwrites profile/credential/vendor/model unconditionally and would blank all four. */
  setNodePool: (id: string, pool: string): Promise<void> =>
    apiPut('/api/v1/nodes/{node_id}/pool', { path: { node_id: id }, body: { pool } }),

  /** Move a folder to a poll-pool (`''` ⇒ inherit). Every node beneath it without a pool of its
   *  own follows on the next sweep. */
  setNodeGroupPool: (id: string, pool: string): Promise<void> =>
    apiPut('/api/v1/node-groups/{id}/pool', { path: { id }, body: { pool } }),

  /** Move a node into a group (or `null` to ungroup it), appending it to the end — used by the
   *  "Move to…" picker and a drop directly onto a group. */
  setNodeGroup: (id: string, groupId: string | null): Promise<void> =>
    apiPut('/api/v1/nodes/{node_id}/group', {
      path: { node_id: id },
      body: { group_id: groupId },
    }),

  /** Set (or clear with `null`) a node's dependency parent (upstream) — the alert-suppression
   *  edge (parent down ⇒ suppress children, ADR-015). Distinct from `setNodeGroup` (the folder
   *  tree). The server rejects self-dependencies and cycles. */
  setNodeParent: (id: string, parentId: string | null): Promise<void> =>
    apiPut('/api/v1/nodes/{node_id}/parent', {
      path: { node_id: id },
      body: { parent_id: parentId },
    }),

  /** Exclude a node from derived alert suppression, or put it back. Only ever removes suppression,
   *  so it cannot cause an outage to go unreported. No effect while the deployment uses the
   *  hand-authored graph. */
  setNodeSuppressionOptOut: (id: string, optOut: boolean): Promise<void> =>
    apiPut('/api/v1/nodes/{node_id}/suppression-opt-out', {
      path: { node_id: id },
      body: { opt_out: optOut },
    }),

  /** Drag-reorder a node: place it in `group_id` (`null` ⇒ ungrouped) next to a sibling node.
   *  `before`/`after` name the sibling (at most one; omit both to append). */
  placeNode: (
    id: string,
    body: { group_id: string | null; before?: string; after?: string },
  ): Promise<void> => apiPut('/api/v1/nodes/{node_id}/placement', { path: { node_id: id }, body }),

  /** The node groups (the inventory folder tree; flat list with parent links). */
  listNodeGroups: (): Promise<NodeGroup[]> => apiGet('/api/v1/node-groups'),

  // ── Troubleshoot analysis jobs (ADR-022) ──
  /** Recent analysis jobs (the runs list), newest first. */
  listAnalysisJobs: (limit?: number): Promise<AnalysisJob[]> =>
    apiGet('/api/v1/analysis/jobs', { query: { limit } }),

  /** One analysis job by id. */
  getAnalysisJob: (id: string): Promise<AnalysisJob> =>
    apiGet('/api/v1/analysis/jobs/{id}', { path: { id } }),

  /** A job's findings (the report list), highest score first. */
  getAnalysisFindings: (id: string): Promise<AnalysisFinding[]> =>
    apiGet('/api/v1/analysis/jobs/{id}/findings', { path: { id } }),

  /** Findings across every run (the All-findings screen), newest first. Keyset-paged: pass the last row's
   *  `at`/`id` back as `before`/`before_id`. */
  searchFindings: (query: SavedFindingsQuery): Promise<SavedFinding[]> =>
    apiGet('/api/v1/analysis/findings', { query }),

  /** Launch a background analysis job; the returned row progresses over SSE. */
  createAnalysisJob: (body: AnalysisJobInput): Promise<AnalysisJob> =>
    apiPost('/api/v1/analysis/jobs', { body }),

  /** Cancel a running analysis job. */
  cancelAnalysisJob: (id: string): Promise<{ cancelled: boolean }> =>
    apiPost('/api/v1/analysis/jobs/{id}/cancel', { path: { id } }),

  /** Recurring analyses, soonest first. */
  listAnalysisSchedules: (): Promise<AnalysisSchedule[]> => apiGet('/api/v1/analysis/schedules'),

  /** Create a recurring analysis. */
  createAnalysisSchedule: (body: AnalysisScheduleInput): Promise<{ id: string }> =>
    apiPost('/api/v1/analysis/schedules', { body }),

  /** Replace a recurring analysis; `next_run_at` is recomputed from the new cadence. */
  updateAnalysisSchedule: (id: string, body: AnalysisScheduleInput): Promise<void> =>
    apiPut('/api/v1/analysis/schedules/{id}', { path: { id }, body }),

  /** Delete a recurring analysis. */
  deleteAnalysisSchedule: (id: string): Promise<void> =>
    apiDelete('/api/v1/analysis/schedules/{id}', { path: { id } }),

  // ── Reports (Dashboard → Reports) ──
  /** The report-section catalog (drives the builder). */
  listReportSections: (): Promise<ReportSectionDef[]> => apiGet('/api/v1/reports/sections'),

  /** All report definitions (templates). */
  listReportDefinitions: (): Promise<ReportDefinition[]> => apiGet('/api/v1/reports/definitions'),

  /** Create a report definition (admin only). */
  createReportDefinition: (body: ReportDefinitionInput): Promise<ReportDefinition> =>
    apiPost('/api/v1/reports/definitions', { body }),

  /** Update a report definition (admin only). */
  updateReportDefinition: (id: string, body: ReportDefinitionInput): Promise<{ ok: boolean }> =>
    apiPut('/api/v1/reports/definitions/{id}', { path: { id }, body }),

  /** Delete a report definition (admin only). */
  deleteReportDefinition: (id: string): Promise<void> =>
    apiDelete('/api/v1/reports/definitions/{id}', { path: { id } }),

  /** Generate a report from a definition now (admin only); the run progresses over SSE. */
  runReport: (id: string): Promise<ReportRun> =>
    apiPost('/api/v1/reports/definitions/{id}/run', { path: { id } }),

  /** Saved report runs, newest first. */
  listReportRuns: (limit?: number): Promise<ReportRun[]> =>
    apiGet('/api/v1/reports/runs', { query: { limit } }),

  /** One report run with its rendered result (the viewer). */
  getReportRun: (id: string): Promise<ReportRunDetail> =>
    apiGet('/api/v1/reports/runs/{id}', { path: { id } }),

  /** Delete a saved report run (admin only). */
  deleteReportRun: (id: string): Promise<void> =>
    apiDelete('/api/v1/reports/runs/{id}', { path: { id } }),

  /** Download a report run as html|csv|pdf. Fetches with the bearer token (so it works on an
   *  auth-enabled deployment, unlike a plain anchor href) and returns a Blob to save. */
  exportReportRun: async (id: string, format: 'html' | 'csv' | 'pdf'): Promise<Blob> =>
    (
      await fetchBlob(
        `/api/v1/reports/runs/${encodeURIComponent(id)}/export?format=${format}`,
        'export_failed',
      )
    ).blob,

  /** All report schedules. */
  listReportSchedules: (): Promise<ReportSchedule[]> => apiGet('/api/v1/reports/schedules'),

  /** Create a report schedule (admin only). */
  createReportSchedule: (body: ReportScheduleInput): Promise<{ id: string }> =>
    apiPost('/api/v1/reports/schedules', { body }),

  /** Update a report schedule (admin only). */
  updateReportSchedule: (id: string, body: ReportScheduleInput): Promise<{ ok: boolean }> =>
    apiPut('/api/v1/reports/schedules/{id}', { path: { id }, body }),

  /** Delete a report schedule (admin only). */
  deleteReportSchedule: (id: string): Promise<void> =>
    apiDelete('/api/v1/reports/schedules/{id}', { path: { id } }),

  /** Create a node group. `parent_id` nests it under another group; `pool` assigns a poll-pool that
   *  its nodes inherit (omit or `''` ⇒ inherit from an ancestor, else the default pool). */
  createNodeGroup: (body: {
    name: string;
    group_type: GroupType;
    parent_id?: string | null;
    pool?: string;
  }): Promise<{ id: string }> => apiPost('/api/v1/node-groups', { body }),

  /** Rename / re-type / re-parent (move) a node group, and optionally move its poll-pool. `pool`
   *  has the same three-state contract as `setNodeBindings`: omitted = unchanged, `''` = inherit. */
  updateNodeGroup: (
    id: string,
    body: { name: string; group_type: GroupType; parent_id?: string | null; pool?: string },
  ): Promise<void> => apiPut('/api/v1/node-groups/{id}', { path: { id }, body }),

  /** Drag-reorder a group: re-parent it under `parent_id` (`null` ⇒ top level) next to a sibling
   *  group. `before`/`after` name the sibling (at most one; omit both to append). Cycle-guarded. */
  placeNodeGroup: (
    id: string,
    body: { parent_id: string | null; before?: string; after?: string },
  ): Promise<void> => apiPut('/api/v1/node-groups/{id}/placement', { path: { id }, body }),

  /** Set or clear a folder's map pin — the coordinates the dashboard's Geo map widget places.
   *  Both fields or neither; `null`/`null` clears it. Validated server-side as well. */
  setNodeGroupGeo: (
    id: string,
    body: { latitude: number | null; longitude: number | null },
  ): Promise<void> => apiPut('/api/v1/node-groups/{id}/geo', { path: { id }, body }),

  /** Delete a node group. Its child groups + member nodes re-parent up; nodes are never deleted. */
  deleteNodeGroup: (id: string): Promise<void> =>
    apiDelete('/api/v1/node-groups/{id}', { path: { id } }),

  /** Device-class profiles. */
  listProfiles: (): Promise<ProfileSummary[]> => apiGet('/api/v1/profiles'),

  /** Create a profile (name + optional category/vendor). */
  createProfile: (body: ProfileInput): Promise<{ id: string }> =>
    apiPost('/api/v1/profiles', { body }),

  /** Update a profile's name / category / vendor. */
  updateProfile: (id: string, body: ProfileInput): Promise<void> =>
    apiPut('/api/v1/profiles/{id}', { path: { id }, body }),

  /** Delete a profile. */
  deleteProfile: (id: string): Promise<void> => apiDelete('/api/v1/profiles/{id}', { path: { id } }),

  /** Threshold rules (hierarchical overrides; most-specific scope wins). */
  /** A capped page of threshold rules, narrowed server-side.
   *
   *  ⚠️ Takes the whole query object rather than naming fields: hand-listing them is how
   *  `listAlertHistory` silently dropped every History filter — TypeScript checks excess
   *  properties on object *literals* only, and a value returned by `queryFor()` is not one. */
  listThresholds: (query?: ThresholdQuery): Promise<ThresholdPage> =>
    apiGet('/api/v1/thresholds', { query }),

  /** Create a threshold rule. */
  createThreshold: (body: {
    scope_level: ScopeLevel;
    scope_id: string;
    metric: string;
    direction: Direction;
    warning?: number;
    critical?: number;
    dwell_samples?: number;
  }): Promise<{ id: string }> => apiPost('/api/v1/thresholds', { body }),

  /** Delete a threshold rule. */
  deleteThreshold: (id: string): Promise<void> =>
    apiDelete('/api/v1/thresholds/{id}', { path: { id } }),

  /** A node's collection items. `resolved` returns the effective set (the profile's
   *  templates overridden by node-level items) rather than just the node-level overrides. */
  listNodeCollection: (id: string, resolved = false): Promise<StoredCollectionItem[]> =>
    arm(
      apiGet('/api/v1/nodes/{node_id}/collection', {
        path: { node_id: id },
        query: { resolved: resolved ? true : undefined },
      }),
    ),

  /** Add (or update) a collection item on a node (overrides the profile/template default). */
  addNodeCollection: (id: string, body: CollectionItemInput): Promise<{ id: string }> =>
    apiPost('/api/v1/nodes/{node_id}/collection', { path: { node_id: id }, body }),

  /** Delete a node-scope collection item by id. */
  deleteCollectionItem: (itemId: string): Promise<void> =>
    apiDelete('/api/v1/collection/{item_id}', { path: { item_id: itemId } }),

  /** Reusable collection templates (named metric bundles profiles attach). */
  listCollectionTemplates: (): Promise<CollectionTemplate[]> =>
    apiGet('/api/v1/collection-templates'),

  /** Create a template. 409 `template_name_taken` if the name is in use. */
  createCollectionTemplate: (body: { name: string; description?: string }): Promise<{ id: string }> =>
    apiPost('/api/v1/collection-templates', { body }),

  /** Delete a template (also detaches it from every profile). */
  deleteCollectionTemplate: (id: string): Promise<void> =>
    apiDelete('/api/v1/collection-templates/{id}', { path: { id } }),

  /** The curated OID catalog (MIB repository), optionally filtered by a substring.
   *
   *  The only endpoint still addressed by a hand-built URL: it percent-encodes the search term
   *  (`if%20hc`) where every other filter is form-encoded (`if+hc`). Both decode to the same value
   *  server-side, but the request assertions compare whole URL strings, so routing this through
   *  `buildUrl` would rewrite a URL nobody asked to change. */
  listMibCatalog: (q?: string): Promise<MibCatalogEntry[]> =>
    request(q ? `/api/v1/mib-catalog?q=${encodeURIComponent(q)}` : '/api/v1/mib-catalog'),

  /** Add a catalog entry. 409 `metric_name_taken` if the name is in use. */
  createMibEntry: (body: {
    metric_name: string;
    oid: string;
    collection: CollectionKind;
    metric_kind: MetricKind;
    vendor?: string;
    description?: string;
  }): Promise<{ id: string }> => apiPost('/api/v1/mib-catalog', { body }),

  /** Delete a catalog entry. */
  deleteMibEntry: (id: string): Promise<void> =>
    apiDelete('/api/v1/mib-catalog/{id}', { path: { id } }),

  /** Start a discovery sweep over explicit target IPs (the UI expands a CIDR). Stored
   *  credentials go by id (resolved server-side). The WebUI scans with stored credentials
   *  only; `communities` remains an optional ad-hoc extra for external automation. */
  startDiscoveryScan: (body: {
    targets: string[];
    communities?: string[];
    credential_ids: string[];
  }): Promise<{ scan_id: string }> => apiPost('/api/v1/discovery/scan', { body }),

  /** Poll a discovery scan's status + candidates. */
  getDiscoveryScan: (id: string): Promise<DiscoveryScan> =>
    apiGet('/api/v1/discovery/scan/{id}', { path: { id } }),

  /** Recent discovered (unclassified) devices across scans — the dashboard discovery queue. */
  getDiscoveryCandidates: (limit?: number): Promise<DiscoveryCandidate[]> =>
    apiGet('/api/v1/discovery/candidates', { query: { limit } }),

  /** Poll-loop self-monitoring (last sweep / jobs per round / results total). */
  getPollerHealth: (): Promise<PollerHealth> => apiGet('/api/v1/poller-health'),

  /** The registered distributed-poller fleet + per-pool summary (ADR-009/020). View-gated;
   *  returns the standard 503 (`admin_unavailable`) in skeleton mode. */
  listPollers: (): Promise<PollersResponse> => apiGet('/api/v1/pollers'),

  /** The nodes a poller currently holds in its published working set — the Pollers-page drill-down
   *  ("if this poller dies, what stops being monitored?"). Capped server-side; the response says
   *  whether it was truncated. */
  listPollerNodes: (id: string, limit?: number): Promise<PollerNodesResponse> =>
    apiGet('/api/v1/pollers/{id}/nodes', { path: { id }, query: { limit } }),

  /** Remove a decommissioned poller from the durable inventory. Rejects with a typed `ApiError`:
   *  409 `poller_online` if it is currently online (stop it first), 404 `poller_not_found` if it is
   *  unknown. ManageConfig-gated. */
  deletePoller: (id: string): Promise<void> => apiDelete('/api/v1/pollers/{id}', { path: { id } }),

  /** Recent core↔poller visibility outages (Phase 3 store-and-forward). View-gated; degrades to an
   *  empty list on a DB read error, and returns 503 (`admin_unavailable`) in skeleton mode. */
  listMonitoringGaps: (): Promise<MonitoringGap[]> => apiGet('/api/v1/monitoring-gaps'),

  /** Yagra self-health: reachability of PostgreSQL / TSDB / bus (indirect). */
  getSystemHealth: (): Promise<SystemHealth> => apiGet('/api/v1/system-health'),

  /** Current host resources (CPU/load/mem/disk) for core + every poller reporting telemetry.
   *  Powers the System Health "Host resources" instance selector + the Pollers table columns. */
  getSystemHosts: (): Promise<SystemHostsResponse> => apiGet('/api/v1/system/hosts'),

  /** Host CPU/load/mem/disk trends for one instance (`core` or a poller id) over a window. */
  getHostMetricRange: (
    instance: string,
    opts?: { from?: number; to?: number; step?: number },
  ): Promise<HostMetricRange> =>
    apiGet('/api/v1/system/hosts/{instance}/metrics/range', {
      path: { instance },
      query: { from: opts?.from, to: opts?.to, step: opts?.step },
    }),

  /** Running core/API version (for Settings ▸ About). Public — no auth required. */
  getVersion: (): Promise<VersionInfo> => apiGet('/api/v1/version'),

  /** Which binary is actually running, how much schema is applied, and whether this deployment
   *  could still be taken back to an earlier release (Settings ▸ Upgrade, ADR-050).
   *  Needs manage-configuration. */
  getUpgradeStatus: (): Promise<UpgradeStatus> => apiGet('/api/v1/system/upgrade'),

  /** Move this deployment to a release. Returns as soon as the updater has the request — the work
   *  outlives core, which restarts partway through, so poll `getUpgradeStatus` for the outcome. */
  applyUpgrade: (targetTag: string): Promise<UpgradeRunAccepted> =>
    apiPost('/api/v1/system/upgrade', { body: { target_tag: targetTag } }),

  /** Ask the updater to re-read the registry now rather than on its own 24-hour clock (ADR-050).
   *  Accepted asynchronously — the answer arrives as a newer `available.written_at` on the next
   *  `getUpgradeStatus`, so callers watch that rather than this promise. */
  checkUpgrades: (): Promise<void> => apiPost('/api/v1/system/upgrade/check', {}),

  /** Turn upgrading from the WebUI on or off for this deployment (ADR-050). Stored in the
   *  database, so it survives the upgrades it governs; the updater picks it up within one beat. */
  setUpgradeEnabled: (enabled: boolean): Promise<void> =>
    apiPut('/api/v1/system/upgrade/enabled', { body: { enabled } }),

  /** Install a release from a `docker save` archive, for a site with no reachable registry
   *  (ADR-050 Increment 3). The path is written out because the body is raw bytes, not JSON, so
   *  the generated helpers do not cover it — the same reason the two downloads write theirs out. */
  uploadUpgradeBundle: (
    file: Blob,
    targetTag: string,
    onProgress?: (fraction: number) => void,
  ): Promise<UpgradeRunAccepted> =>
    uploadRaw(
      `/api/v1/system/upgrade/bundle?target_tag=${encodeURIComponent(targetTag)}`,
      file,
      onProgress,
    ),

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
  ): Promise<{ created: number }> => apiPost('/api/v1/discovery/import', { body: { nodes } }),

  /** Device-classification rules (discovery → suggested profile), ascending by priority. */
  listClassificationRules: (): Promise<ClassificationRule[]> =>
    apiGet('/api/v1/classification-rules'),

  /** Create a classification rule. 400s on bad regex/prefix/profile. */
  createClassificationRule: (body: ClassificationRuleInput): Promise<{ id: string }> =>
    apiPost('/api/v1/classification-rules', { body }),

  /** Update a classification rule in place. */
  updateClassificationRule: (id: string, body: ClassificationRuleInput): Promise<void> =>
    apiPut('/api/v1/classification-rules/{id}', { path: { id }, body }),

  /** Delete a classification rule. */
  deleteClassificationRule: (id: string): Promise<void> =>
    apiDelete('/api/v1/classification-rules/{id}', { path: { id } }),

  /** The metrics in a template. */
  listTemplateItems: (id: string): Promise<StoredCollectionItem[]> =>
    apiGet('/api/v1/collection-templates/{id}/items', { path: { id } }),

  /** Add (or update) a metric in a template. */
  addTemplateItem: (id: string, body: CollectionItemInput): Promise<{ id: string }> =>
    apiPost('/api/v1/collection-templates/{id}/items', { path: { id }, body }),

  /** Delete a metric from a template. */
  deleteTemplateItem: (templateId: string, itemId: string): Promise<void> =>
    apiDelete('/api/v1/collection-templates/{id}/items/{item_id}', {
      path: { id: templateId, item_id: itemId },
    }),

  /** The templates a profile attaches. */
  listProfileTemplates: (id: string): Promise<CollectionTemplate[]> =>
    apiGet('/api/v1/profiles/{id}/templates', { path: { id } }),

  /** Replace the set of templates a profile attaches. */
  setProfileTemplates: (id: string, templateIds: string[]): Promise<void> =>
    apiPut('/api/v1/profiles/{id}/templates', {
      path: { id },
      body: { template_ids: templateIds },
    }),

  /** Credential metadata listing (never includes secret values). */
  listCredentials: (): Promise<CredentialSummary[]> => apiGet('/api/v1/credentials'),

  /** Store a new encrypted credential. */
  createCredential: (body: { name: string; kind: string; secret: string }): Promise<{ id: string }> =>
    apiPost('/api/v1/credentials', { body }),

  /** Update a credential. `name` is required; pass `secret` (with its `kind`) only to replace the
   *  stored secret — omit it to rename in place (the secret is never returned, so editing keeps
   *  the existing one unless you re-enter it). */
  updateCredential: (
    id: string,
    body: { name: string; kind?: string; secret?: string },
  ): Promise<void> => apiPut('/api/v1/credentials/{id}', { path: { id }, body }),

  /** Delete a credential. */
  deleteCredential: (id: string): Promise<void> =>
    apiDelete('/api/v1/credentials/{id}', { path: { id } }),

  /** Active alerts. */
  listAlerts: (): Promise<Alert[]> => apiGet('/api/v1/alerts'),

  /** Alert history page, newest first.
   *
   *  The keyset cursor is a **pair**: pass the last row's `recorded_at` as `before` *and* its `id`
   *  as `before_id`. Both, always — a whole flush of alerts is written in one transaction and
   *  therefore shares one `recorded_at`, so a timestamp-only cursor lands inside that group and
   *  skips its remaining rows. Build it with `pages/historyCursor.ts::nextCursor`. */
  //
  //  ⚠️ Takes the **generated** query type and forwards it whole. The hand-written parameter list
  //  this replaces named only limit/before/before_id, so every filter a caller passed was dropped
  //  here — silently, because `queryFor()` returns a value rather than an object literal and
  //  TypeScript's excess-property check does not apply to those. Do not re-list the fields.
  listAlertHistory: (query?: AlertHistoryQuery): Promise<AlertHistoryRow[]> =>
    apiGet('/api/v1/alerts/history', { query }),

  /** Nodes generating the most alert fires over a trailing window (chronic offenders). */
  getAlertTopNodes: (opts?: { window?: number; limit?: number }): Promise<RankedAlertNodes> =>
    apiGet('/api/v1/alerts/top-nodes', { query: { window: opts?.window, limit: opts?.limit } }),

  /** Alert fires bucketed weekday×hour over the last `days` (heatmap). */
  getAlertCalendar: (days?: number): Promise<CalendarBucket[]> =>
    apiGet('/api/v1/alerts/calendar', { query: { days } }),

  /** Recent up/down transitions (latest fires + recoveries). */
  getAlertTransitions: (limit?: number): Promise<AlertTransition[]> =>
    apiGet('/api/v1/alerts/transitions', { query: { limit } }),

  /** The dependency graph: nodes + parent edges + state + active root-cause attribution. The
   *  endpoint is keyset-paginated (S7) — this pages through `next_cursor` and assembles the whole
   *  graph, so callers keep the same `{ nodes }` shape. Views fetch this once and keep node state
   *  fresh via the node-state SSE stream (`useNodeStates`) rather than re-fetching every 15s. */
  getTopology: async (): Promise<{ nodes: TopologyNode[] }> => {
    const nodes: TopologyNode[] = [];
    let cursor: string | undefined;
    // Bounded loop: pages are ≤5000 server-side, so even a 50k fleet is ~10 iterations. The guard
    // caps a pathological run at 100k nodes (200 pages) rather than looping unbounded.
    for (let i = 0; i < 200; i++) {
      const page = await apiGet('/api/v1/topology', { query: { cursor } });
      nodes.push(...page.nodes);
      if (!page.next_cursor) break;
      cursor = page.next_cursor;
    }
    return { nodes };
  },

  /** The derived connectivity graph: undirected links between nodes, with the evidence that
   *  produced each one. Same keyset-paging contract as `getTopology`, and the same bounded loop.
   *
   *  `summary` and `derived_at` come from the last derivation run and are the same on every page,
   *  so the last page's copy is the one returned — they describe the run, not the page. */
  getTopologyLinks: async (): Promise<{
    links: TopologyLink[];
    summary: TopologyLinkSummary;
    derivedAt: string | null;
  }> => {
    const links: TopologyLink[] = [];
    let cursor: number | undefined;
    let summary: TopologyLinkSummary = {};
    let derivedAt: string | null = null;
    for (let i = 0; i < 200; i++) {
      const page = await apiGet('/api/v1/topology/links', { query: { cursor } });
      links.push(...page.links);
      summary = page.summary;
      derivedAt = page.derived_at ?? null;
      if (!page.next_cursor) break;
      cursor = page.next_cursor;
    }
    return { links, summary, derivedAt };
  },

  /** Operator decisions that override the derived graph (pin / hide / which end is upstream).
   *  Not paged — the number of decisions is bounded by what an operator typed in, not by the fleet. */
  getLinkOverrides: async (): Promise<LinkOverrideRow[]> =>
    (await apiGet('/api/v1/topology/link-overrides')).overrides,

  /** Record a decision about a link, replacing any previous decision of the same kind for the pair.
   *  Takes effect on the next derivation cycle. */
  createLinkOverride: (body: {
    a_node: string;
    b_node: string;
    action: LinkOverrideAction;
    direction?: LinkDirection | null;
    note?: string | null;
  }): Promise<{ id: string }> => apiPost('/api/v1/topology/link-overrides', { body }),

  /** Remove a decision, letting the derivation's own answer stand again. */
  deleteLinkOverride: (id: string): Promise<void> =>
    apiDelete('/api/v1/topology/link-overrides/{id}', { path: { id } }),

  /** What the derived dependency graph would do to alerting, against what the manual one does —
   *  including the alerts that would newly be suppressed, and any pollers with no place in the
   *  graph yet (which block enabling derived mode). */
  getTopologyShadow: (): Promise<TopologyShadow> => apiGet('/api/v1/topology/shadow'),

  /** Choose which dependency graph drives suppression. Moving to `derived` is refused while a pool
   *  that has nodes has an unplaced poller. */
  setTopologyMode: (mode: TopologyMode): Promise<void> =>
    apiPut('/api/v1/settings/topology', { body: { mode } }),

  /** Name the node a poller attaches to, rooting the derived graph. `null` clears it. */
  setPollerAnchor: (id: string, nodeId: string | null): Promise<void> =>
    apiPut('/api/v1/pollers/{id}/anchor', { path: { id }, body: { node_id: nodeId } }),

  /** Fleet-wide status summary (total + per-state counts), computed server-side so the dashboard
   *  status widgets are correct over the whole fleet, not the first page of nodes. */
  getFleetSummary: (): Promise<FleetSummary> => apiGet('/api/v1/fleet/summary'),

  /** Per-group health rollup (each group's direct-member state counts), computed server-side so the
   *  site-matrix / region-rollup / geo-map widgets aggregate the whole fleet, not the first page of
   *  nodes (A-1). The client joins these to the group tree for names/geo and sums descendants. */
  getFleetGroupSummary: (): Promise<FleetGroupSummary> => apiGet('/api/v1/fleet/group-summary'),

  /** Fleet data coverage + the stale-data watchlist (silent/blind-spot nodes). */
  getFleetCoverage: (): Promise<FleetCoverage> => apiGet('/api/v1/fleet/coverage'),

  /** Node-state counts over time (fleet health timeline; default last 24h). */
  getStateHistory: (opts?: { from?: number; to?: number }): Promise<StateHistory> =>
    apiGet('/api/v1/fleet/state-history', { query: { from: opts?.from, to: opts?.to } }),

  /** Fleet aggregate ingress/egress (bits/sec) over time (default last 24h). */
  getThroughputRange: (opts?: {
    from?: number;
    to?: number;
    step?: number;
  }): Promise<ThroughputRange> =>
    apiGet('/api/v1/metrics/throughput-range', {
      query: { from: opts?.from, to: opts?.to, step: opts?.step },
    }),

  /** Busiest-links × time throughput heatmap (default top 8 over last 6h). */
  getInterfaceHeatmap: (opts?: {
    limit?: number;
    from?: number;
    to?: number;
    step?: number;
  }): Promise<InterfaceHeatmap> =>
    apiGet('/api/v1/metrics/interface-heatmap', {
      query: { limit: opts?.limit, from: opts?.from, to: opts?.to, step: opts?.step },
    }),

  /** Notification channels (metadata only; the secret config is never returned). */
  listNotificationChannels: (): Promise<NotificationChannel[]> =>
    apiGet('/api/v1/notification-channels'),

  /** Create a notification channel (name + secret config, sealed server-side). */
  createNotificationChannel: (body: {
    name: string;
    config: ChannelConfigInput;
  }): Promise<{ id: string }> => apiPost('/api/v1/notification-channels', { body }),

  /** Enable/disable a channel. */
  setNotificationChannelEnabled: (id: string, enabled: boolean): Promise<void> =>
    apiPut('/api/v1/notification-channels/{id}', { path: { id }, body: { enabled } }),

  /** Delete a channel (and drop it from any rule). */
  deleteNotificationChannel: (id: string): Promise<void> =>
    apiDelete('/api/v1/notification-channels/{id}', { path: { id } }),

  /** Replace a channel's notification template (ADR-039). Both fields go together; `null` on one
   *  restores Yagra's built-in wording for it. A template that does not compile is rejected with
   *  a typed 400 rather than being stored and failing during an outage. */
  setNotificationTemplate: (
    id: string,
    body: { subject: string | null; body: string | null },
  ): Promise<void> =>
    apiPut('/api/v1/notification-channels/{id}/template', { path: { id }, body }),

  /** Render a template against a representative alert without saving it. A template that cannot
   *  be used comes back as `problems` alongside the built-in text that would be sent instead —
   *  HTTP 200, because these are notes about the text being typed, not a failed request. */
  previewNotificationTemplate: (body: {
    kind: ChannelKind;
    event?: NotifyEvent;
    subject: string | null;
    body: string | null;
  }): Promise<TemplatePreview> =>
    apiPost('/api/v1/notification-channels/preview', { body }),

  /** Every variable a notification template may reference, with what each one means. */
  listTemplateVariables: (): Promise<TemplateVariable[]> =>
    apiGet('/api/v1/notification-channels/template-variables'),

  /** Routing rules (which alerts, by severity, fan out to which channels). */
  listRoutingRules: (): Promise<RoutingRule[]> => apiGet('/api/v1/routing-rules'),

  /** Create a routing rule. `severity` null ⇒ matches all severities. */
  createRoutingRule: (body: {
    name: string;
    severity: Severity | null;
    channel_ids: string[];
  }): Promise<{ id: string }> => apiPost('/api/v1/routing-rules', { body }),

  /** Enable/disable a rule. */
  setRoutingRuleEnabled: (id: string, enabled: boolean): Promise<void> =>
    apiPut('/api/v1/routing-rules/{id}', { path: { id }, body: { enabled } }),

  /** Delete a routing rule. */
  deleteRoutingRule: (id: string): Promise<void> =>
    apiDelete('/api/v1/routing-rules/{id}', { path: { id } }),

  // ── Passive events (syslog / SNMP traps / webhooks) ──

  /** Webhook ingest sources (the bearer token is never returned after create/rotate). */
  listEventSources: (): Promise<EventSource[]> => apiGet('/api/v1/event-sources'),

  /** Create a webhook source; the response `token` is shown once and only its hash is stored. */
  createEventSource: (body: {
    name: string;
    node_id?: string | null;
  }): Promise<{ id: string; token: string }> => apiPost('/api/v1/event-sources', { body }),

  /** Update a source's name / enabled / node binding. */
  updateEventSource: (
    id: string,
    body: { name: string; enabled: boolean; node_id?: string | null },
  ): Promise<void> => apiPut('/api/v1/event-sources/{id}', { path: { id }, body }),

  /** Replace a source's token; the new `token` is shown once. */
  rotateEventSourceToken: (id: string): Promise<{ token: string }> =>
    apiPost('/api/v1/event-sources/{id}/rotate-token', { path: { id } }),

  /** Delete a webhook source. */
  deleteEventSource: (id: string): Promise<void> =>
    apiDelete('/api/v1/event-sources/{id}', { path: { id } }),

  /** Event match rules (substring/regex → alert). */
  listEventRules: (): Promise<EventRule[]> => apiGet('/api/v1/event-rules'),

  /** Create an event rule. */
  createEventRule: (body: EventRuleInput): Promise<{ id: string }> =>
    apiPost('/api/v1/event-rules', { body }),

  /** Update an event rule. */
  updateEventRule: (id: string, body: EventRuleInput): Promise<void> =>
    apiPut('/api/v1/event-rules/{id}', { path: { id }, body }),

  /** Delete an event rule. */
  deleteEventRule: (id: string): Promise<void> =>
    apiDelete('/api/v1/event-rules/{id}', { path: { id } }),

  /** Try a pattern against a sample message (compile errors returned in-band). */
  testEventRule: (body: {
    match_kind: 'substring' | 'regex';
    pattern: string;
    clear_pattern?: string | null;
    sample: string;
  }): Promise<EventRuleTestResult> => apiPost('/api/v1/event-rules/test', { body }),

  /** Received events, keyset-paged on `recorded_at` (newest first) with optional filters. */
  listEvents: (
    opts?: EventStatsFilter & {
      limit?: number;
      /** Keyset paging cursor — rows strictly older than this. Distinct from `start`/`end`, which
       *  bound the range being searched. */
      before?: string;
    },
  ): Promise<EventRow[]> =>
    apiGet('/api/v1/events', {
      query: {
        ...eventStatsQuery(opts ?? {}),
        limit: opts?.limit,
        before: opts?.before || undefined,
      },
    }),

  /** Categorical passive-event summary counts (kind/action/trap/source), ordered by count desc.
   *  Backed by the log store when enabled, else PostgreSQL — same filter as the event log. */
  getEventStats: (
    groupBy: 'kind' | 'action' | 'trap' | 'source',
    opts?: EventStatsFilter & { limit?: number },
  ): Promise<EventStatBucket[]> =>
    arm(
      apiGet('/api/v1/events/stats', {
        query: { ...eventStatsQuery(opts ?? {}), group_by: groupBy, limit: opts?.limit },
      }),
    ),

  /** Passive-event volume time series (counts per `bucketSecs` window; `splitKind` adds a per-kind
   *  breakdown). */
  getEventVolume: (
    opts?: EventStatsFilter & { bucketSecs?: number; splitKind?: boolean },
  ): Promise<EventTimeBucket[]> =>
    arm(
      apiGet('/api/v1/events/stats', {
        query: {
          ...eventStatsQuery(opts ?? {}),
          group_by: 'time',
          bucket_secs: opts?.bucketSecs,
          split: opts?.splitKind ? 'kind' : undefined,
        },
      }),
    ),

  // ── Flow analysis (ADR-031) — served only when a ClickHouse flow store is configured; the
  // endpoints 503 (`flow_unavailable`) otherwise. `from`/`to` are unix seconds. ──
  /** Bytes/packets over time per protocol (trend) for a node/window (proto filter applies). */
  getNodeFlowSeries: (
    nodeId: string,
    opts: { from: number; to: number } & FlowFilters,
  ): Promise<FlowPoint[]> =>
    apiGet('/api/v1/nodes/{node_id}/flow/series', {
      path: { node_id: nodeId },
      query: flowQuery(opts),
    }),

  /** Top source hosts by bytes. */
  getNodeFlowTopTalkers: (
    nodeId: string,
    opts: { from: number; to: number; limit?: number } & FlowFilters,
  ): Promise<FlowTalker[]> =>
    apiGet('/api/v1/nodes/{node_id}/flow/top-talkers', {
      path: { node_id: nodeId },
      query: flowQuery(opts),
    }),

  /** Top src→dst conversations by bytes. */
  getNodeFlowConversations: (
    nodeId: string,
    opts: { from: number; to: number; limit?: number } & FlowFilters,
  ): Promise<FlowConversation[]> =>
    apiGet('/api/v1/nodes/{node_id}/flow/conversations', {
      path: { node_id: nodeId },
      query: flowQuery(opts),
    }),

  /** Top destination ports by bytes. */
  getNodeFlowTopPorts: (
    nodeId: string,
    opts: { from: number; to: number; limit?: number } & FlowFilters,
  ): Promise<FlowPortAgg[]> =>
    apiGet('/api/v1/nodes/{node_id}/flow/top-ports', {
      path: { node_id: nodeId },
      query: flowQuery(opts),
    }),

  /** Traffic by IP protocol. */
  getNodeFlowProtocols: (
    nodeId: string,
    opts: { from: number; to: number; limit?: number } & FlowFilters,
  ): Promise<FlowProtoAgg[]> =>
    apiGet('/api/v1/nodes/{node_id}/flow/protocols', {
      path: { node_id: nodeId },
      query: flowQuery(opts),
    }),

  /** Top autonomous systems by bytes (`dir` = 'src' | 'dst', default 'dst'). */
  getNodeFlowTopAs: (
    nodeId: string,
    opts: { from: number; to: number; limit?: number; dir?: 'src' | 'dst' } & FlowFilters,
  ): Promise<FlowAsAgg[]> =>
    apiGet('/api/v1/nodes/{node_id}/flow/top-as', {
      path: { node_id: nodeId },
      query: flowQuery(opts),
    }),

  // ── Fleet-wide flow (all exporters) — the dashboard Traffic-flow widgets. Same shapes as the
  // per-node endpoints, no node scope; same 503 (`flow_unavailable`) gate. ──
  /** Fleet bytes/packets over time per protocol (trend). */
  getFlowSeries: (opts: { from: number; to: number } & FlowFilters): Promise<FlowPoint[]> =>
    apiGet('/api/v1/flow/series', { query: flowQuery(opts) }),

  /** Fleet top source hosts by bytes. */
  getFlowTopTalkers: (
    opts: { from: number; to: number; limit?: number } & FlowFilters,
  ): Promise<FlowTalker[]> => apiGet('/api/v1/flow/top-talkers', { query: flowQuery(opts) }),

  /** Fleet top src→dst conversations by bytes. */
  getFlowConversations: (
    opts: { from: number; to: number; limit?: number } & FlowFilters,
  ): Promise<FlowConversation[]> =>
    apiGet('/api/v1/flow/conversations', { query: flowQuery(opts) }),

  /** Fleet top destination ports by bytes. */
  getFlowTopPorts: (
    opts: { from: number; to: number; limit?: number } & FlowFilters,
  ): Promise<FlowPortAgg[]> => apiGet('/api/v1/flow/top-ports', { query: flowQuery(opts) }),

  /** Fleet traffic by IP protocol. */
  getFlowProtocols: (
    opts: { from: number; to: number; limit?: number } & FlowFilters,
  ): Promise<FlowProtoAgg[]> => apiGet('/api/v1/flow/protocols', { query: flowQuery(opts) }),

  /** Fleet top autonomous systems by bytes (`dir` = 'src' | 'dst', default 'dst'). */
  getFlowTopAs: (
    opts: { from: number; to: number; limit?: number; dir?: 'src' | 'dst' } & FlowFilters,
  ): Promise<FlowAsAgg[]> => apiGet('/api/v1/flow/top-as', { query: flowQuery(opts) }),

  /** Maintenance windows (nodes covered by an active one are in `maintenance` state). */
  listMaintenanceWindows: (): Promise<MaintenanceWindow[]> => apiGet('/api/v1/maintenance-windows'),

  /** Create a maintenance window. Times are RFC 3339; scope mirrors thresholds plus `group_id`
   *  (a folder group, resolved recursively). */
  createMaintenanceWindow: (body: {
    name: string;
    scope_level: MaintenanceScopeLevel;
    scope_id: string;
    starts_at: string;
    ends_at: string;
  }): Promise<{ id: string }> => apiPost('/api/v1/maintenance-windows', { body }),

  /** Enable/disable a maintenance window. */
  setMaintenanceWindowEnabled: (id: string, enabled: boolean): Promise<void> =>
    apiPut('/api/v1/maintenance-windows/{id}', { path: { id }, body: { enabled } }),

  /** Delete a maintenance window. */
  deleteMaintenanceWindow: (id: string): Promise<void> =>
    apiDelete('/api/v1/maintenance-windows/{id}', { path: { id } }),

  /** End an **active** maintenance window now. The row stays, as a record of the maintenance that
   *  actually happened, and reads as `ended` afterwards — the inventory tree's release for a
   *  window it can act on. `404` if the window is not currently active. */
  endMaintenanceWindow: (id: string): Promise<void> =>
    apiPost('/api/v1/maintenance-windows/{id}/end', { path: { id } }),

  /** Delete every *ended* maintenance window this account can see, and answer how many went.
   *  The browser sends only the discriminator: the server's clock decides what has ended and the
   *  caller's group scope decides which rows are eligible. */
  clearEndedMaintenanceWindows: (): Promise<{ deleted: number }> =>
    apiDelete('/api/v1/maintenance-windows', { query: { status: 'ended' } }),

  /** Unexpired mutes (notification silences; alerts still show in the UI/history). */
  listMutes: (): Promise<Mute[]> => apiGet('/api/v1/mutes'),

  /** Create a mute. `scope_kind` is `node` (one node, optionally one `metric_name`) or `group`
   *  (every node under a folder group, recursive — `metric_name` ignored); `scope_id` is the
   *  node/group id. `until` is RFC 3339. */
  createMute: (body: {
    scope_kind: 'node' | 'group';
    scope_id: string;
    metric_name?: string;
    until: string;
    reason?: string;
  }): Promise<{ id: string }> => apiPost('/api/v1/mutes', { body }),

  /** Delete (lift) a mute. */
  deleteMute: (id: string): Promise<void> => apiDelete('/api/v1/mutes/{id}', { path: { id } }),

  /** Nodes currently released from a suppression they only inherited. Fetched alongside the window
   *  and mute lists — a released node is not suppressed, so the tree needs all three to be right. */
  listSuppressionExemptions: (): Promise<SuppressionExemption[]> =>
    apiGet('/api/v1/suppression-exemptions'),

  /** Release one node from the maintenance it *inherits* (its folder group, profile, a tag, or a
   *  fleet-wide window), or put it back. The rest of the group stays covered.
   *
   *  The browser sends no expiry: the server sizes the release to the coverage actually in force,
   *  so it can never outlive the window and quietly exclude the node from the next one. `400
   *  not_suppressed` when nothing is inherited — including when the only window *names* this node,
   *  which is ended directly instead. */
  setNodeMaintenanceExemption: (nodeId: string, exempt: boolean): Promise<void> =>
    apiPut('/api/v1/nodes/{node_id}/maintenance-exemption', {
      path: { node_id: nodeId },
      body: { exempt },
    }),

  /** The mute counterpart of {@link api.setNodeMaintenanceExemption} — releases one node from a
   *  group mute. A mute naming the node is lifted directly instead. */
  setNodeMuteExemption: (nodeId: string, exempt: boolean): Promise<void> =>
    apiPut('/api/v1/nodes/{node_id}/mute-exemption', {
      path: { node_id: nodeId },
      body: { exempt },
    }),

  /** The role-vs-privilege matrix: which permissions each role grants. Read-only (View). */
  listRoles: (): Promise<RoleMatrix> => apiGet('/api/v1/roles'),

  /** User accounts (metadata only; never the password hash). Requires admin (ManageUsers). */
  listUsers: (): Promise<UserSummary[]> => apiGet('/api/v1/users'),

  /** Create a user account. The password is hashed server-side and never returned. */
  /** Create an account. `password` is required for a local account and refused for a service one
   *  (a machine account cannot sign in), so it is optional here and validated by the caller. */
  createUser: (body: {
    username: string;
    password?: string;
    role: Role;
    kind?: UserKind;
  }): Promise<{ id: string }> => apiPost('/api/v1/users', { body }),

  /** Delete a user account. Refused (409 `last_admin`) for the last admin. */
  deleteUser: (id: string): Promise<void> => apiDelete('/api/v1/users/{id}', { path: { id } }),

  /** Change a user's role. Refused (409 `last_admin`) when demoting the last admin. */
  setUserRole: (id: string, role: Role): Promise<void> =>
    apiPut('/api/v1/users/{id}/role', { path: { id }, body: { role } }),

  /** Limit an account to a set of node groups, or restore fleet-wide visibility with `'All'`.
   *  Revokes the account's sessions server-side — the scope is captured in the session token, so a
   *  live one would keep the old, wider view. Refused (409 `admin_is_unscoped`) for an admin, and
   *  (400) for a scope naming no groups or something that is not an existing group id. */
  setUserScope: (id: string, scope: Scope): Promise<void> =>
    apiPut('/api/v1/users/{id}/scope', { path: { id }, body: { scope } }),

  /** Enable or disable a user account. Refused (409 `last_admin`) when disabling the last
   *  admin that can still log in. A disabled account is kept for the audit trail. */
  setUserEnabled: (id: string, enabled: boolean): Promise<void> =>
    apiPut('/api/v1/users/{id}/enabled', { path: { id }, body: { enabled } }),

  /** Reset a user's password (hashed server-side; never echoed back). */
  setUserPassword: (id: string, password: string): Promise<void> =>
    apiPut('/api/v1/users/{id}/password', { path: { id }, body: { password } }),

  /** Audit log page, newest first (admin-only).
   *
   *  `before` is the keyset cursor: pass the last row's `at` to fetch the next (older) page.
   *  `since`/`until` are a different thing — the window being searched — so a filtered scroll keeps
   *  its bounds while the cursor advances.
   *
   *  The filters are applied **server-side**. They used to narrow the loaded pages in the browser,
   *  which meant "last 30 days, DELETE only" examined the newest 100 rows and hid every older match.
   *  Build the query with `pages/auditQuery.ts::queryFor` so an unset filter is `undefined` and not
   *  the empty string, which the backend would reject as an unknown value. */
  listAudit: (query?: AuditQuery): Promise<AuditRow[]> => apiGet('/api/v1/audit', { query }),

  /** The caller's saved My Dashboard layout, or `null` if none saved yet (the client then uses
   *  its default). The body is opaque JSON — the client owns and migrates the widget shape
   *  (types in `dashboard/types`); the server stores it verbatim, scoped to the logged-in user. */
  getDashboard: (): Promise<unknown> => apiGet('/api/v1/dashboard'),

  /** Save (replace) the caller's My Dashboard layout. */
  putDashboard: (layout: unknown): Promise<{ ok: boolean }> =>
    apiPut('/api/v1/dashboard', { body: layout }),

  /** The global Shared Dashboard layout (one board shown to all users), or null until an admin
   *  has saved one. Open-read; the write side is admin-only. Same opaque-JSON contract. */
  getSharedDashboard: (): Promise<unknown> => apiGet('/api/v1/shared-dashboard'),

  /** Save (replace) the global Shared Dashboard layout. Admin only — a 403 means the caller's role
   *  may not change it (the change applies to every user). */
  putSharedDashboard: (layout: unknown): Promise<{ ok: boolean }> =>
    apiPut('/api/v1/shared-dashboard', { body: layout }),

  /** The caller's saved WebUI preferences, or `null` if none saved yet (ADR-058). Opaque JSON on
   *  the same contract as the dashboard layout: the client owns the shape (`prefs/serverPrefs.ts`)
   *  and the server stores it verbatim, keyed by the signed-in account.
   *
   *  ⚠️ A core that predates ADR-058 answers a **bodyless 404**. Callers must treat 404/405 and a
   *  `null` body as the same thing — "keep the browser-local value" — and must not surface it. */
  getPreferences: (): Promise<unknown> => apiGet('/api/v1/preferences'),

  /** Save (replace) the caller's WebUI preferences. ⚠️ Every call writes an audit row (there is no
   *  per-route opt-out), so callers must debounce rather than save per input event. */
  putPreferences: (prefs: unknown): Promise<{ ok: boolean }> =>
    apiPut('/api/v1/preferences', { body: prefs }),

  /** The current principal (role). Requires a valid session. */
  me: (): Promise<AuthMe> => apiGet('/api/v1/auth/me'),

  /** Log in; stores the bearer token on success. */
  login: async (username: string, password: string): Promise<{ token: string; role: string }> => {
    const res = await apiPost('/api/v1/auth/login', { body: { username, password } });
    setToken(res.token);
    return res;
  },

  /** Log out: revoke the token server-side (so it can't be reused), then forget it locally.
   *  The local clear always happens even if the network call fails — a best-effort revoke must
   *  never trap the user in a logged-in UI. */
  logout: async (): Promise<void> => {
    try {
      await apiPost('/api/v1/auth/logout');
    } catch {
      // Ignore: the token may already be invalid/expired; we still clear it locally below.
    }
    setToken(null);
  },

  // ── OIDC (external IdP login) ──
  /** Begin SSO: get the IdP authorization URL to redirect the browser to. */
  oidcAuthorize: (): Promise<{ authorize_url: string }> => apiGet('/api/v1/auth/oidc/authorize'),

  /** Complete SSO from the IdP redirect: exchange code+state for a session; stores the token. */
  oidcCallback: async (code: string, state: string): Promise<{ token: string; role: string }> => {
    const res = await apiPost('/api/v1/auth/oidc/callback', { body: { code, state } });
    setToken(res.token);
    return res;
  },

  // ── OIDC provider config (Settings ▸ Auth, ManageUsers) ──
  /** List configured OIDC providers (never includes the client_secret). */
  listOidcProviders: (): Promise<OidcProviderSummary[]> => apiGet('/api/v1/settings/oidc'),

  /** Create an OIDC provider. */
  createOidcProvider: (body: OidcProviderInput): Promise<{ id: string }> =>
    apiPost('/api/v1/settings/oidc', { body }),

  /** Update an OIDC provider (omit client_secret to keep the stored one). */
  updateOidcProvider: (id: string, body: OidcProviderInput): Promise<void> =>
    apiPut('/api/v1/settings/oidc/{id}', { path: { id }, body }),

  /** Delete an OIDC provider. */
  deleteOidcProvider: (id: string): Promise<void> =>
    apiDelete('/api/v1/settings/oidc/{id}', { path: { id } }),

  // ── LDAP/AD directory (Settings ▸ Auth, ManageUsers) — ADR-041 ──────────────────────────────
  /** The configured directory, or `{ config: null }` when none is saved. Never the bind password. */
  getLdapConfig: (): Promise<{ config?: LdapConfigView | null }> =>
    apiGet('/api/v1/settings/ldap'),

  /** Save the directory. **Omit** `bind_password` to keep the stored one — sending an empty string
   *  is rejected, because a blank bind password is an anonymous search rather than an absent one. */
  saveLdapConfig: (body: LdapConfigInput): Promise<void> =>
    apiPut('/api/v1/settings/ldap', { body }),

  /** Exercise the **saved** directory configuration. With a username it also reports that user's DN,
   *  groups and resolved role — without binding as them, so it is not a credential-testing proxy and
   *  cannot push anyone towards their domain's lockout threshold. */
  testLdapConfig: (username?: string): Promise<LdapTestResult> =>
    apiPost('/api/v1/settings/ldap/test', { body: { username: username ?? null } }),

  // ── Per-poller bus tokens + the site bundle (Settings ▸ Pollers, ManageSystem) — ADR-065 ───
  /** Issue a poller its own bus token and get back the whole archive its site needs — `.env`, the
   *  bus certificate, the composition and a README, as `tar.gz` bytes.
   *
   *  The response IS the token's only existence: only a digest is stored, so nothing can serve it
   *  again. Creates the inventory row when the poller has not connected yet, which is how a site is
   *  prepared before anything runs there. Returns the blob and the filename the server chose. */
  issuePollerToken: (
    id: string,
    body: { pool?: string; host?: string; port?: number },
  ): Promise<Download> =>
    fetchBlob(`/api/v1/pollers/${encodeURIComponent(id)}/token`, 'poller_token_failed', {
      method: 'POST',
      body: JSON.stringify(body),
      headers: { 'content-type': 'application/json' },
    }),

  /** Revoke a poller's token, returning it to the deployment-wide bootstrap secret. Not the same as
   *  deleting the poller — the inventory row, its anchor and its history stay. */
  revokePollerToken: (id: string): Promise<void> =>
    apiDelete('/api/v1/pollers/{id}/token', { path: { id } }),

  // ── The bus (Settings ▸ Pollers, ManageSystem) — ADR-065 ───────────────────────────────────
  /** The certificate remote pollers must pin, whether the bus is encrypted, and whether the switch
   *  below can be operated. Carries the certificate — public by construction, and the thing a site
   *  is handed — and never the private key. */
  getBus: (): Promise<BusStatus> => apiGet('/api/v1/settings/bus'),

  /** Reissue the bus certificate covering `names` (added to the deployment's internal defaults).
   *  The stored certificate changes immediately; the bus serves it only after it is restarted. */
  regenerateBusCert: (names: string[]): Promise<BusStatus> =>
    apiPost('/api/v1/settings/bus/certificate', { body: { names } }),

  /** Turn acceptance of remote-site pollers on or off. The bus, core and the co-located poller are
   *  recreated, so monitoring stops for the duration and this core restarts — expect the request to
   *  return 202 and the connection to drop shortly after. The `poller_secret` in the response is
   *  shown once. */
  setBusRemote: (enabled: boolean, names: string[]): Promise<BusRemoteAccepted> =>
    apiPut('/api/v1/settings/bus/remote', { body: { enabled, names } }),

  // ── WebUI TLS certificate (Settings ▸ TLS, ManageConfig) — ADR-044 ─────────────────────────
  /** What the WebUI is serving, or `{ config: null }` before the first certificate exists.
   *  Includes the certificate chain — public by construction, and offered as a download — and never
   *  the private key. */
  getWebTls: (): Promise<WebTlsStatus> => apiGet('/api/v1/settings/tls'),

  /** Import a certificate and its key, both PEM. Live within seconds, nothing restarts. The server
   *  validates before it commits, so a rejection means nothing changed. */
  importWebTls: (certificate: string, privateKey: string): Promise<WebTlsStatus> =>
    apiPut('/api/v1/settings/tls', { body: { certificate, private_key: privateKey } }),

  /** Generate a new self-signed certificate for `names` (empty = the deployment's defaults).
   *  Replaces whatever is being served. */
  regenerateWebTls: (names: string[]): Promise<WebTlsStatus> =>
    apiPost('/api/v1/settings/tls/regenerate', { body: { names } }),

  // ── API tokens (Settings ▸ API tokens, ManageUsers) — the MCP/API client credential (ADR-028) ──
  /** List API tokens (metadata only — never the raw token). */
  listApiTokens: (): Promise<ApiTokenSummary[]> => apiGet('/api/v1/api-tokens'),

  /** Create an API token; the response `token` is shown once and only its hash is stored. */
  createApiToken: (body: ApiTokenInput): Promise<CreatedApiToken> =>
    apiPost('/api/v1/api-tokens', { body }),

  /** Revoke (soft-delete) an API token. */
  revokeApiToken: (id: string): Promise<void> =>
    apiDelete('/api/v1/api-tokens/{id}', { path: { id } }),

  // ── Forwarding (Settings ▸ Forwarding, ManageConfig) — the passive-data tee (ADR-034) ──
  /** List forwarding destinations (never the stored secret). */
  listForwardDestinations: (): Promise<ForwardDestination[]> =>
    apiGet('/api/v1/forwarding/destinations'),

  /** Create a forwarding destination. */
  createForwardDestination: (body: ForwardDestinationInput): Promise<{ id: string }> =>
    apiPost('/api/v1/forwarding/destinations', { body }),

  /** Update a forwarding destination; omitting `community` keeps the stored one. */
  updateForwardDestination: (id: string, body: ForwardDestinationInput): Promise<void> =>
    apiPut('/api/v1/forwarding/destinations/{id}', { path: { id }, body }),

  /** Delete a forwarding destination. */
  deleteForwardDestination: (id: string): Promise<void> =>
    apiDelete('/api/v1/forwarding/destinations/{id}', { path: { id } }),

  /** Send one synthetic message to a destination. A transport failure comes back as
   *  `{ delivered: false, error }` (HTTP 200) — the configuration is the caller's, not a server
   *  fault, and the error text is what the admin needs to fix it. */
  testForwardDestination: (id: string): Promise<ForwardTestResult> =>
    apiPost('/api/v1/forwarding/destinations/{id}/test', { path: { id } }),

  /** Live forwarding counters + any online poller that cannot supply original bytes. */
  forwardingStatus: (): Promise<ForwardStatus> => apiGet('/api/v1/forwarding/status'),

  // ── AI-assisted RCA (ADR-029) — provider config is ManageConfig, generation is AckAlerts ──
  /** The active provider's configuration plus the selectable vendors. Never the credential. */
  getLlmConfig: (): Promise<LlmConfigResponse> => apiGet('/api/v1/llm/config'),

  /** Create or replace the provider configuration. Omit `api_key` to keep the stored one. */
  saveLlmConfig: (body: LlmConfigInput): Promise<void> => apiPut('/api/v1/llm/config', { body }),

  /** Send one minimal prompt to the **saved** configuration. Ignores `enabled`, so a provider can
   *  be validated before it is switched on. A provider failure is `{ ok: false, error }` on 200. */
  testLlmProvider: (): Promise<LlmTestResult> => apiPost('/api/v1/llm/test'),

  /** Explain one incident. Serves the cached report for identical evidence unless `force`. */
  createRca: (body: RcaRequestInput): Promise<RcaReport> => apiPost('/api/v1/rca', { body }),
};
