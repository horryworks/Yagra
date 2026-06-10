import { describe, expect, it } from 'vitest';
import {
  pointsToSeries,
  severityColorVar,
  severityRank,
  stateColorVar,
  stateLabel,
} from './format';

describe('format', () => {
  it('maps severity to status/theme color variables', () => {
    expect(severityColorVar('critical')).toBe('var(--status-critical)');
    expect(severityColorVar('warning')).toBe('var(--status-warning)');
    expect(severityColorVar('info')).toBe('var(--severity-info)');
  });

  it('maps node state to status color variables', () => {
    expect(stateColorVar('unreachable')).toBe('var(--status-unreachable)');
    expect(stateColorVar('ok')).toBe('var(--status-up)');
    expect(stateColorVar('warning')).toBe('var(--status-warning)');
    expect(stateColorVar('maintenance')).toBe('var(--status-maintenance)');
  });

  it('ranks severities for sorting', () => {
    expect(severityRank('critical')).toBeGreaterThan(severityRank('warning'));
    expect(severityRank('warning')).toBeGreaterThan(severityRank('info'));
  });

  it('capitalizes state labels', () => {
    expect(stateLabel('maintenance')).toBe('Maintenance');
  });

  it('splits time-series points into parallel arrays', () => {
    const { timestamps, values } = pointsToSeries([
      { t: 100, v: 8 },
      { t: 160, v: 9.5 },
    ]);
    expect(timestamps).toEqual([100, 160]);
    expect(values).toEqual([8, 9.5]);
    expect(pointsToSeries([])).toEqual({ timestamps: [], values: [] });
  });
});
