// SPDX-License-Identifier: AGPL-3.0-only
// Troubleshoot > Investigate (ADR-251): the judgement behind the page, out of the `.tsx` so a test
// can reach it. The verdicts themselves are the server's; this file only lays them out.

import type { Tone } from '../../components/ui/Badge';
import { interfaceDrill, nodeDrill } from '../../dashboard/widgets/drill';
import { alertSubject } from '../../lib/alertSubject';
import { sortedAlerts } from '../../store';
import {
  INVESTIGATE_HYPOTHESES,
  type Alert,
  type FolderInvestigateReport,
  type InvestigateEvidence,
  type InvestigateHypothesis,
  type InvestigateQuestion,
  type InvestigateReport,
  type InvestigateVerdict,
} from '../../types/api';

/** The question cards, and the keys that name what a question is asked about. */
export const INVESTIGATE_PATH = '/troubleshoot/investigate';
export const NODE_PARAM = 'node';
export const GROUP_PARAM = 'group';

/** What a question can be asked about. */
export const TARGET_KINDS = ['node', 'folder'] as const;
export type TargetKind = (typeof TARGET_KINDS)[number];
export type Target = { kind: 'node'; id: string } | { kind: 'folder'; id: string };

/** One question's card on the Investigate page (ADR-251 decision 10). */
export interface QuestionCard {
  /** The question page's path segment under `INVESTIGATE_PATH`. */
  slug: string;
  /** The two letters on the card's tile. */
  mono: string;
  /** What it checks, in the order the answer lists them. */
  hypotheses: readonly InvestigateHypothesis[];
  /** What it can be asked about. */
  targets: readonly TargetKind[];
}

/** Every question's card. A `Record` so a new question cannot ship without one. */
export const QUESTION_CARDS: Record<InvestigateQuestion, QuestionCard> = {
  running_out: {
    slug: 'running-out',
    mono: 'Ro',
    hypotheses: INVESTIGATE_HYPOTHESES,
    targets: ['node', 'folder'],
  },
};

/** The cards, asked about one node — what a node's own "Investigate" action opens. */
export function investigateHref(nodeId: string): string {
  return `${INVESTIGATE_PATH}?${new URLSearchParams({ [NODE_PARAM]: nodeId })}`;
}

/** A question's page, asked about `target` when there is one and the question takes its kind. */
export function questionHref(q: InvestigateQuestion, target?: Target | null): string {
  const card = QUESTION_CARDS[q];
  const path = `${INVESTIGATE_PATH}/${card.slug}`;
  if (!target || !card.targets.includes(target.kind)) return path;
  return `${path}?${new URLSearchParams({ [targetParam(target.kind)]: target.id })}`;
}

/** The URL key that holds a target of `kind`. */
export function targetParam(kind: TargetKind): string {
  return kind === 'node' ? NODE_PARAM : GROUP_PARAM;
}

/** What the URL asks about: a node wins when both are set, since a node is the narrower ask. */
export function targetFromParams(params: URLSearchParams): Target | null {
  const node = params.get(NODE_PARAM);
  if (node) return { kind: 'node', id: node };
  const group = params.get(GROUP_PARAM);
  if (group) return { kind: 'folder', id: group };
  return null;
}

/** One row of a folder answer: a judged node's hit, with the series that made it. */
export interface FolderRow {
  key: string;
  nodeId: string;
  hypothesis: InvestigateHypothesis;
  evidence: InvestigateEvidence | null;
}

/** Every hit in a folder answer, in the server's order (hits first, busiest first). A node with
 *  no hit is only counted ({@link folderCounts}). */
export function folderRows(report: FolderInvestigateReport): FolderRow[] {
  return report.nodes.flatMap((n) =>
    n.hypotheses
      .filter((h) => h.verdict === 'hit')
      .map((h) => ({
        key: `${n.node_id}:${h.hypothesis}`,
        nodeId: n.node_id,
        hypothesis: h.hypothesis,
        evidence: h.evidence.find((e) => e.verdict === 'hit') ?? null,
      })),
  );
}

/** How the judged nodes split: with a hit, with nothing but unknowns among the rest, and clear. */
export function folderCounts(report: FolderInvestigateReport): Record<InvestigateVerdict, number> {
  const out: Record<InvestigateVerdict, number> = { hit: 0, miss: 0, unknown: 0 };
  for (const n of report.nodes) {
    const vs = n.hypotheses.map((h) => h.verdict);
    if (vs.includes('hit')) out.hit += 1;
    else if (vs.includes('unknown')) out.unknown += 1;
    else out.miss += 1;
  }
  return out;
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

/** The nodes with an open alert, worst alert first, each once, at most `max`. Alerts about a pool
 *  or a Meraki organization name no node and are left out. */
export function alertingNodeIds(alerts: readonly Alert[], max: number): string[] {
  const out: string[] = [];
  const byKey = Object.fromEntries(alerts.map((a, i) => [String(i), a]));
  for (const a of sortedAlerts(byKey)) {
    const s = alertSubject(a);
    if (s.kind !== 'node' || !s.nodeId || out.includes(s.nodeId)) continue;
    out.push(s.nodeId);
    if (out.length >= max) break;
  }
  return out;
}
