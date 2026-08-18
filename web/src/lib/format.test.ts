// SPDX-License-Identifier: AGPL-3.0-only
import { describe, expect, it } from 'vitest';
import {
  alertWhat,
  alertWhatOf,
  deriveMem,
  formatAsn,
  formatBps,
  formatPps,
  formatBytes,
  formatCount,
  formatDaysToExpiry,
  formatDbm,
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
  stateColorValue,
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

  it('formats an AS label, or null when the ASN is unknown', () => {
    expect(formatAsn(15169, 'GOOGLE')).toBe('AS15169 · GOOGLE');
    expect(formatAsn(15169)).toBe('AS15169');
    expect(formatAsn(15169, null)).toBe('AS15169');
    // 0 / undefined ⇒ unknown, caller omits the AS line.
    expect(formatAsn(0, 'ignored')).toBeNull();
    expect(formatAsn(undefined)).toBeNull();
  });

  it('resolves a node state to a concrete color (never a CSS var), falling back without a DOM', () => {
    // Canvas charts (uPlot) can't read CSS vars, so this must return a concrete color. In the node
    // test env there's no document, so it returns the concrete fallback — and never a `var(...)`.
    expect(stateColorValue('critical', '#fallback')).toBe('#fallback');
    expect(stateColorValue('warning')).not.toContain('var(');
    expect(stateColorValue('unknown')).not.toContain('var(');
  });

  it('ranks severities for sorting', () => {
    expect(severityRank('critical')).toBeGreaterThan(severityRank('warning'));
    expect(severityRank('warning')).toBeGreaterThan(severityRank('info'));
  });

  it('describes WHAT an alert fired on (alertWhat)', () => {
    // Legacy row with no captured metric → "—".
    expect(alertWhat({})).toEqual({ kind: 'none' });
    expect(alertWhat({ metric: null })).toEqual({ kind: 'none' });

    // Liveness up/down sentinel reads as "Reachability" (never the raw sentinel).
    expect(alertWhat({ metric: '__liveness__' })).toEqual({ kind: 'liveness' });

    // Threshold breach: metric + crossed condition + observed value.
    expect(
      alertWhat({
        metric: 'icmp_rtt_ms',
        direction: 'above',
        threshold_value: 100,
        observed_value: 450,
      }),
    ).toEqual({
      kind: 'metric',
      metric: 'icmp_rtt_ms',
      condition: 'above 100',
      observed: 'was 450',
    });

    // A metric with no numeric breach (e.g. partial data) still shows the metric, no condition.
    expect(alertWhat({ metric: 'http_up' })).toEqual({
      kind: 'metric',
      metric: 'http_up',
      condition: null,
      observed: null,
    });
  });

  it('describes a LIVE alert the same way as its history row (alertWhatOf)', () => {
    // The nested `breach` and the flattened history columns are the same fact in two shapes; the
    // whole point of the adapter is that they can never render differently.
    const live = {
      metric: 'icmp_rtt_ms',
      breach: { value: 450, threshold: 100, direction: 'above' },
    };
    const historyRow = {
      metric: 'icmp_rtt_ms',
      direction: 'above',
      threshold_value: 100,
      observed_value: 450,
    };
    expect(alertWhatOf(live)).toEqual(alertWhat(historyRow));

    // A liveness alert carries no breach at all.
    expect(alertWhatOf({ metric: '__liveness__', breach: null })).toEqual({ kind: 'liveness' });

    // A threshold alert whose committed severity has no bound at that level: value, no threshold.
    expect(
      alertWhatOf({ metric: 'cpu_pct', breach: { value: 91, threshold: null, direction: 'above' } }),
    ).toEqual({ kind: 'metric', metric: 'cpu_pct', condition: null, observed: 'was 91' });

    // An alert with no metric at all (an N-1 core that predates migration 0036) → "—".
    expect(alertWhatOf({})).toEqual({ kind: 'none' });
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

  it('formats packets-per-second with SI units and a dash when unknown', () => {
    expect(formatPps(null)).toBe('—');
    expect(formatPps(12)).toBe('12 pps');
    expect(formatPps(2_500)).toBe('2.5 kpps');
    // A whole number keeps no decimal — "1 Mpps", where formatBps would say "1.0 Gbps". The two
    // deliberately differ: see below and formatPps's own doc.
    expect(formatPps(1_000_000)).toBe('1 Mpps');
    // ⚠️ Unlike formatBps, a sub-1 rate keeps its decimal. The dock only shows an error/discard
    // tile *because* the value is non-zero, so rounding 0.4 to "0 pps" would contradict the
    // reason it is on screen — and a flat zero reads as "no problem here".
    expect(formatPps(0.4)).toBe('0.4 pps');
  });

  it('formats a count: rounds to whole, groups via the active-language locale, dash when non-finite', () => {
    // Grouping follows the interface language's locale (en → en-US here). Assert against the same
    // explicit locale rather than a hardcoded separator so the test is host-locale-independent.
    expect(formatCount(0)).toBe('0');
    expect(formatCount(12_840)).toBe((12_840).toLocaleString('en-US'));
    expect(formatCount(1_234_567)).toBe((1_234_567).toLocaleString('en-US'));
    expect(formatCount(199.6)).toBe((200).toLocaleString('en-US')); // rounds to whole
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

  /// Three Cisco memory MIBs, and no measured device answers more than one (ADR-070).
  it('derives memory from each Cisco family, including the kilobyte one', () => {
    // CISCO-ENHANCED-MEMPOOL — 64-bit bytes. The real N3K figures.
    expect(
      deriveMem('cisco-cemp', {
        cisco_cemp_mem_used: 2_589_782_016,
        cisco_cemp_mem_free: 1_406_861_312,
      }),
    ).toEqual({
      usedBytes: 2_589_782_016,
      totalBytes: 3_996_643_328,
      pct: (2_589_782_016 / 3_996_643_328) * 100,
    });

    // CISCO-PROCESS cpmCPUMemory — the same shape but in **kilobytes**, so the caller passes 1024.
    // Getting this scale wrong would under-report a Catalyst 9000's memory by 1000× while still
    // producing a believable percentage, which is why the byte figures are asserted and not just
    // the percent.
    expect(
      deriveMem('cisco-cpu', { cisco_cpu_mem_used: 2_528_860, cisco_cpu_mem_free: 1_374_112 }, 1024),
    ).toEqual({
      usedBytes: 2_528_860 * 1024,
      totalBytes: (2_528_860 + 1_374_112) * 1024,
      pct: (2_528_860 / (2_528_860 + 1_374_112)) * 100,
    });

    // Each family reads only its own metric names: feeding one family's values under another's id
    // must yield nothing rather than silently borrowing the wrong pair.
    expect(deriveMem('cisco-cemp', { cisco_mem_used: 750, cisco_mem_free: 250 })).toEqual({
      usedBytes: null,
      totalBytes: null,
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

describe('formatDbm', () => {
  it('shows an em dash when the port reports no level', () => {
    expect(formatDbm(null)).toBe('—');
    expect(formatDbm(undefined)).toBe('—');
    expect(formatDbm(Number.NaN)).toBe('—');
  });

  // Optical levels are normally negative — the formatters beside this one mishandle that, which is
  // why dBm gets its own rather than reusing formatBps/formatBytes.
  it('keeps the sign on a normal receive level', () => {
    expect(formatDbm(-7.42)).toBe('-7.4 dBm');
    expect(formatDbm(-20)).toBe('-20.0 dBm');
  });

  // 0 dBm is one milliwatt: a real, strong reading, not an absence.
  it('renders 0 dBm as a value', () => {
    expect(formatDbm(0)).toBe('0.0 dBm');
  });

  // Half a dB matters in a link budget, so the decimal stays even on whole numbers — otherwise a
  // link drifting past an integer would look flat on exactly those ticks.
  it('always keeps one decimal, including on whole numbers', () => {
    expect(formatDbm(3)).toBe('3.0 dBm');
    expect(formatDbm(-7)).toBe('-7.0 dBm');
  });

  // ⚠️ dBm is logarithmic: it must never be SI-scaled the way bit rates are.
  it('never applies an SI prefix', () => {
    expect(formatDbm(-1500)).toBe('-1500.0 dBm');
  });
});
