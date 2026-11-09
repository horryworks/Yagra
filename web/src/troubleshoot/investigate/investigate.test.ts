// SPDX-License-Identifier: AGPL-3.0-only
import { describe, expect, it } from 'vitest';
import type { InvestigateEvidence, InvestigateReport } from '../../types/api';
import { INVESTIGATE_VERDICTS } from '../../types/api';
import {
  VERDICT_TONE,
  formatDays,
  formatPct,
  investigateHref,
  investigateRows,
  seriesHref,
  seriesName,
} from './investigate';

const NODE = '00000000-0000-0000-0000-000000000001';

function evidence(over: Partial<InvestigateEvidence>): InvestigateEvidence {
  return {
    metric: 'huawei_mem_usage',
    verdict: 'miss',
    reason: 'not_reaching',
    current: 40,
    target: 100,
    slope_per_day: null,
    days_to_target: null,
    ...over,
  };
}

const report: InvestigateReport = {
  question: 'running_out',
  node_id: NODE,
  from: 0,
  to: 1,
  monitoring_gaps: [],
  hypotheses: [
    {
      hypothesis: 'memory',
      verdict: 'hit',
      evidence: [evidence({ verdict: 'hit', reason: 'leak' }), evidence({})],
    },
    { hypothesis: 'disk', verdict: 'unknown', evidence: [] },
  ],
};

describe('investigateRows', () => {
  it('lists every series and keeps a hypothesis nothing answers', () => {
    const rows = investigateRows(report);
    expect(rows.map((r) => [r.hypothesis, r.verdict])).toEqual([
      ['memory', 'hit'],
      ['memory', 'miss'],
      ['disk', 'unknown'],
    ]);
    expect(rows[2].evidence).toBeNull();
    expect(new Set(rows.map((r) => r.key)).size).toBe(rows.length);
  });
});

describe('links and names', () => {
  it('opens the asked node', () => {
    expect(investigateHref(NODE)).toBe(`/troubleshoot/investigate?node=${NODE}`);
  });

  it('sends a port to its dock and anything else to the node', () => {
    expect(seriesHref(NODE, evidence({ ifindex: 7 }))).toContain('interfaces.port=7');
    expect(seriesHref(NODE, evidence({}))).toBe(`/nodes/${NODE}`);
  });

  it('names a port by its name and a row by its metric', () => {
    expect(seriesName(evidence({ ifindex: 7, label: 'ge-0/0/7' }))).toBe('ge-0/0/7');
    expect(seriesName(evidence({ ifindex: 7 }))).toBe('ifIndex 7');
    expect(seriesName(evidence({ metric: 'hr_storage_used_pct', row: 3 }))).toBe(
      'hr_storage_used_pct #3',
    );
  });
});

describe('formatting', () => {
  it('rounds a percentage above ten and keeps a place below', () => {
    expect(formatPct(83.6)).toBe('84%');
    expect(formatPct(4.25)).toBe('4.3%');
    expect(formatPct(null)).toBe('');
  });

  it('says under a day rather than zero days', () => {
    expect(formatDays(0.3)).toBe('<1');
    expect(formatDays(19.6)).toBe('20');
    expect(formatDays(undefined)).toBe('');
  });

  it('has a tone for every verdict', () => {
    for (const v of INVESTIGATE_VERDICTS) expect(VERDICT_TONE[v]).toBeTruthy();
  });
});
