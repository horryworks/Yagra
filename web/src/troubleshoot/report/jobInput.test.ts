// SPDX-License-Identifier: AGPL-3.0-only
import { describe, expect, it } from 'vitest';
import { buildJobInput, initialControlState } from './jobInput';
import { REPORTS } from './registry';
import { sigmaFor } from './format';
import type { ControlState } from './types';

const scope = { kind: 'all' as const, id: null, label: 'All nodes' };

function stateFor(tool: keyof typeof REPORTS): ControlState {
  return initialControlState(REPORTS[tool], scope);
}

describe('buildJobInput', () => {
  it('fills fields whose control the tool HIDES, for every report', () => {
    // This is the silent-divergence guard: AnalysisJobInput is one-size-fits-all, so a hidden
    // control still ships a value. The engines floor window/baseline with .max(), so a zero or
    // missing baseline would quietly change results versus the launch drawer for the same tool.
    for (const [key, d] of Object.entries(REPORTS)) {
      const input = buildJobInput(d, stateFor(key as keyof typeof REPORTS), 'last 1 h');
      expect(input.window_secs, key).toBeGreaterThan(0);
      expect(input.baseline_secs, key).toBeGreaterThan(0);
      expect(input.sensitivity, key).toBeGreaterThan(0);
      expect(input.depth, key).toBeTruthy();
      expect(input.tool, key).toBe(key);
    }
  });

  it('sends a baseline even for a tool whose bar has no baseline picker', () => {
    // rule_gap shows only scope + window, but the engine still receives a real baseline.
    const d = REPORTS.rule_gap;
    expect(d.controls.controls).not.toContain('baseline');
    const input = buildJobInput(d, stateFor('rule_gap'), 'last 24 h');
    expect(input.baseline_secs).toBe(d.controls.defaults.baselineSecs);
    expect(input.baseline_secs).toBeGreaterThan(0);
  });

  it('converts the slider position to sigma rather than passing it through raw', () => {
    // A raw slider value would coincidentally look sane at 3 (sigmaFor(3) === 3.0) but be badly
    // wrong at the ends: slider 1 means σ 4.0 (loose), not σ 1.0 (extremely strict).
    const d = REPORTS.anomaly;
    // `sensitivity` is optional on the wire type, so pin that it is actually sent before comparing.
    const loose = buildJobInput(d, { ...stateFor('anomaly'), sensitivity: 1 }, 'w').sensitivity;
    const strict = buildJobInput(d, { ...stateFor('anomaly'), sensitivity: 5 }, 'w').sensitivity;
    expect(loose).toBe(sigmaFor(1));
    expect(loose).toBe(4);
    expect(strict).toBe(2);
    // Higher slider = stricter = LOWER sigma.
    expect(strict!).toBeLessThan(loose!);
  });

  it('composes the scope label the way the runs list expects', () => {
    const input = buildJobInput(REPORTS.flap, stateFor('flap'), 'last 7 d');
    expect(input.scope_label).toBe('All nodes · last 7 d');
  });

  it('carries a group/node scope through unchanged', () => {
    const groupScope = { kind: 'group' as const, id: 'g-1', label: 'group: Matsuyama' };
    const input = buildJobInput(
      REPORTS.capacity,
      initialControlState(REPORTS.capacity, groupScope),
      'last 30 d',
    );
    expect(input.scope_kind).toBe('group');
    expect(input.scope_id).toBe('g-1');
    expect(input.scope_label).toBe('group: Matsuyama · last 30 d');
  });

  it('never notifies from a report launch by accident', () => {
    // The report bar is an operator action, so notify stays on — asserted so a future edit that
    // flips it is a deliberate, visible change rather than a silent one.
    expect(buildJobInput(REPORTS.anomaly, stateFor('anomaly'), 'w').notify).toBe(true);
  });
});

describe('initialControlState', () => {
  it('seeds every control from the tool’s declared defaults', () => {
    for (const [key, d] of Object.entries(REPORTS)) {
      const s = initialControlState(d, scope);
      expect(s.windowSecs, key).toBe(d.controls.defaults.windowSecs);
      expect(s.baselineSecs, key).toBe(d.controls.defaults.baselineSecs);
      expect(s.sensitivity, key).toBe(d.controls.defaults.sensitivity);
      expect(s.depth, key).toBe(d.controls.defaults.depth);
    }
  });

  it('offers the default window as a selectable preset, so the bar opens on a real option', () => {
    // Otherwise the window Select would render with no matching option and look blank.
    for (const [key, d] of Object.entries(REPORTS)) {
      if (!d.controls.controls.includes('window')) continue;
      const secs = d.controls.defaults.windowSecs;
      expect(
        d.controls.windows.map((w) => w.secs),
        `${key} default window must be one of its presets`,
      ).toContain(secs);
    }
  });

  it('offers the default baseline as a preset wherever a baseline picker is shown', () => {
    for (const [key, d] of Object.entries(REPORTS)) {
      if (!d.controls.controls.includes('baseline')) continue;
      expect(
        (d.controls.baselines ?? []).map((b) => b.secs),
        `${key} default baseline must be one of its presets`,
      ).toContain(d.controls.defaults.baselineSecs);
    }
  });
});
