// SPDX-License-Identifier: AGPL-3.0-only
// The filter columns behind the three Troubleshoot report bodies that still narrowed from their
// toolbar (ADR-053 Inc.7): `rule_gap`, `flow_scan` and `auth_probe`.
//
// **Only three of the fifteen bodies are here, and that is a decision rather than a stopping
// point (Inc.7 決定 J).** The other twelve narrow with `Chips`, and their chips are *tool-specific
// lenses* — `soon` / `mid` / `far` for time-to-exhaustion, `chronic` / `intermittent` for a flap's
// shape, `inverse` for a correlation's direction. Those choose what the report is showing, not a
// row attribute, and folding them into a generic column filter would make the control say something
// the report does not mean. The three collected here were different: their toolbars carried a plain
// text search and a plain closed-set select, which is precisely what the filter row is for.
//
// In a `.ts` because Vitest never executes a `.tsx` (testing.md): every judgement about *which rows
// survive* has to live where a test can reach it.
//
// ⚠️ **The factories deliberately carry no `Record<string, ColumnFilterSpec<T>>` return
// annotation.** Every older spec factory in the tree has one, and the price is that its screen then
// reads specs through an untyped index (`specs[c.key]`), so renaming a column key ships a column
// with no filter cell and nothing complains — the Inc.5 hazard, still live in `findingsQuery.ts`.
// `satisfies` applies the same constraint while keeping the literal keys, so `specs.sgi` is a
// compile error here and `specColumns()` still accepts the result. Copy this shape, not the old one.

import {
  specColumns,
  TEXT_MODES,
  type ColumnFilterSpec,
  type FilterableColumn,
} from '../../lib/columnFilter';
import { EVENT_KINDS, FINDING_SEVERITIES, type AnalysisFinding } from '../../types/api';
import { SCAN_PATTERNS, detailNum, detailStr, scanPattern, sevOf } from './format';
import type { TFunction } from 'i18next';

/** What every spec in this file narrows. Named once so the three `satisfies` clauses agree. */
type FindingSpecs = Record<string, ColumnFilterSpec<AnalysisFinding>>;

// ---------------------------------------------------------------------------
// Row accessors. Exported because the bodies render the same values they filter on — a body with
// its own copy would be one edit away from a filter that disagrees with the cell above it.

/** rule_gap: the event signature (a trap OID or a syslog app-name). */
export const gapSignature = (f: AnalysisFinding) => detailStr(f, 'signature') ?? f.metric;
/** rule_gap: the event SOURCE kind. ⚠️ From `detail`, never `finding.kind` — that is always the
 *  literal `'rule_gap'` and the two collide by name (`RuleGapBody`'s header says so). */
export const gapSource = (f: AnalysisFinding) => detailStr(f, 'kind') ?? 'unknown';
/** rule_gap: how many of these arrived with no rule matching. */
export const gapCount = (f: AnalysisFinding) => detailNum(f, 'count') ?? 0;

/** flow_scan: the scanning source address. */
export const scanSource = (f: AnalysisFinding) => detailStr(f, 'src') ?? '—';
/** flow_scan: distinct destinations touched. */
export const scanDst = (f: AnalysisFinding) => detailNum(f, 'distinct_dst') ?? 0;
/** flow_scan: distinct ports touched. */
export const scanPorts = (f: AnalysisFinding) => detailNum(f, 'distinct_ports') ?? 0;
/** flow_scan: flow records behind the finding. */
export const scanFlows = (f: AnalysisFinding) => detailNum(f, 'flows') ?? 0;
/** flow_scan: sweep vs probe, recomputed here exactly as the Rust does (`format.ts::scanPattern`). */
export const scanShape = (f: AnalysisFinding) => scanPattern(scanDst(f), scanPorts(f));

/** auth_probe: the source IP doing the authenticating. The node is the *target*, not the subject. */
export const probeSource = (f: AnalysisFinding) => detailStr(f, 'source_ip');
/** auth_probe: failed attempts from that source. */
export const probeCount = (f: AnalysisFinding) => detailNum(f, 'count') ?? 0;

// ---------------------------------------------------------------------------
// rule_gap — a work queue over signatures, rendered as a `DataTable`.

