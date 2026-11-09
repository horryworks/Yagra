// SPDX-License-Identifier: AGPL-3.0-only
// Troubleshoot > Investigate (ADR-251): the judgement behind the page, out of the `.tsx` so a test
// can reach it. The verdicts themselves are the server's; this file only lays them out.

import type { Tone } from '../../components/ui/Badge';
import { interfaceDrill, nodeDrill } from '../../dashboard/widgets/drill';
import type {
  InvestigateEvidence,
  InvestigateHypothesis,
  InvestigateReport,
  InvestigateVerdict,
} from '../../types/api';

/** The page, and the key that names the node it is asked about. */
export const INVESTIGATE_PATH = '/troubleshoot/investigate';
export const NODE_PARAM = 'node';

/** The page asked about one node — what a node's own "Investigate" action opens. */
export function investigateHref(nodeId: string): string {
  return `${INVESTIGATE_PATH}?${new URLSearchParams({ [NODE_PARAM]: nodeId })}`;
}

/** A hit is something to act on, a miss is fine, unknown is neither. */
export const VERDICT_TONE: Record<InvestigateVerdict, Tone> = {
  hit: 'warning',
  miss: 'up',
  unknown: 'neutral',
};

/** One table row: a series behind a hypothesis, or the hypothesis alone when nothing on the node
 *  answers it. */
export interface InvestigateRow {
  key: string;
  hypothesis: InvestigateHypothesis;
  verdict: InvestigateVerdict;
  evidence: InvestigateEvidence | null;
}

/** Every hypothesis, in the server's order, each followed by its series. A hypothesis with no
 *  series still gets a row, so "this node has no disk metric" is on screen rather than absent. */
export function investigateRows(report: InvestigateReport): InvestigateRow[] {
  return report.hypotheses.flatMap((h): InvestigateRow[] =>
    h.evidence.length === 0
      ? [
          {
            key: h.hypothesis,
            hypothesis: h.hypothesis,
            verdict: h.verdict,
            evidence: null,
          },
        ]
      : h.evidence.map((e, i) => ({
          key: `${h.hypothesis}:${i}`,
          hypothesis: h.hypothesis,
          verdict: e.verdict,
          evidence: e,
        })),
  );
}

/** Where a series is drawn: a port opens its dock on the Interfaces tab, anything else the node's
 *  overview, where the health gauges are. */
export function seriesHref(nodeId: string, e: InvestigateEvidence): string {
  return e.ifindex != null ? interfaceDrill(nodeId, e.ifindex) : nodeDrill(nodeId);
}

/** The name a series goes by: the port's name, else the metric with its row. */
export function seriesName(e: InvestigateEvidence): string {
  if (e.ifindex != null) return e.label ?? `ifIndex ${e.ifindex}`;
  return e.row != null ? `${e.metric} #${e.row}` : e.metric;
}

/** A percentage, whole above ten and to one place below. */
export function formatPct(v: number | null | undefined): string {
  if (v == null || !Number.isFinite(v)) return '';
  return `${Math.abs(v) >= 10 ? Math.round(v) : v.toFixed(1)}%`;
}

/** Days to the target line: `<1` under a day, whole days otherwise. */
export function formatDays(v: number | null | undefined): string {
  if (v == null || !Number.isFinite(v)) return '';
  return v < 1 ? '<1' : String(Math.round(v));
}
