import { afterEach, describe, expect, it, vi } from 'vitest';
import { api, ApiError } from './api';

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
});