export function ruleGapFilters(t: TFunction) {
  return {
    src: {
      kind: 'enum',
      // The passive-event source vocabulary, not a set derived from the findings: a control whose
      // options appear and vanish with the data cannot be reasoned about, and a run that happens to
      // contain no traps should still let an operator ask for traps and be told "none".
      // Labelled with the bare token because that is exactly what the row's `<Badge>` renders.
      options: EVENT_KINDS.map((k) => ({ value: k, label: k })),
      readValue: gapSource,
      allLabel: t('report.rule_gap.filter.allSources'),
      counts: 'client',
    },
    sig: {
      kind: 'text',
      modes: TEXT_MODES,
      not: true,
      readText: (f) => [gapSignature(f)],
      containsSemantics: 'substring',
      placeholder: t('report.rule_gap.searchPlaceholder'),
    },
    count: {
      kind: 'number',
      readNumber: gapCount,
      min: 0,
      step: 1,
    },
    scope: {
      kind: 'text',
      modes: TEXT_MODES,
      not: true,
      // ⚠️ A fleet-wide signature has no node, and its cell renders the *localized* word — so the
      // filter has to match that same word, not the backend's literal `"fleet"`. Reading the row's
      // raw `node_name` here would mean typing what the screen shows returns nothing.
      readText: (f) => [f.node_id ? f.node_name : t('report.rule_gap.fleet')],
      containsSemantics: 'substring',
      placeholder: t('report.rule_gap.filter.scopePlaceholder'),
    },
  } satisfies FindingSpecs;
}

export function ruleGapColumns(t: TFunction): FilterableColumn<AnalysisFinding>[] {
  return specColumns(ruleGapFilters(t));
}

// ---------------------------------------------------------------------------
// flow_scan — one row per scanning source, rendered as a `DataTable`.

export function flowScanFilters(t: TFunction) {
  return {
    src: {
      kind: 'text',
      modes: TEXT_MODES,
      not: true,
      readText: (f) => [scanSource(f)],
      containsSemantics: 'substring',
      placeholder: t('report.flow_scan.searchPlaceholder'),
    },
    node: {
      kind: 'text',
      modes: TEXT_MODES,
      not: true,
      readText: (f) => [f.node_name],
      containsSemantics: 'substring',
      placeholder: t('report.flow_scan.filter.nodePlaceholder'),
    },
    dst: { kind: 'number', readNumber: scanDst, min: 0, step: 1 },
    ports: { kind: 'number', readNumber: scanPorts, min: 0, step: 1 },
    flows: { kind: 'number', readNumber: scanFlows, min: 0, step: 1 },
    pattern: {
      kind: 'enum',
      options: SCAN_PATTERNS.map((p) => ({
        value: p,
        label: t(`report.flow_scan.pattern.${p}`),
      })),
      // Derived, not stored: the backend ships the shape inside an English `duration` string, so the
      // filter reads the same recomputation the column renders.
      readValue: scanShape,
      allLabel: t('report.flow_scan.filter.allPatterns'),
      counts: 'client',
    },
    score: { kind: 'number', readNumber: (f) => f.score, min: 0, max: 100, step: 1 },
  } satisfies FindingSpecs;
}

export function flowScanColumns(t: TFunction): FilterableColumn<AnalysisFinding>[] {
  return specColumns(flowScanFilters(t));
}

// ---------------------------------------------------------------------------
// auth_probe — a card list with no header row, so its controls go in a `FilterBar` (決定 K).

export function authProbeFilters(t: TFunction) {
  return {
    source: {
      kind: 'text',
      modes: TEXT_MODES,
      not: true,
      // Triage here is usually by subnet ("is this all one management range?"), which is why NOT
      // matters more on this report than on most: "everything except our own jump hosts".
      readText: (f) => [probeSource(f)],
      containsSemantics: 'substring',
      placeholder: t('report.auth_probe.searchPlaceholder'),
    },
    severity: {
      kind: 'enum',
      // All three, where the chips this replaced offered `all` / `crit` / `warn` only. `info` was
      // unreachable as a *selection* even though findings carry it, and a set can now say
      // "crit and warn" — which the single-valued chip row could not.
      options: FINDING_SEVERITIES.map((s) => ({ value: s, label: t(`findings.severity.${s}`) })),
      readValue: sevOf,
      allLabel: t('report.auth_probe.filter.allSeverities'),
      counts: 'client',
    },
    count: { kind: 'number', readNumber: probeCount, min: 0, step: 1 },
  } satisfies FindingSpecs;
}

export function authProbeColumns(t: TFunction): FilterableColumn<AnalysisFinding>[] {
  return specColumns(authProbeFilters(t));
}

/** Plain-text names for the bar and the mobile sheet — a card list has no header above each
 *  control, so the name is part of the control (`FilterBar`). */
export function authProbeFilterLabels(t: TFunction): Record<string, string> {
  return {
    source: t('report.auth_probe.cols.source'),
    severity: t('report.auth_probe.cols.severity'),
    count: t('report.auth_probe.cols.failures'),
  };
}
