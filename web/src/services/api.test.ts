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

  it('passes the agg param on the latest metric read', async () => {
    const spy = vi
      .fn()
      .mockResolvedValue({ ok: true, status: 200, json: async () => ({}) } as Response);
    globalThis.fetch = spy;
    await api.getNodeMetric('n1', 'huawei_cpu_usage', { agg: 'max' });
    expect(spy).toHaveBeenCalledWith('/api/v1/nodes/n1/metrics/huawei_cpu_usage?agg=max');
  });

  it('omits the agg param on the latest read when not aggregating', async () => {
    const spy = vi
      .fn()
      .mockResolvedValue({ ok: true, status: 200, json: async () => ({}) } as Response);
    globalThis.fetch = spy;
    await api.getNodeMetric('n1', 'huawei_cpu_usage');
    expect(spy).toHaveBeenCalledWith('/api/v1/nodes/n1/metrics/huawei_cpu_usage');
  });

  it('builds the range path with the agg param', async () => {
    const spy = vi
      .fn()
      .mockResolvedValue({ ok: true, status: 200, json: async () => ({}) } as Response);
    globalThis.fetch = spy;
    await api.getNodeMetricRange('n1', 'huawei_mem_usage', { from: 100, to: 200, agg: 'max' });
    expect(spy).toHaveBeenCalledWith(
      '/api/v1/nodes/n1/metrics/huawei_mem_usage/range?from=100&to=200&agg=max',
    );
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

  it('posts a new user as a JSON body', async () => {
    const spy = vi
      .fn()
      .mockResolvedValue({ ok: true, status: 201, json: async () => ({ id: 'u1' }) } as Response);
    globalThis.fetch = spy;
    await api.createUser({ username: 'alice', password: 'hunter2hunter2', role: 'operator' });
    const [url, init] = spy.mock.calls[0];
    expect(url).toBe('/api/v1/users');
    expect(init.method).toBe('POST');
    expect(JSON.parse(init.body)).toMatchObject({ username: 'alice', role: 'operator' });
  });

  it('puts a role change to the user role endpoint', async () => {
    const spy = vi
      .fn()
      .mockResolvedValue({ ok: true, status: 204, json: async () => ({}) } as Response);
    globalThis.fetch = spy;
    await api.setUserRole('u/1', 'admin');
    const [url, init] = spy.mock.calls[0];
    expect(url).toBe('/api/v1/users/u%2F1/role');
    expect(init.method).toBe('PUT');
    expect(JSON.parse(init.body)).toEqual({ role: 'admin' });
  });

  it('deletes a user via the user endpoint', async () => {
    const spy = vi
      .fn()
      .mockResolvedValue({ ok: true, status: 204, json: async () => ({}) } as Response);
    globalThis.fetch = spy;
    await api.deleteUser('u1');
    const [url, init] = spy.mock.calls[0];
    expect(url).toBe('/api/v1/users/u1');
    expect(init.method).toBe('DELETE');
  });

  it('surfaces the last-admin guard as a typed error', async () => {
    mockFetch(409, { error: { code: 'last_admin', message: 'cannot remove the last admin' } });
    await expect(api.deleteUser('admin-id')).rejects.toMatchObject({
      code: 'last_admin',
      status: 409,
    });
  });

  it('requests the current principal from /auth/me', async () => {
    mockFetch(200, { role: 'Admin' });
    const me = await api.me();
    expect(me.role).toBe('Admin');
  });

  it('lists node interfaces', async () => {
    const spy = vi
      .fn()
      .mockResolvedValue({ ok: true, status: 200, json: async () => [] } as Response);
    globalThis.fetch = spy;
    await api.listNodeInterfaces('n1');
    expect(spy).toHaveBeenCalledWith('/api/v1/nodes/n1/interfaces');
  });

  it('requests the resolved node collection set with the query flag', async () => {
    const spy = vi
      .fn()
      .mockResolvedValue({ ok: true, status: 200, json: async () => [] } as Response);
    globalThis.fetch = spy;
    await api.listNodeCollection('n1', true);
    expect(spy).toHaveBeenCalledWith('/api/v1/nodes/n1/collection?resolved=true');
  });

  it('omits the resolved flag for the node-level collection set', async () => {
    const spy = vi
      .fn()
      .mockResolvedValue({ ok: true, status: 200, json: async () => [] } as Response);
    globalThis.fetch = spy;
    await api.listNodeCollection('n1');
    expect(spy).toHaveBeenCalledWith('/api/v1/nodes/n1/collection');
  });

  it('posts a node collection item as a JSON body', async () => {
    const spy = vi
      .fn()
      .mockResolvedValue({ ok: true, status: 201, json: async () => ({ id: 'c1' }) } as Response);
    globalThis.fetch = spy;
    await api.addNodeCollection('n1', {
      metric_name: 'if_hc_in_octets',
      oid: '1.3.6.1.2.1.31.1.1.1.6',
      collection: 'table',
      metric_kind: 'counter',
    });
    const [url, init] = spy.mock.calls[0];
    expect(url).toBe('/api/v1/nodes/n1/collection');
    expect(init.method).toBe('POST');
    expect(JSON.parse(init.body)).toMatchObject({ metric_name: 'if_hc_in_octets', collection: 'table' });
  });

  it('deletes a collection item by id (url-encoded)', async () => {
    const spy = vi
      .fn()
      .mockResolvedValue({ ok: true, status: 204, json: async () => ({}) } as Response);
    globalThis.fetch = spy;
    await api.deleteCollectionItem('c/1');
    const [url, init] = spy.mock.calls[0];
    expect(url).toBe('/api/v1/collection/c%2F1');
    expect(init.method).toBe('DELETE');
  });

  it('builds the interface-series path with query params', async () => {
    const spy = vi
      .fn()
      .mockResolvedValue({ ok: true, status: 200, json: async () => ({}) } as Response);
    globalThis.fetch = spy;
    await api.getInterfaceSeries('n1', 3, { from: 100, to: 200 });
    expect(spy).toHaveBeenCalledWith('/api/v1/nodes/n1/interfaces/3/series?from=100&to=200');
  });

  it('omits the query string for the interface series when no opts given', async () => {
    const spy = vi
      .fn()
      .mockResolvedValue({ ok: true, status: 200, json: async () => ({}) } as Response);
    globalThis.fetch = spy;
    await api.getInterfaceSeries('n1', 3);
    expect(spy).toHaveBeenCalledWith('/api/v1/nodes/n1/interfaces/3/series');
  });

  it('lists collection templates', async () => {
    const spy = vi
      .fn()
      .mockResolvedValue({ ok: true, status: 200, json: async () => [] } as Response);
    globalThis.fetch = spy;
    await api.listCollectionTemplates();
    expect(spy).toHaveBeenCalledWith('/api/v1/collection-templates');
  });

  it('creates a collection template as a JSON body', async () => {
    const spy = vi
      .fn()
      .mockResolvedValue({ ok: true, status: 201, json: async () => ({ id: 't1' }) } as Response);
    globalThis.fetch = spy;
    await api.createCollectionTemplate({ name: 'Standard interfaces', description: 'ifTable' });
    const [url, init] = spy.mock.calls[0];
    expect(url).toBe('/api/v1/collection-templates');
    expect(init.method).toBe('POST');
    expect(JSON.parse(init.body)).toMatchObject({ name: 'Standard interfaces' });
  });

  it('posts a template item to the nested items path', async () => {
    const spy = vi
      .fn()
      .mockResolvedValue({ ok: true, status: 201, json: async () => ({ id: 'i1' }) } as Response);
    globalThis.fetch = spy;
    await api.addTemplateItem('t1', {
      metric_name: 'if_hc_in_octets',
      oid: '1.3.6.1.2.1.31.1.1.1.6',
      collection: 'table',
      metric_kind: 'counter',
    });
    const [url, init] = spy.mock.calls[0];
    expect(url).toBe('/api/v1/collection-templates/t1/items');
    expect(init.method).toBe('POST');
  });

  it('deletes a template item via the nested path (url-encoded)', async () => {
    const spy = vi
      .fn()
      .mockResolvedValue({ ok: true, status: 204, json: async () => ({}) } as Response);
    globalThis.fetch = spy;
    await api.deleteTemplateItem('t/1', 'i/2');
    const [url, init] = spy.mock.calls[0];
    expect(url).toBe('/api/v1/collection-templates/t%2F1/items/i%2F2');
    expect(init.method).toBe('DELETE');
  });

  it('replaces a profile’s templates with a PUT body', async () => {
    const spy = vi
      .fn()
      .mockResolvedValue({ ok: true, status: 204, json: async () => ({}) } as Response);
    globalThis.fetch = spy;
    await api.setProfileTemplates('p1', ['t1', 't2']);
    const [url, init] = spy.mock.calls[0];
    expect(url).toBe('/api/v1/profiles/p1/templates');
    expect(init.method).toBe('PUT');
    expect(JSON.parse(init.body)).toEqual({ template_ids: ['t1', 't2'] });
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
