import { describe, expect, it } from 'vitest';
import {
  deriveMem,
  formatBps,
  formatBytes,
  formatCount,
  formatDaysToExpiry,
  formatUptimeTicks,
  formatUtil,
  httpStatusLabel,
  httpStatusTone,
  initials,
  localTimeZone,
  pointsToSeries,
  relativeTime,
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

  it('formats a count: rounds to whole, groups via the locale, dash when non-finite', () => {
    // Grouping is delegated to the platform locale (i18n-ready) — assert against the same call
    // rather than a hardcoded separator so the test is locale-independent.
    expect(formatCount(0)).toBe('0');
    expect(formatCount(12_840)).toBe((12_840).toLocaleString());
    expect(formatCount(1_234_567)).toBe((1_234_567).toLocaleString());
    expect(formatCount(199.6)).toBe((200).toLocaleString()); // rounds to whole
    expect(formatCount(Number.NaN)).toBe('—');
    expect(formatCount(Number.POSITIVE_INFINITY)).toBe('—');
  });

  it('formats utilization percentage and a dash when unknown', () => {
    expect(formatUtil(null)).toBe('—');
    expect(formatUtil(0)).toBe('0%');
    expect(formatUtil(2.5)).toBe('2.5%');
    expect(formatUtil(73)).toBe('73%');
    expect(formatUtil(100)).toBe('100%');
  });

  it('formats byte counts with binary-scaled units and a dash when unknown', () => {
    expect(formatBytes(null)).toBe('—');
    expect(formatBytes(-1)).toBe('—');
    expect(formatBytes(512)).toBe('512 B');
    expect(formatBytes(1024)).toBe('1 KB');
    expect(formatBytes(32 * 1024 ** 3)).toBe('32 GB');
    expect(formatBytes(1.5 * 1024 ** 3)).toBe('1.5 GB');
    expect(formatBytes(128 * 1024 ** 3)).toBe('128 GB');
  });

  it('derives memory used/total bytes and % per source shape', () => {
    // Huawei: total + free (bytes) → used = total − free.
    expect(
      deriveMem('huawei', { huawei_mem_total: 4_000_000_000, huawei_mem_free: 1_000_000_000 }),
    ).toEqual({ usedBytes: 3_000_000_000, totalBytes: 4_000_000_000, pct: 75 });

    // Cisco: used + free → total = used + free.
    expect(deriveMem('cisco', { cisco_mem_used: 750, cisco_mem_free: 250 })).toEqual({
      usedBytes: 750,
      totalBytes: 1000,
      pct: 75,
    });

    // UCD: KB inputs scaled to bytes; used = total − avail.
    expect(deriveMem('ucd', { ucd_mem_total_kb: 1000, ucd_mem_avail_kb: 250 }, 1024)).toEqual({
      usedBytes: 750 * 1024,
      totalBytes: 1000 * 1024,
      pct: 75,
    });

    // Partial inputs ⇒ derive what's possible, null the rest; no divide-by-zero.
    expect(deriveMem('huawei', { huawei_mem_total: 4_000_000_000 })).toEqual({
      usedBytes: null,
      totalBytes: 4_000_000_000,
      pct: null,
    });
    expect(deriveMem('cisco', { cisco_mem_used: 100 })).toEqual({
      usedBytes: 100,
      totalBytes: null,
      pct: null,
    });
    expect(deriveMem('ucd', { ucd_mem_total_kb: 0, ucd_mem_avail_kb: 0 }, 1024)).toEqual({
      usedBytes: 0,
      totalBytes: 0,
      pct: null,
    });
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

  it('maps HTTP status to the status-palette tone (2xx/4xx/5xx)', () => {
    expect(httpStatusTone(200)).toBe('up');
    expect(httpStatusTone(201)).toBe('up');
    expect(httpStatusTone(401)).toBe('warning');
    expect(httpStatusTone(409)).toBe('warning');
    expect(httpStatusTone(500)).toBe('critical');
  });

  it('labels HTTP status codes for humans', () => {
    expect(httpStatusLabel(200)).toBe('OK');
    expect(httpStatusLabel(401)).toBe('Denied');
    expect(httpStatusLabel(403)).toBe('Denied');
    expect(httpStatusLabel(409)).toBe('Conflict');
    expect(httpStatusLabel(404)).toBe('Client error');
    expect(httpStatusLabel(503)).toBe('Server error');
  });

  it('labels a TLS certificate days-to-expiry (future / today / expired)', () => {
    expect(formatDaysToExpiry(45)).toBe('45 days left');
    expect(formatDaysToExpiry(1)).toBe('1 day left');
    expect(formatDaysToExpiry(29.6)).toBe('30 days left'); // rounds to whole days
    expect(formatDaysToExpiry(0.4)).toBe('expires today');
    expect(formatDaysToExpiry(-3)).toBe('expired 3d ago');
  });

  it('derives up to two monogram initials', () => {
    expect(initials('k.tanaka')).toBe('KT');
    expect(initials('noc-shift')).toBe('NS');
    expect(initials('admin')).toBe('AD');
    expect(initials('unknown')).toBe('?');
    expect(initials('')).toBe('?');
  });

  it('formats relative time against an injected now', () => {
    const now = Date.UTC(2026, 5, 15, 9, 0, 0); // 2026-06-15T09:00:00Z
    expect(relativeTime(null, now)).toBe('Never');
    expect(relativeTime('2026-06-15T08:59:40Z', now)).toBe('just now');
    expect(relativeTime('2026-06-15T08:42:00Z', now)).toBe('18m ago');
    expect(relativeTime('2026-06-15T07:00:00Z', now)).toBe('2h ago');
    expect(relativeTime('2026-06-14T06:00:00Z', now)).toBe('Yesterday'); // 27h → 24-48h band
    expect(relativeTime('2026-06-10T09:00:00Z', now)).toBe('5d ago');
  });

  it('gives known scalars a friendly label + formatted value, unknowns the raw name', () => {
    const up = scalarDisplay('snmp_sys_uptime_ticks', 337326072);
    expect(up).toEqual({ label: 'Uptime', value: '1mo 9d 01:01', known: true });

    const raw = scalarDisplay('snmp_oid_1_3_6', 42);
    expect(raw).toEqual({ label: 'snmp_oid_1_3_6', value: '42', known: false });
  });
});
