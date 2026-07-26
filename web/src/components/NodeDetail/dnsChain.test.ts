// SPDX-License-Identifier: AGPL-3.0-only
import { describe, expect, it } from 'vitest';
import { chainSummary, chainToRows, failureLabel, recordTypeLabel, recordValue } from './dnsChain';
import type { DnsChain, DnsHop } from '../../types/api';

function hop(name: string, ...answers: DnsHop['answers']): DnsHop {
  return { name, answers };
}

const cname = (target: string, ttl = 300) => ({ record: { kind: 'cname' as const, target }, ttl });
const a = (addr: string, ttl = 60) => ({ record: { kind: 'a' as const, addr }, ttl });
const aaaa = (addr: string, ttl = 60) => ({ record: { kind: 'aaaa' as const, addr }, ttl });

/** `horryworks.net → CNAME horry.net → A 10.1.2.3` — the shape from the feature request. */
function sampleChain(overrides: Partial<DnsChain> = {}): DnsChain {
  return {
    query: 'horryworks.net',
    record_type: 'A',
    resolver: '10.0.0.53:53',
    hops: [hop('horryworks.net', cname('horry.net')), hop('horry.net', a('10.1.2.3'))],
    failure: null,
    resolve_ms: 14,
    ...overrides,
  };
}

describe('chainToRows', () => {
  it('flattens a CNAME chain in walk order and marks the terminal hop', () => {
    const rows = chainToRows(sampleChain());
    expect(rows).toHaveLength(2);
    expect(rows[0]).toMatchObject({
      name: 'horryworks.net',
      edge: 'CNAME',
      values: ['horry.net'],
      ttl: 300,
      terminal: false,
    });
    expect(rows[1]).toMatchObject({
      name: 'horry.net',
      edge: 'A',
      values: ['10.1.2.3'],
      ttl: 60,
      terminal: true,
    });
  });

  it('handles a single-hop direct answer', () => {
    const rows = chainToRows(
      sampleChain({ query: 'horry.net', hops: [hop('horry.net', a('10.1.2.3'))] }),
    );
    expect(rows).toHaveLength(1);
    expect(rows[0].terminal).toBe(true);
  });

  it('keeps every address of a multi-record terminal hop, in the order given', () => {
    // The poller already canonicalized (sorted) these, so the UI must not re-order them.
    const rows = chainToRows(
      sampleChain({
        hops: [
          hop('horryworks.net', cname('horry.net')),
          hop('horry.net', a('9.9.9.9'), a('10.1.2.3'), a('10.1.2.4')),
        ],
      }),
    );
    expect(rows[1].values).toEqual(['9.9.9.9', '10.1.2.3', '10.1.2.4']);
  });

  it('handles a three-hop chain', () => {
    const rows = chainToRows(
      sampleChain({
        hops: [
          hop('a.example', cname('b.example')),
          hop('b.example', cname('c.example')),
          hop('c.example', a('10.0.0.1')),
        ],
      }),
    );
    expect(rows.map((r) => r.name)).toEqual(['a.example', 'b.example', 'c.example']);
    expect(rows.map((r) => r.terminal)).toEqual([false, false, true]);
  });

  it('returns no rows for a failure-only chain', () => {
    // NXDOMAIN on the first query: nothing was ever answered, so there is nothing to draw.
    expect(chainToRows(sampleChain({ hops: [], failure: { kind: 'nx_domain' } }))).toEqual([]);
  });

  it('keeps the hops of a partial chain that then failed', () => {
    const rows = chainToRows(
      sampleChain({
        hops: [hop('horryworks.net', cname('horry.net'))],
        failure: { kind: 'nx_domain' },
      }),
    );
    expect(rows).toHaveLength(1);
    expect(rows[0].values).toEqual(['horry.net']);
  });

  it('tolerates a hop with no answers', () => {
    const rows = chainToRows(sampleChain({ hops: [hop('horry.net')] }));
    expect(rows[0]).toMatchObject({ edge: null, values: [], ttl: null });
  });
});

describe('recordTypeLabel / recordValue', () => {
  it('labels and renders each modelled record kind', () => {
    expect(recordTypeLabel(cname('horry.net').record)).toBe('CNAME');
    expect(recordTypeLabel(a('10.1.2.3').record)).toBe('A');
    expect(recordTypeLabel(aaaa('2001:db8::1').record)).toBe('AAAA');
    expect(recordValue(cname('horry.net').record)).toBe('horry.net');
    expect(recordValue(a('10.1.2.3').record)).toBe('10.1.2.3');
    expect(recordValue(aaaa('2001:db8::1').record)).toBe('2001:db8::1');
  });
});

describe('failureLabel', () => {
  it('is null when the chain resolved', () => {
    expect(failureLabel(null)).toBeNull();
  });

  it('maps every failure variant to a key, carrying the discriminating detail', () => {
    // Each variant needs a JA/EN string, so a missing case here means a missing translation.
    expect(failureLabel({ kind: 'nx_domain' })).toEqual({
      key: 'overview.dnsFailure.nx_domain',
      values: {},
    });
    expect(failureLabel({ kind: 'no_data' })?.key).toBe('overview.dnsFailure.no_data');
    expect(failureLabel({ kind: 'serv_fail' })?.key).toBe('overview.dnsFailure.serv_fail');
    expect(failureLabel({ kind: 'refused' })?.key).toBe('overview.dnsFailure.refused');
    expect(failureLabel({ kind: 'timeout' })?.key).toBe('overview.dnsFailure.timeout');
    expect(failureLabel({ kind: 'malformed' })?.key).toBe('overview.dnsFailure.malformed');
    expect(failureLabel({ kind: 'other_rcode', rcode: 9 })).toEqual({
      key: 'overview.dnsFailure.other_rcode',
      values: { rcode: 9 },
    });
    expect(failureLabel({ kind: 'loop_detected', at: 'a.example' })).toEqual({
      key: 'overview.dnsFailure.loop_detected',
      values: { at: 'a.example' },
    });
    expect(failureLabel({ kind: 'depth_exceeded', max_depth: 8 })).toEqual({
      key: 'overview.dnsFailure.depth_exceeded',
      values: { maxDepth: 8 },
    });
  });
});

describe('chainSummary', () => {
  it('reads like the dig answer it represents', () => {
    expect(chainSummary(sampleChain())).toBe('horryworks.net → horry.net → 10.1.2.3');
  });

  it('shows how far a failed chain got, then why it stopped', () => {
    expect(
      chainSummary(
        sampleChain({
          hops: [hop('horryworks.net', cname('horry.net'))],
          failure: { kind: 'nx_domain' },
        }),
      ),
    ).toBe('horryworks.net → horry.net (nx_domain)');
  });

  it('degrades to just the query when nothing was answered', () => {
    expect(chainSummary(sampleChain({ hops: [], failure: { kind: 'timeout' } }))).toBe(
      'horryworks.net (timeout)',
    );
  });
});
