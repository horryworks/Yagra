// SPDX-License-Identifier: AGPL-3.0-only
// Troubleshoot report screens — the per-tool descriptor contract (ADR-022).
//
// Every analysis tool gets its OWN report (1:1 with the scan), because each tool's real entity
// differs: `flow_scan`'s entity is a source IP (a whole job's findings can sit on one node),
// `event_flap`'s is a (rule, node) pair, `rule_gap`'s is a signature. A generic findings table would
// misreport all three. What is shared is only the *chrome* — SSE wiring, `?job=` resolution, the
// lifecycle states, the launch bar — which lives in `ReportShell`; everything tool-specific lives in
// a descriptor registered in `registry.tsx`.
//
// This module is deliberately **pure types, no JSX**, so `registry.test.ts` can import the contract
// under vitest's `environment: 'node'`.

import type { ReactNode } from 'react';
import type { AnalysisFinding, AnalysisJob, AnalysisToolKey } from '../../types/api';

/** Which launch controls a tool's config bar shows. The config bar is also the tool's launcher. */
export type ControlKey = 'scope' | 'window' | 'baseline' | 'sensitivity' | 'depth';

/** A window/baseline preset: seconds + an i18next key (resolved at render, never at module load). */
export interface PresetOption {
  secs: number;
  labelKey: string;
}

export interface ControlSpec {
  /** Rendered left→right in this order. */
  controls: ControlKey[];
  windows: PresetOption[];
  /** Only meaningful when `controls` includes `'baseline'`. */
  baselines?: PresetOption[];
  /**
   * Fills BOTH the visible controls' initial values AND the fields hidden from this tool's bar.
   * Hiding a control does not remove it from the POST body — the engines apply `.max()` floors to
   * `window_secs`/`baseline_secs`, so a missing default would silently produce different results
   * than the launch drawer for the same tool.
   */
  defaults: {
    windowSecs: number;
    baselineSecs: number;
    /** 1–5 slider position; `sigmaFor()` converts to σ. */
    sensitivity: number;
    depth: 'quick' | 'standard' | 'exhaustive';
  };
}

/** Live values of the config bar (the shell owns this state). */
export interface ControlState {
  scopeKind: 'all' | 'group' | 'node';
  scopeId: string | null;
  scopeLabel: string;
  windowSecs: number;
  baselineSecs: number;
  sensitivity: number;
  depth: 'quick' | 'standard' | 'exhaustive';
}

/**
 * One number in the summary strip. Tool-specific on purpose: `flow_scan` counts *sources*, not
 * nodes. `value` must tolerate an empty array (and never see notice rows — the shell strips them).
 */
export interface SummaryStat {
  /** i18next key under `report.common.*` or `report.<tool>.*`. */
  labelKey: string;
  value: (findings: AnalysisFinding[]) => string;
  tone?: 'crit' | 'warn';
  /** Emit the `.ts-res-sep` divider before this stat. */
  separatorBefore?: boolean;
}

export interface ReportBodyProps {
  job: AnalysisJob;
  /** Real findings only — the shell has already split out `kind: 'info'` notices. */
  findings: AnalysisFinding[];
}

/** One CSV column for the shared export (findings are flat; `detail` is destructured per tool). */
export interface CsvColumn {
  header: string;
  cell: (f: AnalysisFinding) => string;
}

export interface ReportDescriptor {
  tool: AnalysisToolKey;
  /** Always `report.${tool}` — asserted by `registry.test.ts` so key subtrees can't drift. */
  i18nKey: string;
  controls: ControlSpec;
  summary: SummaryStat[];
  /** Processing-card captions, worded for this tool's actual phases. */
  phaseKeys: string[];
  /** A component, NOT a config blob — this is where 1:1 lives. */
  Body: (props: ReportBodyProps) => ReactNode;
  csv: CsvColumn[];
}
