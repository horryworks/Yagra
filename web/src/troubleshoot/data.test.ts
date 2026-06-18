import { describe, expect, it } from 'vitest';
import { ANOMS, KINDS, METHODS, TOOLS, severityFor, toolById } from './data';

describe('troubleshoot catalog data', () => {
  it('every tool has a unique id and a known method', () => {
    const ids = TOOLS.map((t) => t.id);
    expect(new Set(ids).size).toBe(ids.length);
    for (const t of TOOLS) expect(METHODS[t.method]).toBeDefined();
  });

  it('only tools with a report screen expose a reportPath', () => {
    const anomaly = toolById('anomaly');
    expect(anomaly?.reportPath).toBe('/troubleshoot/anomaly');
    // The other three have no report screen yet.
    expect(toolById('correlation')?.reportPath).toBeUndefined();
    expect(toolById('capacity')?.reportPath).toBeUndefined();
    expect(toolById('flap')?.reportPath).toBeUndefined();
  });

  it('depth is within the 1–5 pip range', () => {
    for (const t of TOOLS) {
      expect(t.depth).toBeGreaterThanOrEqual(1);
      expect(t.depth).toBeLessThanOrEqual(5);
    }
  });
});

describe('anomaly findings', () => {
  it('severityFor matches the score thresholds (≥90 crit, ≥75 warn, else info)', () => {
    expect(severityFor(98)).toBe('crit');
    expect(severityFor(90)).toBe('crit');
    expect(severityFor(89)).toBe('warn');
    expect(severityFor(75)).toBe('warn');
    expect(severityFor(74)).toBe('info');
  });

  it('each finding’s stored severity agrees with its score', () => {
    for (const a of ANOMS) expect(a.sev).toBe(severityFor(a.score));
  });

  it('every finding kind has catalog metadata', () => {
    for (const a of ANOMS) expect(KINDS[a.kind]).toBeDefined();
  });
});
