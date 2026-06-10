import { afterEach, describe, expect, it, vi } from 'vitest';
import { api, ApiError, getToken, setToken, setUnauthorizedHandler } from './api';

function mockFetch(status: number, body: unknown) {
  globalThis.fetch = vi.fn().mockResolvedValue({
    ok: status >= 200 && status < 300,
    status,
    json: async () => body,
  } as Response);
}

afterEach(() => {
  vi.restoreAllMocks();
});

describe('api client', () => {
  it('returns the parsed body on success', async () => {
    mockFetch(200, { node_id: 'n1', metric: 'icmp_rtt_ms', value: 8.0 });
    const reading = await api.getNodeMetric('n1', 'icmp_rtt_ms');
    expect(reading.value).toBe(8.0);
    expect(reading.metric).toBe('icmp_rtt_ms');
  });

  it('decodes the error envelope into a typed ApiError', async () => {
    mockFetch(404, { error: { code: 'metric_not_found', message: 'no reading' } });
    await expect(api.getNodeMetric('n1', 'missing')).rejects.toMatchObject({
      code: 'metric_not_found',
      status: 404,
    });
  });

  it('falls back to a generic error on a non-JSON failure body', async () => {
    globalThis.fetch = vi.fn().mockResolvedValue({
      ok: false,
      status: 500,
      json: async () => {
        throw new Error('not json');
      },
    } as unknown as Response);
    const err = await api.listAlerts().catch((e) => e);
    expect(err).toBeInstanceOf(ApiError);
    expect(err.code).toBe('http_error');
    expect(err.status).toBe(500);
  });

  it('url-encodes path parameters', async () => {
    const spy = vi.fn().mockResolvedValue({ ok: true, status: 200, json: async () => ({}) } as Response);
    globalThis.fetch = spy;
    await api.getNodeMetric('a/b', 'm m');
    expect(spy).toHaveBeenCalledWith('/api/v1/nodes/a%2Fb/metrics/m%20m');
  });

  it('builds the range path with query params', async () => {
    const spy = vi
      .fn()
      .mockResolvedValue({ ok: true, status: 200, json: async () => ({}) } as Response);
    globalThis.fetch = spy;
    await api.getNodeMetricRange('n1', 'icmp_rtt_ms', { from: 100, to: 200, step: 30 });
    expect(spy).toHaveBeenCalledWith(
      '/api/v1/nodes/n1/metrics/icmp_rtt_ms/range?from=100&to=200&step=30',
    );
  });

  it('omits the query string when no range options are given', async () => {
    const spy = vi
      .fn()
      .mockResolvedValue({ ok: true, status: 200, json: async () => ({}) } as Response);
    globalThis.fetch = spy;
    await api.getNodeMetricRange('n1', 'icmp_rtt_ms');
    expect(spy).toHaveBeenCalledWith('/api/v1/nodes/n1/metrics/icmp_rtt_ms/range');
  });

  it('fetches the public client config', async () => {
    mockFetch(200, { public_dashboard: false, auth_available: true });
    const cfg = await api.getConfig();
    expect(cfg.public_dashboard).toBe(false);
    expect(cfg.auth_available).toBe(true);
  });

  it('passes keyset cursor + limit on the node page request', async () => {
    const spy = vi
      .fn()
      .mockResolvedValue({ ok: true, status: 200, json: async () => ({ nodes: [], next_cursor: null }) } as Response);
    globalThis.fetch = spy;
    await api.listNodesPage({ cursor: 'abc', limit: 50 });
    expect(spy).toHaveBeenCalledWith('/api/v1/nodes?cursor=abc&limit=50');
  });

  it('omits the query string for the first node page', async () => {
    const spy = vi
      .fn()
      .mockResolvedValue({ ok: true, status: 200, json: async () => ({ nodes: [], next_cursor: null }) } as Response);
    globalThis.fetch = spy;
    await api.listNodesPage();
    expect(spy).toHaveBeenCalledWith('/api/v1/nodes');
  });

  it('posts a threshold rule as a JSON body', async () => {
    const spy = vi
      .fn()
      .mockResolvedValue({ ok: true, status: 201, json: async () => ({ id: 't1' }) } as Response);
    globalThis.fetch = spy;
    await api.createThreshold({
      scope_level: 'node',
      scope_id: 's1',
      metric: 'cpu_util',
      direction: 'above',
      warning: 70,
      critical: 90,
    });
    const [url, init] = spy.mock.calls[0];
    expect(url).toBe('/api/v1/thresholds');
    expect(init.method).toBe('POST');
    expect(JSON.parse(init.body)).toMatchObject({ scope_level: 'node', metric: 'cpu_util' });
  });

  it('requests the current principal from /auth/me', async () => {
    mockFetch(200, { role: 'Admin' });
    const me = await api.me();
    expect(me.role).toBe('Admin');
  });

  it('clears a stale token and notifies on a 401 with a token attached', async () => {
    setToken('stale-token');
    const onUnauth = vi.fn();
    setUnauthorizedHandler(onUnauth);
    mockFetch(401, { error: { code: 'unauthorized', message: 'a valid bearer token is required' } });

    await expect(api.createProfile('p1')).rejects.toMatchObject({ status: 401 });
    expect(getToken()).toBeNull();
    expect(onUnauth).toHaveBeenCalledOnce();

    setUnauthorizedHandler(null);
  });

  it('does not fire the unauthorized handler when no token was attached (e.g. bad login)', async () => {
    setToken(null);
    const onUnauth = vi.fn();
    setUnauthorizedHandler(onUnauth);
    mockFetch(401, { error: { code: 'invalid_credentials', message: 'bad' } });

    await expect(api.login('u', 'bad')).rejects.toMatchObject({ status: 401 });
    expect(onUnauth).not.toHaveBeenCalled();

    setUnauthorizedHandler(null);
  });
});
