// SPDX-License-Identifier: AGPL-3.0-only
import { describe, expect, it } from 'vitest';
import { KINDS, METHODS, TOOLS, TOOL_GROUPS, kindMeta, reportPathFor, toolById } from './data';

describe('troubleshoot catalog data', () => {
  it('every tool has a unique id and a known method', () => {
    const ids = TOOLS.map((t) => t.id);
    expect(new Set(ids).size).toBe(ids.length);
    for (const t of TOOLS) expect(METHODS[t.method]).toBeDefined();
  });

  it('every group the catalog renders holds at least one tool', () => {
    // The catalog iterates TOOL_GROUPS and filters (ADR-055 Inc.5), so a group with nothing in it
    // draws a heading over empty space — visible, silent, and easy to leave behind after moving a
    // tool. Asserting both directions also catches the inverse: a tool whose group is not rendered
    // at all would simply vanish from the page with nothing failing.
    for (const group of TOOL_GROUPS) {
      expect(TOOLS.filter((t) => t.group === group).length, `group ${group} is empty`).toBeGreaterThan(0);
    }
    expect(TOOLS.filter((t) => !TOOL_GROUPS.includes(t.group))).toEqual([]);
    expect(TOOL_GROUPS.flatMap((g) => TOOLS.filter((t) => t.group === g)).length).toBe(TOOLS.length);
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
