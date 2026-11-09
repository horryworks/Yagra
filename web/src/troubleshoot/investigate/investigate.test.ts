// SPDX-License-Identifier: AGPL-3.0-only
import { describe, expect, it } from 'vitest';
import type {
  Alert,
  FolderInvestigateReport,
  InvestigateEvidence,
  InvestigateReport,
} from '../../types/api';
import { INVESTIGATE_QUESTIONS, INVESTIGATE_VERDICTS } from '../../types/api';
import {
  QUESTION_CARDS,
  VERDICT_TONE,
  alertingNodeIds,
  folderCounts,
  folderRows,
  questionHref,
  targetFromParams,
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

describe('questions and targets', () => {
  it('has a card for every question, each at its own path', () => {
    const slugs = INVESTIGATE_QUESTIONS.map((q) => QUESTION_CARDS[q].slug);
    expect(new Set(slugs).size).toBe(slugs.length);
  });

  it('carries the target onto the question page', () => {
    expect(questionHref('running_out')).toBe('/troubleshoot/investigate/running-out');
    expect(questionHref('running_out', { kind: 'node', id: NODE })).toBe(
      `/troubleshoot/investigate/running-out?node=${NODE}`,
    );
    expect(questionHref('running_out', { kind: 'folder', id: 'g1' })).toBe(
      '/troubleshoot/investigate/running-out?group=g1',
    );
  });

  it('reads a node before a folder from the URL', () => {
    expect(targetFromParams(new URLSearchParams(`node=${NODE}&group=g1`))).toEqual({
      kind: 'node',
      id: NODE,
    });
    expect(targetFromParams(new URLSearchParams('group=g1'))).toEqual({ kind: 'folder', id: 'g1' });
    expect(targetFromParams(new URLSearchParams())).toBeNull();
  });
});

describe('a folder answer', () => {
  const judged = (id: string, verdicts: InvestigateReport['hypotheses']) => ({
    ...report,
    node_id: id,
    hypotheses: verdicts,
  });
  const folder: FolderInvestigateReport = {
    question: 'running_out',
    group_id: 'g1',
    from: 0,
    to: 1,
    nodes_total: 10,
    nodes_with_data: 9,
    candidates: 3,
    judged: 3,
    not_judged: 0,
    links_screened: true,
    nodes: [
      judged('a', report.hypotheses),
      judged('b', [{ hypothesis: 'disk', verdict: 'unknown', evidence: [] }]),
      judged('c', [{ hypothesis: 'cpu', verdict: 'miss', evidence: [evidence({})] }]),
    ],
  };

  it('lists only the hits, each with the series that made it', () => {
    const rows = folderRows(folder);
    expect(rows.map((r) => [r.nodeId, r.hypothesis])).toEqual([['a', 'memory']]);
    expect(rows[0].evidence?.verdict).toBe('hit');
  });

  it('counts each judged node once, by its worst answer', () => {
    expect(folderCounts(folder)).toEqual({ hit: 1, miss: 1, unknown: 1 });
  });
});

describe('nodes with an alert', () => {
  const alert = (node: string, severity: string, at: number, kind?: string) =>
    ({ node, severity, at_unix_ms: at, subject_kind: kind }) as unknown as Alert;

  it('puts the worst first, names each node once and leaves out a pool', () => {
    const ids = alertingNodeIds(
      [
        alert('n1', 'warning', 3),
        alert('n2', 'critical', 1),
        alert('n1', 'critical', 2),
        alert('pool:p', 'critical', 4, 'pool'),
      ],
      8,
    );
    expect(ids).toEqual(['n1', 'n2']);
  });

  it('stops at the limit', () => {
    const many = Array.from({ length: 5 }, (_, i) => alert(`n${i}`, 'warning', i));
    expect(alertingNodeIds(many, 2)).toHaveLength(2);
  });
});
