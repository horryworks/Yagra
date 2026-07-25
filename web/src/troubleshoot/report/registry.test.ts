// SPDX-License-Identifier: AGPL-3.0-only
import { describe, expect, it } from 'vitest';
import { TOOLS } from '../data';
import { REPORTS } from './registry';
import type { ControlKey } from './types';
import type { AnalysisFinding } from '../../types/api';

const CONTROL_KEYS: ControlKey[] = ['scope', 'window', 'baseline', 'sensitivity', 'depth'];

/** A finding shaped like the real API rows, for exercising `summary`/`csv` without a server. */
function finding(over: Partial<AnalysisFinding> = {}): AnalysisFinding {
  return {
    id: 'f1',
    score: 91,
    severity: 'crit',
    node_id: '11111111-1111-1111-1111-111111111111',
    node_name: 'edge-1',
    metric: 'icmp_rtt_ms',
    kind: 'spike',
    when_label: '2h ago',
    duration: 'ongoing',
    detail: {},
    ...over,
  };
}

describe('troubleshoot report registry', () => {
  it('every descriptor is internally consistent', () => {
    for (const [key, d] of Object.entries(REPORTS)) {
      expect(d, key).toBeDefined();
      if (!d) continue;
      // The map key and the descriptor must agree, else the route would render another tool's body.
      expect(d.tool, key).toBe(key);
      // Pinned so a body's i18n subtree can't silently drift from its tool id.
      expect(d.i18nKey, key).toBe(`report.${key}`);
      expect(d.controls.controls.length, key).toBeGreaterThan(0);
      for (const c of d.controls.controls) expect(CONTROL_KEYS, key).toContain(c);
      expect(d.controls.windows.length, key).toBeGreaterThan(0);
      // A tool showing a baseline control must offer baseline presets to choose from.
      if (d.controls.controls.includes('baseline')) expect(d.controls.baselines, key).toBeDefined();
      // Hidden controls still ride along in the POST body — defaults must be complete and sane.
      const def = d.controls.defaults;
      expect(def.windowSecs, key).toBeGreaterThan(0);
      expect(def.baselineSecs, key).toBeGreaterThan(0);
      expect(def.sensitivity, key).toBeGreaterThanOrEqual(1);
      expect(def.sensitivity, key).toBeLessThanOrEqual(5);
      expect(['quick', 'standard', 'exhaustive'], key).toContain(def.depth);
      expect(d.summary.length, key).toBeGreaterThanOrEqual(3);
      expect(d.summary.length, key).toBeLessThanOrEqual(5);
      expect(d.phaseKeys.length, key).toBeGreaterThanOrEqual(2);
      expect(d.csv.length, key).toBeGreaterThanOrEqual(4);
      expect(typeof d.Body, key).toBe('function');
    }
  });

  it('a tool advertises a report path exactly when it has a report body', () => {
    // The biconditional is what keeps each rollout increment shippable: a half-wired tool would
    // otherwise show a "View →" button that redirects straight back to the catalog.
    for (const tool of TOOLS) {
      expect(Boolean(REPORTS[tool.id]), tool.id).toBe(Boolean(tool.reportPath));
      if (tool.reportPath) expect(tool.reportPath, tool.id).toBe(`/troubleshoot/report/${tool.id}`);
    }
  });

  it('summary stats tolerate no findings and never count notice rows', () => {
    // `flow_tier_off()` emits a synthetic `kind: 'info'` row; the shell strips it, but a stat that
    // divided by a count would still blow up on an empty list.
    for (const [key, d] of Object.entries(REPORTS)) {
      if (!d) continue;
      for (const s of d.summary) {
        const empty = s.value([]);
        expect(typeof empty, `${key}/${s.labelKey}`).toBe('string');
        expect(empty, `${key}/${s.labelKey}`).not.toContain('NaN');
        expect(s.value([finding()]), `${key}/${s.labelKey}`).not.toContain('NaN');
      }
    }
  });

  it('csv cells render as strings for a well-formed finding', () => {
    for (const [key, d] of Object.entries(REPORTS)) {
      if (!d) continue;
      for (const c of d.csv) {
        expect(typeof c.cell(finding()), `${key}/${c.header}`).toBe('string');
      }
    }
  });
});
