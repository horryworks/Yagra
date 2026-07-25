// SPDX-License-Identifier: AGPL-3.0-only
import { describe, expect, it } from 'vitest';
import { KINDS, METHODS, TOOLS, kindMeta, reportPathFor, toolById } from './data';

describe('troubleshoot catalog data', () => {
  it('every tool has a unique id and a known method', () => {
    const ids = TOOLS.map((t) => t.id);
    expect(new Set(ids).size).toBe(ids.length);
    for (const t of TOOLS) expect(METHODS[t.method]).toBeDefined();
  });

  it('tool ids match the backend tool keys', () => {
    expect(TOOLS.map((t) => t.id).sort()).toEqual(
      [
        'anomaly',
        'capacity',
        'correlation',
        'flap',
        'event_storm',
        'event_flap',
        'severity_shift',
        'rule_gap',
        'auth_probe',
        'traffic_anomaly',
        'talker_shift',
        'new_destination',
        'flow_scan',
        'saturation',
        'incident_correlate',
      ].sort(),
    );
  });

  it('a report path, when present, is the canonical per-tool report route', () => {
    // Reports land per tool (1:1 with the scan) across several increments, so this asserts the SHAPE
    // rather than the set; `report/registry.test.ts` owns the stronger invariant — a tool advertises
    // a reportPath exactly when it has a report body.
    for (const t of TOOLS) {
      if (t.reportPath) expect(t.reportPath, t.id).toBe(reportPathFor(t.id));
    }
    expect(toolById('anomaly')?.reportPath).toBe('/troubleshoot/report/anomaly');
  });

  it('depth is within the 1–5 pip range', () => {
    for (const t of TOOLS) {
      expect(t.depth).toBeGreaterThanOrEqual(1);
      expect(t.depth).toBeLessThanOrEqual(5);
    }
  });

  it('kindMeta resolves anomaly shapes and falls back for unknown kinds', () => {
    expect(kindMeta('spike').label).toBe(KINDS.spike.label);
    expect(kindMeta('correlation').label).toBe('correlation'); // not an anomaly shape
  });
});
