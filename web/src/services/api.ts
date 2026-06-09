// The single typed boundary to the Yagra-core northbound API (coding-conventions: never
// scatter raw fetch across components). All calls go through `api`; errors surface as a
// typed `ApiError` decoded from the fixed error envelope (ADR-019).

import type {
  Alert,
  ApiErrorBody,
  CredentialSummary,
  MetricRange,
  MetricReading,
  NodeSummary,
} from '../types/api';

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

export const api = {
  /** Latest reading for one node metric. */
  getNodeMetric: (nodeId: string, metric: string): Promise<MetricReading> =>
    request(`/nodes/${encodeURIComponent(nodeId)}/metrics/${encodeURIComponent(metric)}`),

  /** Time-series window for one node metric (defaults: last hour, 60s step). */
  getNodeMetricRange: (
    nodeId: string,
    metric: string,
    opts?: { from?: number; to?: number; step?: number },
  ): Promise<MetricRange> => {
    const params = new URLSearchParams();
    if (opts?.from != null) params.set('from', String(opts.from));
    if (opts?.to != null) params.set('to', String(opts.to));
    if (opts?.step != null) params.set('step', String(opts.step));
    const qs = params.toString();
    const path = `/nodes/${encodeURIComponent(nodeId)}/metrics/${encodeURIComponent(metric)}/range`;
    return request(qs ? `${path}?${qs}` : path);
  },

  /** Inventory listing (first page; the response is keyset-paginated). */
  listNodes: (): Promise<NodeSummary[]> =>
    request<{ nodes: NodeSummary[]; next_cursor: string | null }>('/nodes').then((r) => r.nodes),

  /** Create a node. */
  createNode: (body: { name: string; address: string; pool?: string }): Promise<{ id: string }> =>
    request('/nodes', jsonBody('POST', body)),

  /** Delete a node. */
  deleteNode: (id: string): Promise<void> =>
    request(`/nodes/${encodeURIComponent(id)}`, { method: 'DELETE' }),

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
