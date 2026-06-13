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

  it('creates a mute, omitting the optional check/reason when empty', async () => {
    const spy = vi
      .fn()
      .mockResolvedValue({ ok: true, status: 201, json: async () => ({ id: 'm1' }) } as Response);
    globalThis.fetch = spy;
    await api.createMute({ node_id: 'n1', until: '2026-06-12T02:00:00.000Z' });
    const [url, init] = spy.mock.calls[0];
    expect(url).toBe('/api/v1/mutes');
    expect(JSON.parse(init.body)).toEqual({ node_id: 'n1', until: '2026-06-12T02:00:00.000Z' });
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
