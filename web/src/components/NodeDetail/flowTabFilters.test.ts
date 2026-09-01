// SPDX-License-Identifier: AGPL-3.0-only
// Unit tests for the Flow tab's drill-down filters (ADR-053 Inc.8). No DOM — the tab itself is a
// `.tsx` and therefore unreachable, which is why every judgement lives in `flowTabFilters.ts`.

import { describe, expect, it } from 'vitest';
import {
  FLOW_FILTER_MAX,
  chartIgnoresFilters,
  flowFilterColumns,
  flowFilterLabels,
  flowFilters,
  flowQueryFilters,
  ipToken,
  toRows,
  toggleFlowValue,
} from './flowTabFilters';
import { formatBytes } from '../../lib/format';
import {
  activeFilterCount,
  defaultFilters,
  isAnyFiltered,
  normalizeValues,
  reservedKeyCollisions,
} from '../../lib/columnFilter';
import { summarize } from '../../lib/filterSummary';
import { buildPredicate } from '../../lib/filterPredicate';

const t = ((k: string) => k) as unknown as Parameters<typeof flowFilters>[0];
const cols = flowFilterColumns(t);
const specs = flowFilters(t);

describe('the flow drill-down bar', () => {
  it('starts empty and claims no reserved URL key', () => {
    expect(isAnyFiltered(cols, defaultFilters(cols))).toBe(false);
    expect(reservedKeyCollisions(cols)).toEqual([]);
  });

  it('names every control — a bar has no header above a cell to do it', () => {
    expect(Object.keys(flowFilterLabels(t)).sort()).toEqual(cols.map((c) => c.key).sort());
  });

  it('narrows nothing in the browser: every filter is a question for the server', () => {
    // The whole tab re-queries. A predicate that narrowed rows here would filter the ten
    // conversations ClickHouse already ranked — the failure this increment exists to avoid.
    const predicate = buildPredicate(cols, { proto: '6', port: '443', peer: '10.0.0.1' }, 0);
    expect(predicate(undefined as never)).toBe(true);
  });

  it('sends only the filters that are set', () => {
    expect(flowQueryFilters({ proto: '6,17', port: '', peer: '  ' })).toEqual({
      proto: '6,17',
      port: undefined,
      peer: undefined,
      asn: undefined,
    });
  });
});

describe('the port drill-down', () => {
  const port = specs.port;

  it('takes several exact ports', () => {
    expect(normalizeValues('80, 443,8080', port.parse, port.max)).toBe('80,443,8080');
  });

  it('refuses what is not a port, rather than matching it loosely', () => {
    // The store matches `dst_port` exactly. A substring filter would offer `80` → `8080`, which is
    // why this is not the `text` kind.
    for (const bad of ['not-a-port', '65536', '-1', '80abc', '', '0x50']) {
      expect(port.parse(bad), bad).toBeNull();
    }
    expect(port.parse('0')).toBe('0');
    expect(port.parse('65535')).toBe('65535');
  });

  it('drops duplicates and keeps the operator’s order', () => {
    expect(normalizeValues('443,80,443', port.parse, port.max)).toBe('443,80');
  });

  it('stops at the cap the API enforces', () => {
    const many = Array.from({ length: FLOW_FILTER_MAX + 3 }, (_, i) => String(i + 1)).join(',');
    expect(normalizeValues(many, port.parse, port.max).split(',')).toHaveLength(FLOW_FILTER_MAX);
  });

  it('reads back as the label the Top-ports card shows', () => {
    expect(summarize(port, '443')).toEqual({ kind: 'one', label: port.format('443') });
    expect(summarize(port, '443,80')).toEqual({
      kind: 'many',
      label: port.format('443'),
      more: 1,
    });
    expect(summarize(port, '')).toEqual({ kind: 'none' });
  });
});

describe('the peer drill-down', () => {
  it('accepts v4 and v6 and rejects what is not an address', () => {
    expect(ipToken('10.0.0.1')).toBe('10.0.0.1');
    expect(ipToken('2001:DB8::1')).toBe('2001:db8::1');
    expect(ipToken('::1')).toBe('::1');
    for (const bad of ["' OR 1=1 --", 'example.com', '10.0.0.256', '10.0.0', '', '1.2.3.4.5']) {
      expect(ipToken(bad), bad).toBeNull();
    }
  });

  it('takes two peers at once — the thing the single-valued box could not say', () => {
    const peer = specs.peer;
    expect(normalizeValues('10.0.0.1, 8.8.8.8', peer.parse, peer.max)).toBe('10.0.0.1,8.8.8.8');
  });
});

