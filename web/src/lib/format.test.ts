import { describe, expect, it } from 'vitest';
import {
  formatBps,
  formatUptimeTicks,
  formatUtil,
  localTimeZone,
  pointsToSeries,
  scalarDisplay,
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

  it('formats bits-per-second with SI units and a dash when unknown', () => {
    expect(formatBps(null)).toBe('—');
    expect(formatBps(500)).toBe('500 bps');
    expect(formatBps(2_500)).toBe('2.5 kbps');
    expect(formatBps(1_000_000_000)).toBe('1.0 Gbps');
  });

  it('formats utilization percentage and a dash when unknown', () => {
    expect(formatUtil(null)).toBe('—');
    expect(formatUtil(2.5)).toBe('2.5%');
    expect(formatUtil(73)).toBe('73%');
  });

  it('formats SNMP TimeTicks as a compact human uptime (mo + HH:MM)', () => {
    // The screenshot value: 337326072 ticks ≈ 39 days 1h 1m.
    expect(formatUptimeTicks(337326072)).toBe('1mo 9d 01:01');
    // Years through minutes, all populated — month is "mo", minutes after the colon.
    expect(formatUptimeTicks(3702444000)).toBe('1y 2mo 3d 12:34');
    // Sub-day uptime drops the y/mo/d head and keeps zero-padded HH:MM.
    expect(formatUptimeTicks(540000)).toBe('01:30');
    expect(formatUptimeTicks(0)).toBe('00:00');
    // Missing / nonsensical values fall back to a dash.
    expect(formatUptimeTicks(-1)).toBe('—');
    expect(formatUptimeTicks(Number.NaN)).toBe('—');
  });

  it('returns a non-empty time-zone label for datetime hints', () => {
    const tz = localTimeZone();
    expect(typeof tz).toBe('string');
    expect(tz.length).toBeGreaterThan(0);
  });

  it('gives known scalars a friendly label + formatted value, unknowns the raw name', () => {
    const up = scalarDisplay('snmp_sys_uptime_ticks', 337326072);
    expect(up).toEqual({ label: 'Uptime', value: '1mo 9d 01:01', known: true });

    const raw = scalarDisplay('snmp_oid_1_3_6', 42);
    expect(raw).toEqual({ label: 'snmp_oid_1_3_6', value: '42', known: false });
  });
});
