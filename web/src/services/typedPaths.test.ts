// SPDX-License-Identifier: AGPL-3.0-only
import { describe, expect, it } from 'vitest';
import { buildUrl } from './typedPaths';

describe('buildUrl', () => {
  it('substitutes path parameters with each segment percent-encoded', () => {
    // A slash inside an id must not become a new path segment — that would address a different
    // endpoint entirely, and the server would answer about the wrong resource rather than 404.
    expect(
      buildUrl('/api/v1/nodes/{node_id}/metrics/{metric}', {
        path: { node_id: 'a/b', metric: 'm m' },
      }),
    ).toBe('/api/v1/nodes/a%2Fb/metrics/m%20m');
  });

  it('omits the query string entirely when nothing is set', () => {
    // A bare `?` is a different URL from no query at all, and the request assertions in
    // api.test.ts pin the exact string.
    expect(buildUrl('/api/v1/nodes', { query: {} })).toBe('/api/v1/nodes');
    expect(buildUrl('/api/v1/nodes')).toBe('/api/v1/nodes');
  });

  it('drops null and undefined query values but keeps false and empty string', () => {
    // An unset filter must produce no parameter; whether an *empty* filter counts as unset is the
    // endpoint wrapper's decision, so this layer must not silently make it for them.
    expect(buildUrl('/api/v1/events', { query: { q: undefined, kind: null, regex: false } })).toBe(
      '/api/v1/events?regex=false',
    );
    expect(buildUrl('/api/v1/events', { query: { q: '' } })).toBe('/api/v1/events?q=');
  });

  it('encodes query values as a form-encoded string', () => {
    // `+` for a space and `%3A` for a colon: URLSearchParams, not encodeURIComponent. The two
    // disagree, and the difference is visible to any operator reading their proxy logs.
    expect(buildUrl('/api/v1/events', { query: { q: 'link down' } })).toBe(
      '/api/v1/events?q=link+down',
    );
    expect(buildUrl('/api/v1/audit', { query: { before: '2026-06-12T00:00:00+00:00' } })).toBe(
      '/api/v1/audit?before=2026-06-12T00%3A00%3A00%2B00%3A00',
    );
  });

  it('preserves the order the caller wrote the query in', () => {
    // Not a correctness property of the API, but the request assertions compare whole URLs, so a
    // reordering here would read as dozens of unrelated test failures.
    expect(buildUrl('/api/v1/metrics/top', { query: { metric: 'icmp_rtt_ms', limit: 10 } })).toBe(
      '/api/v1/metrics/top?metric=icmp_rtt_ms&limit=10',
    );
  });
});
