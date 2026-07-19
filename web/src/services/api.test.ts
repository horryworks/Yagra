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

  it('fetches recent monitoring gaps from the store-and-forward endpoint', async () => {
    const spy = vi.fn().mockResolvedValue({
      ok: true,
      status: 200,
      json: async () => [
        {
          id: 'g1',
          poller_id: 'edge-1',
          pool: 'tokyo',
          started_at: '2026-07-14T00:00:00Z',
          ended_at: '2026-07-14T00:05:00Z',
          duration_secs: 300,
          recorded_at: '2026-07-14T00:05:01Z',
        },
      ],
    } as Response);
    globalThis.fetch = spy;
    const gaps = await api.listMonitoringGaps();
    expect(spy).toHaveBeenCalledWith('/api/v1/monitoring-gaps');
    expect(gaps).toHaveLength(1);
    expect(gaps[0].poller_id).toBe('edge-1');
    expect(gaps[0].duration_secs).toBe(300);
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
    mockFetch(200, {
      public_dashboard: false,
      auth_available: true,
      default_poll_interval_secs: 45,
    });
    const cfg = await api.getConfig();
    expect(cfg.public_dashboard).toBe(false);
    expect(cfg.auth_available).toBe(true);
    expect(cfg.default_poll_interval_secs).toBe(45);
  });

  it('puts the default poll interval to the config endpoint', async () => {
    const spy = vi
      .fn()
      .mockResolvedValue({ ok: true, status: 204, json: async () => ({}) } as Response);
    globalThis.fetch = spy;
    await api.updateConfig({ default_poll_interval_secs: 120 });
    const [url, init] = spy.mock.calls[0];
    expect(url).toBe('/api/v1/config');
    expect(init.method).toBe('PUT');
    expect(JSON.parse(init.body)).toEqual({ default_poll_interval_secs: 120 });
  });

  it('posts a profile with its poll-interval override in the body', async () => {
    const spy = vi
      .fn()
      .mockResolvedValue({ ok: true, status: 201, json: async () => ({ id: 'p1' }) } as Response);
    globalThis.fetch = spy;
    await api.createProfile({ name: 'Edge router', category: 'router', poll_interval_secs: 15 });
    const [url, init] = spy.mock.calls[0];
    expect(url).toBe('/api/v1/profiles');
    expect(init.method).toBe('POST');
    expect(JSON.parse(init.body)).toMatchObject({ name: 'Edge router', poll_interval_secs: 15 });
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

  it('sends the node-picker search term + cap on the search request (A-2)', async () => {
    const spy = vi
      .fn()
      .mockResolvedValue({ ok: true, status: 200, json: async () => [] } as Response);
    globalThis.fetch = spy;
    await api.searchNodes('tokyo', 25);
    expect(spy).toHaveBeenCalledWith('/api/v1/nodes/search?q=tokyo&limit=25');
  });

  it('omits an empty search term but still sends the cap (first page by name)', async () => {
    const spy = vi
      .fn()
      .mockResolvedValue({ ok: true, status: 200, json: async () => [] } as Response);
    globalThis.fetch = spy;
    await api.searchNodes('', 50);
    expect(spy).toHaveBeenCalledWith('/api/v1/nodes/search?limit=50');
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

  it('triggers an immediate poll via POST to the node poll endpoint', async () => {
    const spy = vi
      .fn()
      .mockResolvedValue({ ok: true, status: 202, json: async () => ({ dispatched: 3 }) } as Response);
    globalThis.fetch = spy;
    const res = await api.pollNode('n/1');
    const [url, init] = spy.mock.calls[0];
    expect(url).toBe('/api/v1/nodes/n%2F1/poll');
    expect(init.method).toBe('POST');
    expect(res.dispatched).toBe(3);
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

  it('creates a profile with its name, category and vendor', async () => {
    const spy = vi
      .fn()
      .mockResolvedValue({ ok: true, status: 201, json: async () => ({ id: 'p1' }) } as Response);
    globalThis.fetch = spy;
    await api.createProfile({ name: 'Cisco Nexus', category: 'l3-switch', vendor: 'Cisco' });
    const [url, init] = spy.mock.calls[0];
    expect(url).toBe('/api/v1/profiles');
    expect(init.method).toBe('POST');
    expect(JSON.parse(init.body)).toEqual({
      name: 'Cisco Nexus',
      category: 'l3-switch',
      vendor: 'Cisco',
    });
  });

  it('updates a profile via PUT to the id path', async () => {
    const spy = vi
      .fn()
      .mockResolvedValue({ ok: true, status: 204, json: async () => ({}) } as Response);
    globalThis.fetch = spy;
    await api.updateProfile('p1', { name: 'Edge', category: 'router', vendor: null });
    const [url, init] = spy.mock.calls[0];
    expect(url).toBe('/api/v1/profiles/p1');
    expect(init.method).toBe('PUT');
    expect(JSON.parse(init.body)).toEqual({ name: 'Edge', category: 'router', vendor: null });
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

  it('posts a notification channel with a tagged config body', async () => {
    const spy = vi
      .fn()
      .mockResolvedValue({ ok: true, status: 201, json: async () => ({ id: 'c1' }) } as Response);
    globalThis.fetch = spy;
    await api.createNotificationChannel({
      name: 'pagerduty',
      config: { kind: 'webhook', url: 'https://hooks.test/x' },
    });
    const [url, init] = spy.mock.calls[0];
    expect(url).toBe('/api/v1/notification-channels');
    expect(init.method).toBe('POST');
    expect(JSON.parse(init.body)).toMatchObject({
      name: 'pagerduty',
      config: { kind: 'webhook', url: 'https://hooks.test/x' },
    });
  });

  it('toggles a channel via PUT and deletes via the channel endpoint', async () => {
    const spy = vi
      .fn()
      .mockResolvedValue({ ok: true, status: 204, json: async () => ({}) } as Response);
    globalThis.fetch = spy;
    await api.setNotificationChannelEnabled('c1', false);
    expect(spy.mock.calls[0][0]).toBe('/api/v1/notification-channels/c1');
    expect(JSON.parse(spy.mock.calls[0][1].body)).toEqual({ enabled: false });
    await api.deleteNotificationChannel('c1');
    expect(spy.mock.calls[1][0]).toBe('/api/v1/notification-channels/c1');
    expect(spy.mock.calls[1][1].method).toBe('DELETE');
  });

  it('creates a routing rule with severity + channels', async () => {
    const spy = vi
      .fn()
      .mockResolvedValue({ ok: true, status: 201, json: async () => ({ id: 'r1' }) } as Response);
    globalThis.fetch = spy;
    await api.createRoutingRule({ name: 'crit', severity: 'critical', channel_ids: ['c1', 'c2'] });
    const [url, init] = spy.mock.calls[0];
    expect(url).toBe('/api/v1/routing-rules');
    expect(JSON.parse(init.body)).toEqual({
      name: 'crit',
      severity: 'critical',
      channel_ids: ['c1', 'c2'],
    });
  });

  it('lists the MIB catalog with an encoded search query', async () => {
    const spy = vi
      .fn()
      .mockResolvedValue({ ok: true, status: 200, json: async () => [] } as Response);
    globalThis.fetch = spy;
    await api.listMibCatalog('if hc');
    expect(spy).toHaveBeenCalledWith('/api/v1/mib-catalog?q=if%20hc');
    await api.listMibCatalog();
    expect(spy).toHaveBeenLastCalledWith('/api/v1/mib-catalog');
  });

  it('creates a maintenance window with the scope + RFC 3339 times', async () => {
    const spy = vi
      .fn()
      .mockResolvedValue({ ok: true, status: 201, json: async () => ({ id: 'w1' }) } as Response);
    globalThis.fetch = spy;
    await api.createMaintenanceWindow({
      name: 'fw upgrade',
      scope_level: 'node',
      scope_id: 'n1',
      starts_at: '2026-06-12T00:00:00.000Z',
      ends_at: '2026-06-12T02:00:00.000Z',
    });
    const [url, init] = spy.mock.calls[0];
    expect(url).toBe('/api/v1/maintenance-windows');
    expect(JSON.parse(init.body)).toEqual({
      name: 'fw upgrade',
      scope_level: 'node',
      scope_id: 'n1',
      starts_at: '2026-06-12T00:00:00.000Z',
      ends_at: '2026-06-12T02:00:00.000Z',
    });
  });

  it('toggles a maintenance window via PUT', async () => {
    const spy = vi
      .fn()
      .mockResolvedValue({ ok: true, status: 204, json: async () => undefined } as Response);
    globalThis.fetch = spy;
    await api.setMaintenanceWindowEnabled('w1', false);
    const [url, init] = spy.mock.calls[0];
    expect(url).toBe('/api/v1/maintenance-windows/w1');
    expect(init.method).toBe('PUT');
    expect(JSON.parse(init.body)).toEqual({ enabled: false });
  });

  it('creates a node mute, omitting the optional check/reason when empty', async () => {
    const spy = vi
      .fn()
      .mockResolvedValue({ ok: true, status: 201, json: async () => ({ id: 'm1' }) } as Response);
    globalThis.fetch = spy;
    await api.createMute({
      scope_kind: 'node',
      scope_id: 'n1',
      until: '2026-06-12T02:00:00.000Z',
    });
    const [url, init] = spy.mock.calls[0];
    expect(url).toBe('/api/v1/mutes');
    expect(JSON.parse(init.body)).toEqual({
      scope_kind: 'node',
      scope_id: 'n1',
      until: '2026-06-12T02:00:00.000Z',
    });
  });

  it('creates a folder-group mute', async () => {
    const spy = vi
      .fn()
      .mockResolvedValue({ ok: true, status: 201, json: async () => ({ id: 'm2' }) } as Response);
    globalThis.fetch = spy;
    await api.createMute({
      scope_kind: 'group',
      scope_id: 'g1',
      until: '2026-06-12T02:00:00.000Z',
    });
    const [url, init] = spy.mock.calls[0];
    expect(url).toBe('/api/v1/mutes');
    expect(JSON.parse(init.body)).toEqual({
      scope_kind: 'group',
      scope_id: 'g1',
      until: '2026-06-12T02:00:00.000Z',
    });
  });

  it('lifts a mute via DELETE', async () => {
    const spy = vi
      .fn()
      .mockResolvedValue({ ok: true, status: 204, json: async () => undefined } as Response);
    globalThis.fetch = spy;
    await api.deleteMute('m1');
    const [url, init] = spy.mock.calls[0];
    expect(url).toBe('/api/v1/mutes/m1');
    expect(init.method).toBe('DELETE');
  });

  it('builds the audit path with limit and the before cursor', async () => {
    const spy = vi
      .fn()
      .mockResolvedValue({ ok: true, status: 200, json: async () => [] } as Response);
    globalThis.fetch = spy;
    await api.listAudit({ limit: 100, before: '2026-06-12T00:00:00+00:00' });
    expect(spy).toHaveBeenCalledWith(
      '/api/v1/audit?limit=100&before=2026-06-12T00%3A00%3A00%2B00%3A00',
    );
    await api.listAudit();
    expect(spy).toHaveBeenLastCalledWith('/api/v1/audit');
  });

  it('starts a discovery scan with targets, communities and credential ids', async () => {
    const spy = vi
      .fn()
      .mockResolvedValue({ ok: true, status: 202, json: async () => ({ scan_id: 's1' }) } as Response);
    globalThis.fetch = spy;
    await api.startDiscoveryScan({
      targets: ['192.168.1.1', '192.168.1.2'],
      communities: ['public'],
      credential_ids: ['c1', 'c2'],
    });
    const [url, init] = spy.mock.calls[0];
    expect(url).toBe('/api/v1/discovery/scan');
    expect(JSON.parse(init.body)).toEqual({
      targets: ['192.168.1.1', '192.168.1.2'],
      communities: ['public'],
      credential_ids: ['c1', 'c2'],
    });
  });

  it('starts a discovery scan with stored credentials only (no ad-hoc communities)', async () => {
    const spy = vi
      .fn()
      .mockResolvedValue({ ok: true, status: 202, json: async () => ({ scan_id: 's2' }) } as Response);
    globalThis.fetch = spy;
    await api.startDiscoveryScan({
      targets: ['192.168.1.1'],
      credential_ids: ['c1'],
    });
    const [url, init] = spy.mock.calls[0];
    expect(url).toBe('/api/v1/discovery/scan');
    const body = JSON.parse(init.body);
    expect(body).toEqual({ targets: ['192.168.1.1'], credential_ids: ['c1'] });
    expect('communities' in body).toBe(false);
  });

  it('renames a credential (name only, secret left intact)', async () => {
    const spy = vi
      .fn()
      .mockResolvedValue({ ok: true, status: 204, json: async () => ({}) } as Response);
    globalThis.fetch = spy;
    await api.updateCredential('c1', { name: 'core-ro' });
    const [url, init] = spy.mock.calls[0];
    expect(url).toBe('/api/v1/credentials/c1');
    expect(init.method).toBe('PUT');
    expect(JSON.parse(init.body)).toEqual({ name: 'core-ro' });
  });

  it('replaces a credential secret (carries kind + secret)', async () => {
    const spy = vi
      .fn()
      .mockResolvedValue({ ok: true, status: 204, json: async () => ({}) } as Response);
    globalThis.fetch = spy;
    await api.updateCredential('c1', { name: 'core-ro', kind: 'snmp_v2c', secret: 'public2' });
    const [, init] = spy.mock.calls[0];
    expect(JSON.parse(init.body)).toEqual({
      name: 'core-ro',
      kind: 'snmp_v2c',
      secret: 'public2',
    });
  });

  it('creates a node with optional maker/model', async () => {
    const spy = vi
      .fn()
      .mockResolvedValue({ ok: true, status: 201, json: async () => ({ id: 'n1' }) } as Response);
    globalThis.fetch = spy;
    await api.createNode({ name: 'core-sw', address: '10.0.0.1', vendor: 'Cisco', model: 'C2960' });
    const [url, init] = spy.mock.calls[0];
    expect(url).toBe('/api/v1/nodes');
    expect(JSON.parse(init.body)).toEqual({
      name: 'core-sw',
      address: '10.0.0.1',
      vendor: 'Cisco',
      model: 'C2960',
    });
  });

  it('updates a node with maker/model alongside bindings', async () => {
    const spy = vi
      .fn()
      .mockResolvedValue({ ok: true, status: 204, json: async () => ({}) } as Response);
    globalThis.fetch = spy;
    await api.setNodeBindings('n1', {
      profile_id: 'p1',
      credential_id: null,
      vendor: 'Huawei',
      model: 'USG6000',
    });
    const [url, init] = spy.mock.calls[0];
    expect(url).toBe('/api/v1/nodes/n1/bindings');
    expect(init.method).toBe('PUT');
    expect(JSON.parse(init.body)).toEqual({
      profile_id: 'p1',
      credential_id: null,
      vendor: 'Huawei',
      model: 'USG6000',
    });
  });

  it('imports discovered nodes carrying maker/model', async () => {
    const spy = vi
      .fn()
      .mockResolvedValue({ ok: true, status: 201, json: async () => ({ created: 1 }) } as Response);
    globalThis.fetch = spy;
    await api.importDiscovered([
      { address: '10.0.0.1', name: 'fw', vendor: 'Huawei', model: 'USG6000' },
    ]);
    const [url, init] = spy.mock.calls[0];
    expect(url).toBe('/api/v1/discovery/import');
    expect(JSON.parse(init.body)).toEqual({
      nodes: [{ address: '10.0.0.1', name: 'fw', vendor: 'Huawei', model: 'USG6000' }],
    });
  });

  it('lists classification rules', async () => {
    const spy = vi
      .fn()
      .mockResolvedValue({ ok: true, status: 200, json: async () => [] } as Response);
    globalThis.fetch = spy;
    await api.listClassificationRules();
    expect(spy).toHaveBeenCalledWith('/api/v1/classification-rules');
  });

  it('creates a classification rule as a JSON body', async () => {
    const spy = vi
      .fn()
      .mockResolvedValue({ ok: true, status: 201, json: async () => ({ id: 'cr1' }) } as Response);
    globalThis.fetch = spy;
    await api.createClassificationRule({
      priority: 100,
      sysobjectid_prefix: '1.3.6.1.4.1.9.',
      sysdescr_regex: '(?i)cisco',
      profile_id: 'p1',
      enabled: true,
    });
    const [url, init] = spy.mock.calls[0];
    expect(url).toBe('/api/v1/classification-rules');
    expect(init.method).toBe('POST');
    expect(JSON.parse(init.body)).toMatchObject({
      priority: 100,
      sysobjectid_prefix: '1.3.6.1.4.1.9.',
      profile_id: 'p1',
      enabled: true,
    });
  });

  it('updates and deletes a classification rule via its id (url-encoded)', async () => {
    const spy = vi
      .fn()
      .mockResolvedValue({ ok: true, status: 204, json: async () => ({}) } as Response);
    globalThis.fetch = spy;
    await api.updateClassificationRule('r/1', {
      priority: 110,
      profile_id: 'p1',
      enabled: false,
    });
    expect(spy.mock.calls[0][0]).toBe('/api/v1/classification-rules/r%2F1');
    expect(spy.mock.calls[0][1].method).toBe('PUT');
    await api.deleteClassificationRule('r1');
    expect(spy.mock.calls[1][0]).toBe('/api/v1/classification-rules/r1');
    expect(spy.mock.calls[1][1].method).toBe('DELETE');
  });

  it('creates an event source and returns the once-shown token', async () => {
    const spy = vi.fn().mockResolvedValue({
      ok: true,
      status: 201,
      json: async () => ({ id: 'es1', token: 'abc123' }),
    } as Response);
    globalThis.fetch = spy;
    const res = await api.createEventSource({ name: 'Grafana' });
    const [url, init] = spy.mock.calls[0];
    expect(url).toBe('/api/v1/event-sources');
    expect(init.method).toBe('POST');
    expect(JSON.parse(init.body)).toMatchObject({ name: 'Grafana' });
    expect(res.token).toBe('abc123');
  });

  it('rotates an event source token via its id', async () => {
    const spy = vi.fn().mockResolvedValue({
      ok: true,
      status: 200,
      json: async () => ({ token: 'new-token' }),
    } as Response);
    globalThis.fetch = spy;
    await api.rotateEventSourceToken('es1');
    expect(spy.mock.calls[0][0]).toBe('/api/v1/event-sources/es1/rotate-token');
    expect(spy.mock.calls[0][1].method).toBe('POST');
  });

  it('oidcAuthorize fetches the IdP authorize URL', async () => {
    const spy = vi.fn().mockResolvedValue({
      ok: true,
      status: 200,
      json: async () => ({ authorize_url: 'https://idp.example.com/authorize?x=1' }),
    } as Response);
    globalThis.fetch = spy;
    const res = await api.oidcAuthorize();
    expect(spy.mock.calls[0][0]).toBe('/api/v1/auth/oidc/authorize');
    expect(res.authorize_url).toBe('https://idp.example.com/authorize?x=1');
  });

  it('oidcCallback exchanges code+state and stores the returned session token', async () => {
    const spy = vi.fn().mockResolvedValue({
      ok: true,
      status: 200,
      json: async () => ({ token: 'sso-tok', role: 'operator' }),
    } as Response);
    globalThis.fetch = spy;
    const res = await api.oidcCallback('the-code', 'the-state');
    const [url, init] = spy.mock.calls[0];
    expect(url).toBe('/api/v1/auth/oidc/callback');
    expect(init.method).toBe('POST');
    expect(JSON.parse(init.body)).toEqual({ code: 'the-code', state: 'the-state' });
    expect(res.role).toBe('operator');
    expect(getToken()).toBe('sso-tok');
    setToken(null);
  });

  it('creates an event rule as a tagged JSON body', async () => {
    const spy = vi
      .fn()
      .mockResolvedValue({ ok: true, status: 201, json: async () => ({ id: 'er1' }) } as Response);
    globalThis.fetch = spy;
    await api.createEventRule({
      name: 'link down',
      enabled: true,
      match_kind: 'substring',
      pattern: 'link down',
      clear_pattern: 'link up',
      severity: 'critical',
      ttl_secs: 1800,
      min_count: 1,
      window_secs: 60,
    });
    const [url, init] = spy.mock.calls[0];
    expect(url).toBe('/api/v1/event-rules');
    expect(init.method).toBe('POST');
    expect(JSON.parse(init.body)).toMatchObject({
      match_kind: 'substring',
      pattern: 'link down',
      severity: 'critical',
    });
  });

  it('tests an event rule pattern', async () => {
    const spy = vi.fn().mockResolvedValue({
      ok: true,
      status: 200,
      json: async () => ({ matched: true, clear_matched: false, error: null }),
    } as Response);
    globalThis.fetch = spy;
    const res = await api.testEventRule({
      match_kind: 'substring',
      pattern: 'link down',
      sample: 'link down on ge-0/0/1',
    });
    expect(spy.mock.calls[0][0]).toBe('/api/v1/event-rules/test');
    expect(res.matched).toBe(true);
  });

  it('builds the events path with filters and the before cursor', async () => {
    const spy = vi
      .fn()
      .mockResolvedValue({ ok: true, status: 200, json: async () => [] } as Response);
    globalThis.fetch = spy;
    await api.listEvents({ limit: 100, kind: 'syslog', matched: true });
    expect(spy).toHaveBeenCalledWith('/api/v1/events?limit=100&kind=syslog&matched=true');
    await api.listEvents();
    expect(spy).toHaveBeenLastCalledWith('/api/v1/events');
    // Free-text search rides along as `q`; an empty string is omitted.
    await api.listEvents({ q: 'link down' });
    expect(spy).toHaveBeenLastCalledWith('/api/v1/events?q=link+down');
    await api.listEvents({ q: '' });
    expect(spy).toHaveBeenLastCalledWith('/api/v1/events');
    // Regex mode and the time-range bounds ride along; false/absent are omitted.
    await api.listEvents({ q: '^%LINK', regex: true });
    expect(spy).toHaveBeenLastCalledWith('/api/v1/events?q=%5E%25LINK&regex=true');
    await api.listEvents({ q: 'x', regex: false });
    expect(spy).toHaveBeenLastCalledWith('/api/v1/events?q=x');
    await api.listEvents({
      start: '2024-01-01T00:00:00.000Z',
      end: '2024-01-02T00:00:00.000Z',
    });
    expect(spy).toHaveBeenLastCalledWith(
      '/api/v1/events?start=2024-01-01T00%3A00%3A00.000Z&end=2024-01-02T00%3A00%3A00.000Z',
    );
  });

  it('builds the flow endpoint paths with from/to and an optional limit (ADR-031)', async () => {
    const spy = vi
      .fn()
      .mockResolvedValue({ ok: true, status: 200, json: async () => [] } as Response);
    globalThis.fetch = spy;
    await api.getNodeFlowSeries('n1', { from: 100, to: 200 });
    expect(spy).toHaveBeenLastCalledWith('/api/v1/nodes/n1/flow/series?from=100&to=200');
    await api.getNodeFlowTopTalkers('n1', { from: 100, to: 200, limit: 10 });
    expect(spy).toHaveBeenLastCalledWith('/api/v1/nodes/n1/flow/top-talkers?from=100&to=200&limit=10');
    await api.getNodeFlowConversations('n1', { from: 100, to: 200, limit: 5 });
    expect(spy).toHaveBeenLastCalledWith('/api/v1/nodes/n1/flow/conversations?from=100&to=200&limit=5');
    await api.getNodeFlowTopPorts('n1', { from: 100, to: 200 });
    expect(spy).toHaveBeenLastCalledWith('/api/v1/nodes/n1/flow/top-ports?from=100&to=200');
    await api.getNodeFlowProtocols('n1', { from: 100, to: 200 });
    expect(spy).toHaveBeenLastCalledWith('/api/v1/nodes/n1/flow/protocols?from=100&to=200');
  });

  it('surfaces the flow-disabled 503 as a typed flow_unavailable error', async () => {
    mockFetch(503, {
      error: { code: 'flow_unavailable', message: 'flow monitoring is not enabled' },
    });
    await expect(api.getNodeFlowTopTalkers('n1', { from: 0, to: 1 })).rejects.toMatchObject({
      code: 'flow_unavailable',
      status: 503,
    });
  });

  it('creates a PagerDuty notification channel with a tagged config', async () => {
    const spy = vi
      .fn()
      .mockResolvedValue({ ok: true, status: 201, json: async () => ({ id: 'c1' }) } as Response);
    globalThis.fetch = spy;
    await api.createNotificationChannel({
      name: 'pd',
      config: { kind: 'pagerduty', routing_key: 'rk' },
    });
    const [url, init] = spy.mock.calls[0];
    expect(url).toBe('/api/v1/notification-channels');
    expect(JSON.parse(init.body)).toMatchObject({
      name: 'pd',
      config: { kind: 'pagerduty', routing_key: 'rk' },
    });
  });

  it('fetches the role/privilege matrix', async () => {
    const spy = vi.fn().mockResolvedValue({
      ok: true,
      status: 200,
      json: async () => ({
        permissions: [{ key: 'view', label: 'View', description: '…' }],
        roles: [{ key: 'admin', label: 'Admin', description: '…', builtin: true, permissions: ['view'] }],
      }),
    } as Response);
    globalThis.fetch = spy;
    const matrix = await api.listRoles();
    expect(spy).toHaveBeenCalledWith('/api/v1/roles');
    expect(matrix.roles[0].key).toBe('admin');
    expect(matrix.permissions[0].key).toBe('view');
  });

  it('creates a node group with a parent', async () => {
    const spy = vi
      .fn()
      .mockResolvedValue({ ok: true, status: 201, json: async () => ({ id: 'g2' }) } as Response);
    globalThis.fetch = spy;
    await api.createNodeGroup({ name: 'Rack A', group_type: 'device_type', parent_id: 'g1' });
    const [url, init] = spy.mock.calls[0];
    expect(url).toBe('/api/v1/node-groups');
    expect(init.method).toBe('POST');
    expect(JSON.parse(init.body)).toEqual({
      name: 'Rack A',
      group_type: 'device_type',
      parent_id: 'g1',
    });
  });

  it('moves a node into a group (and can ungroup with null)', async () => {
    const spy = vi
      .fn()
      .mockResolvedValue({ ok: true, status: 204, json: async () => ({}) } as Response);
    globalThis.fetch = spy;
    await api.setNodeGroup('n1', 'g2');
    expect(spy.mock.calls[0][0]).toBe('/api/v1/nodes/n1/group');
    expect(JSON.parse(spy.mock.calls[0][1].body)).toEqual({ group_id: 'g2' });

    await api.setNodeGroup('n1', null);
    expect(JSON.parse(spy.mock.calls[1][1].body)).toEqual({ group_id: null });
  });

  it("sets a node's dependency upstream (and can clear it with null)", async () => {
    const spy = vi
      .fn()
      .mockResolvedValue({ ok: true, status: 204, json: async () => ({}) } as Response);
    globalThis.fetch = spy;
    await api.setNodeParent('n1', 'n2');
    expect(spy.mock.calls[0][0]).toBe('/api/v1/nodes/n1/parent');
    expect(spy.mock.calls[0][1].method).toBe('PUT');
    expect(JSON.parse(spy.mock.calls[0][1].body)).toEqual({ parent_id: 'n2' });

    await api.setNodeParent('n1', null);
    expect(JSON.parse(spy.mock.calls[1][1].body)).toEqual({ parent_id: null });
  });

  it('drag-reorders a node before a sibling (placement)', async () => {
    const spy = vi
      .fn()
      .mockResolvedValue({ ok: true, status: 204, json: async () => ({}) } as Response);
    globalThis.fetch = spy;
    await api.placeNode('n1', { group_id: 'g2', before: 'n3' });
    expect(spy.mock.calls[0][0]).toBe('/api/v1/nodes/n1/placement');
    expect(spy.mock.calls[0][1].method).toBe('PUT');
    expect(JSON.parse(spy.mock.calls[0][1].body)).toEqual({ group_id: 'g2', before: 'n3' });
  });

  it('drag-reorders a group after a sibling (placement)', async () => {
    const spy = vi
      .fn()
      .mockResolvedValue({ ok: true, status: 204, json: async () => ({}) } as Response);
    globalThis.fetch = spy;
    await api.placeNodeGroup('g1', { parent_id: null, after: 'g2' });
    expect(spy.mock.calls[0][0]).toBe('/api/v1/node-groups/g1/placement');
    expect(spy.mock.calls[0][1].method).toBe('PUT');
    expect(JSON.parse(spy.mock.calls[0][1].body)).toEqual({ parent_id: null, after: 'g2' });
  });

  it('lists node groups', async () => {
    const spy = vi.fn().mockResolvedValue({
      ok: true,
      status: 200,
      json: async () => [{ id: 'g1', name: 'Tokyo', group_type: 'site', parent_id: null }],
    } as Response);
    globalThis.fetch = spy;
    const groups = await api.listNodeGroups();
    expect(spy).toHaveBeenCalledWith('/api/v1/node-groups');
    expect(groups[0].group_type).toBe('site');
  });

  it('builds the Top-N path with metric, agg and limit', async () => {
    const spy = vi
      .fn()
      .mockResolvedValue({ ok: true, status: 200, json: async () => [] } as Response);
    globalThis.fetch = spy;
    await api.getTopMetrics('icmp_rtt_ms', { agg: 'max_1h', limit: 10 });
    expect(spy).toHaveBeenCalledWith('/api/v1/metrics/top?metric=icmp_rtt_ms&agg=max_1h&limit=10');
  });

  it('defaults the Top-N path to just the metric', async () => {
    const spy = vi
      .fn()
      .mockResolvedValue({ ok: true, status: 200, json: async () => [] } as Response);
    globalThis.fetch = spy;
    await api.getTopMetrics('icmp_rtt_ms');
    expect(spy).toHaveBeenCalledWith('/api/v1/metrics/top?metric=icmp_rtt_ms');
  });

  it('builds the interface Top-N path with metric, agg and limit', async () => {
    const spy = vi
      .fn()
      .mockResolvedValue({ ok: true, status: 200, json: async () => [] } as Response);
    globalThis.fetch = spy;
    await api.getInterfaceTop('throughput', { agg: 'now', limit: 8 });
    expect(spy).toHaveBeenCalledWith(
      '/api/v1/metrics/interface-top?metric=throughput&agg=now&limit=8',
    );
    await api.getInterfaceTop('errors');
    expect(spy).toHaveBeenLastCalledWith('/api/v1/metrics/interface-top?metric=errors');
  });

  it('builds the interface-delta path (spikes/drops)', async () => {
    const spy = vi
      .fn()
      .mockResolvedValue({ ok: true, status: 200, json: async () => [] } as Response);
    globalThis.fetch = spy;
    await api.getInterfaceDelta('up', { window: 300, limit: 6 });
    expect(spy).toHaveBeenCalledWith('/api/v1/metrics/interface-delta?direction=up&window=300&limit=6');
    await api.getInterfaceDelta('down');
    expect(spy).toHaveBeenLastCalledWith('/api/v1/metrics/interface-delta?direction=down');
  });

  it('builds throughput-range and interface-heatmap paths', async () => {
    const spy = vi
      .fn()
      .mockResolvedValue({ ok: true, status: 200, json: async () => ({}) } as Response);
    globalThis.fetch = spy;
    await api.getThroughputRange();
    expect(spy).toHaveBeenCalledWith('/api/v1/metrics/throughput-range');
    await api.getInterfaceHeatmap({ limit: 8 });
    expect(spy).toHaveBeenLastCalledWith('/api/v1/metrics/interface-heatmap?limit=8');
  });

  it('requests poller health and discovery candidates', async () => {
    const spy = vi
      .fn()
      .mockResolvedValue({ ok: true, status: 200, json: async () => ({}) } as Response);
    globalThis.fetch = spy;
    await api.getPollerHealth();
    expect(spy).toHaveBeenCalledWith('/api/v1/poller-health');
    await api.getSystemHealth();
    expect(spy).toHaveBeenCalledWith('/api/v1/system-health');
    await api.getDiscoveryCandidates(10);
    expect(spy).toHaveBeenLastCalledWith('/api/v1/discovery/candidates?limit=10');
  });

  it('lists the poller fleet and decodes pollers + pools', async () => {
    const spy = vi.fn().mockResolvedValue({
      ok: true,
      status: 200,
      json: async () => ({
        pollers: [
          {
            id: 'edge-1',
            pool: 'tokyo',
            status: 'online',
            last_seen: '2026-07-06T01:00:00+00:00',
            first_seen: '2026-07-06T00:00:00+00:00',
            version: '0.1.2',
            working_set_nodes: 5,
            working_set_specs: 9,
            results_total: 42,
          },
        ],
        pools: [
          {
            pool: 'tokyo',
            nodes: 10,
            live_pollers: 1,
            mode: 'working_set',
            warning: null,
          },
        ],
      }),
    } as Response);
    globalThis.fetch = spy;
    const res = await api.listPollers();
    expect(spy).toHaveBeenCalledWith('/api/v1/pollers');
    expect(res.pollers[0].id).toBe('edge-1');
    expect(res.pollers[0].status).toBe('online');
    expect(res.pools[0].mode).toBe('working_set');
  });

  it('lists self-monitored hosts (core + pollers)', async () => {
    const spy = vi.fn().mockResolvedValue({
      ok: true,
      status: 200,
      json: async () => ({
        hosts: [
          {
            instance: 'core',
            role: 'core',
            pool: null,
            online: true,
            cpu_pct: 12.5,
            load1: 0.4,
            mem_used_bytes: 2,
            mem_total_bytes: 8,
            mem_used_pct: 25,
            disk_used_pct: 60,
            disks: [{ mount: 'root', used_bytes: 60, size_bytes: 100 }],
          },
        ],
      }),
    } as Response);
    globalThis.fetch = spy;
    const res = await api.getSystemHosts();
    expect(spy).toHaveBeenCalledWith('/api/v1/system/hosts');
    expect(res.hosts[0].instance).toBe('core');
    expect(res.hosts[0].disks[0].mount).toBe('root');
  });

  it('fetches a host metric range for an instance with the range params', async () => {
    const spy = vi.fn().mockResolvedValue({
      ok: true,
      status: 200,
      json: async () => ({
        instance: 'edge-1',
        cpu_pct: [{ t: 1, v: 10 }],
        load1: [],
        load5: [],
        load15: [],
        mem_used_bytes: [],
        mem_total_bytes: [],
        disks: [],
      }),
    } as Response);
    globalThis.fetch = spy;
    const res = await api.getHostMetricRange('edge-1', { from: 100, to: 200, step: 30 });
    expect(spy).toHaveBeenCalledWith(
      '/api/v1/system/hosts/edge-1/metrics/range?from=100&to=200&step=30',
    );
    expect(res.instance).toBe('edge-1');
    expect(res.cpu_pct[0].v).toBe(10);
  });

  it('deletes a poller via DELETE to its id (url-encoded)', async () => {
    const spy = vi
      .fn()
      .mockResolvedValue({ ok: true, status: 204, json: async () => undefined } as Response);
    globalThis.fetch = spy;
    await api.deletePoller('edge/1');
    const [url, init] = spy.mock.calls[0];
    expect(url).toBe('/api/v1/pollers/edge%2F1');
    expect(init.method).toBe('DELETE');
  });

  it('surfaces the online-poller guard as a typed 409 error', async () => {
    mockFetch(409, {
      error: { code: 'poller_online', message: 'poller is currently online; stop it before removing it' },
    });
    await expect(api.deletePoller('edge-1')).rejects.toMatchObject({
      code: 'poller_online',
      status: 409,
    });
  });

  it('requests the running core version', async () => {
    mockFetch(200, { core: '0.1.0' });
    const info = await api.getVersion();
    expect(info.core).toBe('0.1.0');
  });

  it('builds the alert aggregation paths', async () => {
    const spy = vi
      .fn()
      .mockResolvedValue({ ok: true, status: 200, json: async () => [] } as Response);
    globalThis.fetch = spy;
    await api.getAlertTopNodes({ window: 3600, limit: 5 });
    expect(spy).toHaveBeenCalledWith('/api/v1/alerts/top-nodes?window=3600&limit=5');
    await api.getAlertCalendar(7);
    expect(spy).toHaveBeenLastCalledWith('/api/v1/alerts/calendar?days=7');
    await api.getAlertTransitions(12);
    expect(spy).toHaveBeenLastCalledWith('/api/v1/alerts/transitions?limit=12');
  });

  it('reads the saved dashboard layout (null when none saved)', async () => {
    mockFetch(200, null);
    const layout = await api.getDashboard();
    expect(layout).toBeNull();
  });

  it('requests the dependency topology', async () => {
    const spy = vi
      .fn()
      .mockResolvedValue({ ok: true, status: 200, json: async () => ({ nodes: [] }) } as Response);
    globalThis.fetch = spy;
    await api.getTopology();
    expect(spy).toHaveBeenCalledWith('/api/v1/topology');
  });

  it('requests fleet coverage', async () => {
    const spy = vi.fn().mockResolvedValue({
      ok: true,
      status: 200,
      json: async () => ({ total: 0, fresh: 0, coverage_pct: 100, stale: [] }),
    } as Response);
    globalThis.fetch = spy;
    await api.getFleetCoverage();
    expect(spy).toHaveBeenCalledWith('/api/v1/fleet/coverage');
  });

  it('builds the state-history path (default + windowed)', async () => {
    const spy = vi.fn().mockResolvedValue({
      ok: true,
      status: 200,
      json: async () => ({ timestamps: [], series: {} }),
    } as Response);
    globalThis.fetch = spy;
    await api.getStateHistory();
    expect(spy).toHaveBeenCalledWith('/api/v1/fleet/state-history');
    await api.getStateHistory({ from: 100, to: 200 });
    expect(spy).toHaveBeenLastCalledWith('/api/v1/fleet/state-history?from=100&to=200');
  });

  it('saves the dashboard layout as a JSON body', async () => {
    const spy = vi
      .fn()
      .mockResolvedValue({ ok: true, status: 200, json: async () => ({ ok: true }) } as Response);
    globalThis.fetch = spy;
    await api.putDashboard({ version: 1, widgets: [{ instanceId: 'w1', type: 'health-ring', span: 4 }] });
    const [url, init] = spy.mock.calls[0];
    expect(url).toBe('/api/v1/dashboard');
    expect(init.method).toBe('PUT');
    expect(JSON.parse(init.body)).toEqual({
      version: 1,
      widgets: [{ instanceId: 'w1', type: 'health-ring', span: 4 }],
    });
  });

  it('reads the shared dashboard layout (null when none saved)', async () => {
    mockFetch(200, null);
    const layout = await api.getSharedDashboard();
    expect(layout).toBeNull();
  });

  it('saves the shared dashboard layout as a JSON body', async () => {
    const spy = vi
      .fn()
      .mockResolvedValue({ ok: true, status: 200, json: async () => ({ ok: true }) } as Response);
    globalThis.fetch = spy;
    const doc = {
      version: 2,
      boards: [{ id: 'b1', name: 'Dashboard 1', widgets: [{ instanceId: 'w1', type: 'health-ring', span: 4 }] }],
    };
    await api.putSharedDashboard(doc);
    const [url, init] = spy.mock.calls[0];
    expect(url).toBe('/api/v1/shared-dashboard');
    expect(init.method).toBe('PUT');
    expect(JSON.parse(init.body)).toEqual(doc);
  });

  it('clears a stale token and notifies on a 401 with a token attached', async () => {
    setToken('stale-token');
    const onUnauth = vi.fn();
    setUnauthorizedHandler(onUnauth);
    mockFetch(401, { error: { code: 'unauthorized', message: 'a valid bearer token is required' } });

    await expect(api.createProfile({ name: 'p1' })).rejects.toMatchObject({ status: 401 });
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

  // ── Cisco Meraki (read-only Dashboard API monitoring) ──────────────────────────────
  it('discovers Meraki orgs with the key + base url in the body', async () => {
    const spy = vi
      .fn()
      .mockResolvedValue({ ok: true, status: 200, json: async () => [] } as Response);
    globalThis.fetch = spy;
    await api.merakiDiscover({ api_key: 'k', base_url: 'https://api.meraki.com' });
    const [url, init] = spy.mock.calls[0];
    expect(url).toBe('/api/v1/meraki/orgs/discover');
    expect(init.method).toBe('POST');
    expect(JSON.parse(init.body)).toEqual({ api_key: 'k', base_url: 'https://api.meraki.com' });
  });

  it('creates Meraki orgs from a shared key + selected org ids', async () => {
    const spy = vi
      .fn()
      .mockResolvedValue({ ok: true, status: 201, json: async () => ({ created: 2 }) } as Response);
    globalThis.fetch = spy;
    await api.createMerakiOrgs({ api_key: 'k', org_ids: ['1', '2'] });
    const [url, init] = spy.mock.calls[0];
    expect(url).toBe('/api/v1/meraki/orgs');
    expect(init.method).toBe('POST');
    expect(JSON.parse(init.body)).toEqual({ api_key: 'k', org_ids: ['1', '2'] });
  });

  it('sets the network scope via PUT with the ids + monitored flag', async () => {
    const spy = vi
      .fn()
      .mockResolvedValue({ ok: true, status: 204, json: async () => ({}) } as Response);
    globalThis.fetch = spy;
    await api.setMerakiNetworksMonitored('org-1', ['N_1', 'N_2'], true);
    const [url, init] = spy.mock.calls[0];
    expect(url).toBe('/api/v1/meraki/orgs/org-1/networks');
    expect(init.method).toBe('PUT');
    expect(JSON.parse(init.body)).toEqual({ network_ids: ['N_1', 'N_2'], monitored: true });
  });

  it('enumerates an org via POST (no body)', async () => {
    const spy = vi.fn().mockResolvedValue({
      ok: true,
      status: 200,
      json: async () => ({ networks: [], devices: [] }),
    } as Response);
    globalThis.fetch = spy;
    await api.enumerateMerakiOrg('org-1');
    const [url, init] = spy.mock.calls[0];
    expect(url).toBe('/api/v1/meraki/orgs/org-1/enumerate');
    expect(init.method).toBe('POST');
  });

  it('imports devices with the org, scope, and selected devices', async () => {
    const spy = vi
      .fn()
      .mockResolvedValue({ ok: true, status: 201, json: async () => ({ imported: 1 }) } as Response);
    globalThis.fetch = spy;
    const devices = [
      {
        serial: 'Q2-A',
        name: 'edge',
        model: 'MX67',
        product_type: 'appliance',
        network_id: 'N_1',
        network_name: 'HQ',
        lan_ip: '10.0.0.1',
      },
    ];
    await api.importMerakiDevices({
      org_uuid: 'org-1',
      monitored_network_ids: ['N_1'],
      devices,
    });
    const [url, init] = spy.mock.calls[0];
    expect(url).toBe('/api/v1/meraki/import');
    expect(init.method).toBe('POST');
    expect(JSON.parse(init.body)).toEqual({
      org_uuid: 'org-1',
      monitored_network_ids: ['N_1'],
      devices,
    });
  });

  it('toggles the global Meraki kill switch', async () => {
    const spy = vi
      .fn()
      .mockResolvedValue({ ok: true, status: 204, json: async () => ({}) } as Response);
    globalThis.fetch = spy;
    await api.setMerakiPolling(false);
    const [url, init] = spy.mock.calls[0];
    expect(url).toBe('/api/v1/meraki/polling');
    expect(init.method).toBe('PUT');
    expect(JSON.parse(init.body)).toEqual({ enabled: false });
  });
});