describe('the AS drill-down', () => {
  const asn = specs.asn;

  it('keeps 0 — it is the "unknown AS" bucket, not an absent filter', () => {
    // `0` is a row on the Top-AS card and clicking it must filter. A `|| undefined` anywhere on this
    // path would drop it, which is the same trap the Score filter documented.
    expect(asn.parse('0')).toBe('0');
    expect(normalizeValues('0', asn.parse, asn.max)).toBe('0');
    expect(summarize(asn, '0')).toEqual({ kind: 'one', label: 'flow.as.unknown' });
    expect(summarize(asn, '15169')).toEqual({ kind: 'one', label: 'AS15169' });
  });

  it('refuses a prefixed or malformed ASN', () => {
    for (const bad of ['AS15169', '-1', '4294967296', 'abc']) {
      expect(asn.parse(bad), bad).toBeNull();
    }
  });
});

describe('click-to-filter', () => {
  it('adds a second value rather than replacing the first', () => {
    // The old `toggleFilterValue` replaced: clicking a second talker silently dropped the first.
    let peer = toggleFlowValue('', '10.0.0.1', specs.peer);
    peer = toggleFlowValue(peer, '8.8.8.8', specs.peer);
    expect(peer).toBe('10.0.0.1,8.8.8.8');
  });

  it('removes a value when the active one is clicked again', () => {
    expect(toggleFlowValue('10.0.0.1,8.8.8.8', '10.0.0.1', specs.peer)).toBe('8.8.8.8');
    expect(toggleFlowValue('10.0.0.1', '10.0.0.1', specs.peer)).toBe('');
  });

  it('normalizes a clicked value the same way a typed one is', () => {
    // Otherwise `010` from a click and `10` from the keyboard would be two entries for one port —
    // both are the token `10`, and clicking the row again has to remove the one that is there.
    expect(toggleFlowValue('', '010', specs.port)).toBe('10');
    expect(toggleFlowValue('10', '010', specs.port)).toBe('');
    expect(toggleFlowValue('', ' 443 ', specs.port)).toBe('443');
  });

  it('toggles a protocol too, where the vocabulary is closed', () => {
    expect(toggleFlowValue('', '6', specs.proto)).toBe('6');
    expect(toggleFlowValue('6', '17', specs.proto)).toBe('6,17');
    expect(toggleFlowValue('6,17', '6', specs.proto)).toBe('17');
  });

  it('ignores a click carrying a value the column cannot hold', () => {
    expect(toggleFlowValue('443', 'not-a-port', specs.port)).toBe('443');
  });
});

describe('what the trend chart can answer', () => {
  it('says so when a filter the 5-minute rollup cannot apply is set', () => {
    // The rollup carries only `proto`, so port/peer/AS narrow the tables and not the chart. Saying
    // nothing would leave the chart looking like it disagreed with everything below it.
    expect(chartIgnoresFilters({ proto: '6' })).toBe(false);
    expect(chartIgnoresFilters({ port: '443' })).toBe(true);
    expect(chartIgnoresFilters({ peer: '10.0.0.1' })).toBe(true);
    expect(chartIgnoresFilters({ asn: '0' })).toBe(true);
    expect(chartIgnoresFilters({})).toBe(false);
  });
});

describe('the clear-all count', () => {
  it('counts one narrowed column once, however many values it holds', () => {
    expect(activeFilterCount(cols, { proto: '6,17', port: '80,443' })).toBe(2);
    expect(activeFilterCount(cols, {})).toBe(0);
  });
});

describe('toRows', () => {
  it('turns any of the four drill-downs into byte-valued bars', () => {
    // Generic because the four (talkers, conversations, ports, ASes) agree on nothing except
    // "there is a label and there is a byte count".
    const rows = toRows(
      [
        { who: '10.0.0.1', bytes: 2048 },
        { who: '10.0.0.2', bytes: 1_500_000 },
      ],
      (x) => x.who,
      (x) => x.bytes,
    );
    expect(rows).toHaveLength(2);
    expect(rows[0].label).toBe('10.0.0.1');
    expect(rows[0].value).toBe(2048);
    expect(rows[0].valueText).toBe(formatBytes(2048));
    expect(rows[1].valueText).toBe(formatBytes(1_500_000));
  });

  it('renders nothing for an empty answer rather than throwing', () => {
    expect(toRows([], (x: { n: string }) => x.n, () => 0)).toEqual([]);
  });
});
