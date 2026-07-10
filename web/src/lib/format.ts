// Presentation helpers. Colors resolve to theme CSS variables (ui-conventions) — no hardcoded hex
// here, so the theme stays the single source of truth. User-facing words (state/http/relative-time
// labels, units of time) resolve through the global i18next instance so they follow the active
// language without every call site needing a hook; importing i18n here also guarantees it is
// initialized (English is bundled synchronously) before any helper runs, including in tests.
// Reactivity on a language switch comes from the App-level `useTranslation` re-render cascade.

import i18n from '../i18n';
import { intlLocale } from './locale';
import type { MetricPoint, NodeState, Severity } from '../types/api';

/** Split time-series points into the parallel `[timestamps, values]` uPlot wants. */
export function pointsToSeries(points: MetricPoint[]): {
  timestamps: number[];
  values: number[];
} {
  return {
    timestamps: points.map((p) => p.t),
    values: points.map((p) => p.v),
  };
}

/** CSS variable holding the color for a severity. Severity maps onto the status palette
   (critical/warning); 'info' is not a network status, so it borrows a categorical color. */
export function severityColorVar(severity: Severity): string {
  switch (severity) {
    case 'critical':
      return 'var(--status-critical)';
    case 'warning':
      return 'var(--status-warning)';
    case 'info':
      return 'var(--severity-info)';
  }
}

/** CSS variable holding the color for a node state (design-system §1.3 status semantics). */
export function stateColorVar(state: NodeState): string {
  switch (state) {
    case 'ok':
      return 'var(--status-up)';
    case 'warning':
      return 'var(--status-warning)';
    case 'critical':
      return 'var(--status-critical)';
    case 'unreachable':
      return 'var(--status-unreachable)';
    case 'unknown':
      return 'var(--status-unknown)';
    case 'maintenance':
      return 'var(--status-maintenance)';
  }
}

/** Resolve a node state's status color to a **concrete** color string for canvas charts (uPlot
 *  strokes can't read CSS vars). Reads the active theme's variable off the document root, so the
 *  chart's status colors track light/dark and stay identical to the table/donut/tree — unlike a
 *  per-component hardcoded hex. Falls back to `fallback` when there's no DOM or the var is unset. */
export function stateColorValue(state: NodeState, fallback = '#8a93a3'): string {
  const name = stateColorVar(state).replace(/^var\((--[^)]+)\)$/, '$1');
  if (typeof document === 'undefined') return fallback;
  const v = getComputedStyle(document.documentElement).getPropertyValue(name).trim();
  return v || fallback;
}

/** Localized human label for a node state. */
export function stateLabel(state: NodeState): string {
  return i18n.t(`format:state.${state}`);
}

/** Rank a severity for sorting (higher = worse). */
export function severityRank(severity: Severity): number {
  return { info: 0, warning: 1, critical: 2 }[severity];
}

/** Localized human label for an alert severity (parallels `stateLabel`). */
export function severityLabel(severity: Severity): string {
  return i18n.t(`format:severity.${severity}`);
}

/** Format a Unix-ms timestamp as a local date-time string in the active interface language's
 *  locale (pass `locale` to override). Zone is always the browser's local zone. */
export function formatTimestamp(unixMs: number, locale: string = intlLocale(i18n.language)): string {
  return new Date(unixMs).toLocaleString(locale);
}

/** Exact timestamp as a stable, locale-independent `YYYY-MM-DD HH:MM:SS` in the browser's
 *  local zone (sv-SE renders ISO-like regardless of the user's locale). For forensic screens
 *  (audit) where the precise instant is the primary value. */
export function formatExactTime(iso: string): string {
  return new Date(iso).toLocaleString('sv-SE').replace('T', ' ');
}

/** Date-only `YYYY-MM-DD` in the local zone (en-CA renders ISO-like). */
export function dateOnly(iso: string): string {
  return new Date(iso).toLocaleDateString('en-CA');
}

/** Compact relative time ("just now" / "5m ago" / "3h ago" / "Yesterday" / "12d ago"), or
 *  "Never" for a null timestamp. `now` is injectable so callers/tests are deterministic. */
export function relativeTime(iso: string | null, now: number = Date.now()): string {
  if (!iso) return i18n.t('format:relative.never');
  const mins = Math.floor((now - new Date(iso).getTime()) / 60000);
  if (mins < 1) return i18n.t('format:relative.justNow');
  if (mins < 60) return i18n.t('format:relative.min', { count: mins });
  if (mins < 1440) return i18n.t('format:relative.hour', { count: Math.floor(mins / 60) });
  if (mins < 2880) return i18n.t('format:relative.yesterday');
  return i18n.t('format:relative.day', { count: Math.floor(mins / 1440) });
}

/** Tone for an HTTP status code, on the status palette only (2xx up / 4xx warning / 5xx
 *  critical). Used by the audit log; the method/path is categorical and never tone-colored. */
export function httpStatusTone(status: number): 'up' | 'warning' | 'critical' {
  if (status < 300) return 'up';
  if (status < 500) return 'warning';
  return 'critical';
}

/** Short human label for an HTTP status (paired with the code + dot so it's not color-alone). */
export function httpStatusLabel(status: number): string {
  if (status < 300) return i18n.t('format:http.ok');
  if (status === 401 || status === 403) return i18n.t('format:http.denied');
  if (status === 409) return i18n.t('format:http.conflict');
  if (status < 500) return i18n.t('format:http.clientError');
  return i18n.t('format:http.serverError');
}

/** Localized label for a TLS certificate's days-to-expiry. Negative ⇒ already expired. */
export function formatDaysToExpiry(days: number): string {
  const d = Math.round(days);
  if (d < 0) return i18n.t('format:expiry.expired', { count: Math.abs(d) });
  if (d === 0) return i18n.t('format:expiry.today');
  return i18n.t('format:expiry.left', { count: d });
}

/** Up to two initials for a monogram avatar. Splits on `.`/`-`/`_`; `unknown`/empty ⇒ "?". */
export function initials(name: string): string {
  if (!name || name === 'unknown') return '?';
  const parts = name.replace(/[^a-zA-Z0-9.\-_]/g, '').split(/[.\-_]/).filter(Boolean);
  const pick = parts.length >= 2 ? parts[0][0] + parts[1][0] : name.slice(0, 2);
  return pick.toUpperCase();
}

/** The browser's IANA time-zone name (e.g. "Asia/Tokyo"), used to label datetime-local
 *  inputs so operators know they're entering local time (stored as UTC). Falls back to a
 *  generic phrase if the runtime can't resolve a zone. */
export function localTimeZone(): string {
  try {
    return Intl.DateTimeFormat().resolvedOptions().timeZone || i18n.t('format:localTimeZoneFallback');
  } catch {
    return i18n.t('format:localTimeZoneFallback');
  }
}

/** Format a millisecond RTT value. */
export function formatRtt(ms: number): string {
  return `${ms.toFixed(1)} ms`;
}

/** Format a bits-per-second rate with SI-ish units (k/M/G), or `—` when unknown. */
export function formatBps(bps: number | null): string {
  if (bps == null) return '—';
  const units = ['bps', 'kbps', 'Mbps', 'Gbps', 'Tbps'];
  let v = bps;
  let u = 0;
  while (v >= 1000 && u < units.length - 1) {
    v /= 1000;
    u += 1;
  }
  return `${v.toFixed(v >= 100 || u === 0 ? 0 : 1)} ${units[u]}`;
}

/** Format a utilization percentage, or `—` when unknown (no speed / no data). Whole numbers
 *  (including 0 and 100) show no decimal ("0%", "75%"); sub-10 fractions keep one place. */
export function formatUtil(pct: number | null): string {
  if (pct == null) return '—';
  const digits = Number.isInteger(pct) || pct >= 10 ? 0 : 1;
  return `${pct.toFixed(digits)}%`;
}

/** Format a byte count with binary-scaled units rendered with familiar GB-style suffixes
 *  (memory totals read naturally as e.g. "32 GB"), or `—` when unknown/negative. Whole values
 *  drop the decimal (`32 GB`, not `32.0 GB`); fractional ones keep one place under 100. */
export function formatBytes(bytes: number | null): string {
  if (bytes == null || !Number.isFinite(bytes) || bytes < 0) return '—';
  const units = ['B', 'KB', 'MB', 'GB', 'TB', 'PB'];
  let v = bytes;
  let u = 0;
  while (v >= 1024 && u < units.length - 1) {
    v /= 1024;
    u += 1;
  }
  const digits = u === 0 || Number.isInteger(v) || v >= 100 ? 0 : 1;
  return `${v.toFixed(digits)} ${units[u]}`;
}

/** Per-source memory math: normalize a node's raw metric values (keyed by metric name) to used
 *  and total **bytes**, plus the derived utilization %. Each built-in source exposes a different
 *  pair of inputs, all reducible to used+total:
 *   - `huawei`: HUAWEI-MEMORY-MIB total + free bytes -> used = total - free.
 *   - `cisco` : CISCO-MEMORY-POOL used + free bytes -> total = used + free.
 *   - `ucd`   : UCD real-memory total + avail KB -> used = total - avail (scaled to bytes).
 *  `unitToBytes` scales the inputs to bytes (1 for byte OIDs, 1024 for KB). Fields are null when
 *  the inputs needed for them are missing/non-finite; `pct` needs both used and a positive total. */
export function deriveMem(
  id: 'huawei' | 'cisco' | 'ucd',
  vals: Record<string, number | null | undefined>,
  unitToBytes = 1,
): { usedBytes: number | null; totalBytes: number | null; pct: number | null } {
  const num = (k: string): number | null => {
    const x = vals[k];
    return typeof x === 'number' && Number.isFinite(x) ? x : null;
  };
  let used: number | null = null;
  let total: number | null = null;
  if (id === 'huawei') {
    const t = num('huawei_mem_total');
    const f = num('huawei_mem_free');
    if (t != null) total = t * unitToBytes;
    if (t != null && f != null) used = (t - f) * unitToBytes;
  } else if (id === 'cisco') {
    const u = num('cisco_mem_used');
    const f = num('cisco_mem_free');
    if (u != null) used = u * unitToBytes;
    if (u != null && f != null) total = (u + f) * unitToBytes;
  } else {
    const t = num('ucd_mem_total_kb');
    const a = num('ucd_mem_avail_kb');
    if (t != null) total = t * unitToBytes;
    if (t != null && a != null) used = (t - a) * unitToBytes;
  }
  const pct = used != null && total != null && total > 0 ? (used / total) * 100 : null;
  return { usedBytes: used, totalBytes: total, pct };
}

/** Format SNMP TimeTicks (hundredths of a second) as a compact human uptime, e.g.
 *  `1y 2mo 3d 12:34`. The date head uses distinct, spaced unit suffixes — `y` / `mo` / `d` — so
 *  month never collides with the minutes that live after the `HH:MM` colon. Larger zero units are
 *  dropped (39 days reads `1mo 9d 02:09`, not `0y 1mo 9d …`); `HH:MM` is always shown, zero-padded.
 *  Months/years are approximate (30d / 365d) — fine for an at-a-glance uptime. Returns `—` for a
 *  missing/negative value. */
export function formatUptimeTicks(ticks: number): string {
  if (!Number.isFinite(ticks) || ticks < 0) return '—';
  let secs = Math.floor(ticks / 100);
  const YEAR = 365 * 86400;
  const MONTH = 30 * 86400;
  const years = Math.floor(secs / YEAR);
  secs -= years * YEAR;
  const months = Math.floor(secs / MONTH);
  secs -= months * MONTH;
  const days = Math.floor(secs / 86400);
  secs -= days * 86400;
  const hours = Math.floor(secs / 3600);
  secs -= hours * 3600;
  const minutes = Math.floor(secs / 60);
  const parts: string[] = [];
  if (years > 0) parts.push(`${years}y`);
  if (parts.length || months > 0) parts.push(`${months}mo`);
  if (parts.length || days > 0) parts.push(`${days}d`);
  const hm = `${String(hours).padStart(2, '0')}:${String(minutes).padStart(2, '0')}`;
  return parts.length ? `${parts.join(' ')} ${hm}` : hm;
}

/** Metric names that have a friendly display label under `format:scalar.*`. Kept as a registry
 *  (the labels themselves are localized) — an unknown metric falls back to its raw name. */
const KNOWN_SCALARS = new Set<string>([
  'snmp_sys_uptime_ticks',
  // Cisco Meraki (Dashboard API) metrics.
  'meraki_device_up',
  'meraki_client_count',
  'meraki_usage_sent_kb',
  'meraki_usage_recv_kb',
  'meraki_uplink_loss_pct',
  'meraki_uplink_latency_ms',
  'meraki_uplink_status',
]);

/** A known scalar gets a localized label + formatted value (and renders in the UI font, not mono);
 *  an unknown one keeps its raw OID-ish metric name + numeric value (mono). */
export function scalarDisplay(metric: string, value: number): {
  label: string;
  value: string;
  known: boolean;
} {
  const known = KNOWN_SCALARS.has(metric);
  const label = known ? i18n.t(`format:scalar.${metric}`) : metric;
  const display = metric === 'snmp_sys_uptime_ticks' ? formatUptimeTicks(value) : String(value);
  return { label, value: display, known };
}

/** Whole-number count with locale thousands separators (e.g. 12840 → "12,840"), or `—` for a
 *  non-finite value. For session/connection counts shown as a headline or chart-hover readout
 *  (where the axis uses the compact `formatSi`). Rounds to the nearest integer. */
export function formatCount(n: number, locale: string = intlLocale(i18n.language)): string {
  if (!Number.isFinite(n)) return '—';
  return Math.round(n).toLocaleString(locale);
}

/** The liveness check sentinel (yagra-core `LIVENESS`), shown to humans as "Reachability". */
export const LIVENESS_METRIC = '__liveness__';

/** What an alert-history row fired on, split into parts so the cell can style the metric (mono)
 *  apart from the condition. Pure so it's unit-testable without the DOM:
 *   - `none`     → no metric captured (legacy row) ⇒ render "—"
 *   - `liveness` → the reachability up/down check ⇒ render "Reachability"
 *   - `metric`   → a threshold metric, with an optional crossed condition + observed value. */
export type AlertWhat =
  | { kind: 'none' }
  | { kind: 'liveness' }
  | { kind: 'metric'; metric: string; condition: string | null; observed: string | null };

export function alertWhat(row: {
  metric?: string | null;
  direction?: string | null;
  threshold_value?: number | null;
  observed_value?: number | null;
}): AlertWhat {
  if (!row.metric) return { kind: 'none' };
  if (row.metric === LIVENESS_METRIC) return { kind: 'liveness' };
  const condition =
    row.direction && row.threshold_value != null
      ? `${row.direction} ${row.threshold_value}`
      : null;
  const observed =
    row.observed_value != null
      ? i18n.t('format:alertObservedWas', { value: row.observed_value })
      : null;
  return { kind: 'metric', metric: row.metric, condition, observed };
}

/** Compact, unit-less SI suffix (k/M/G/T) for a plain number — for chart axis ticks so big
 *  values (e.g. 455000) render as "455k" instead of being clipped. */
export function formatSi(n: number): string {
  const abs = Math.abs(n);
  const units: [number, string][] = [
    [1e12, 'T'],
    [1e9, 'G'],
    [1e6, 'M'],
    [1e3, 'k'],
  ];
  for (const [div, suffix] of units) {
    if (abs >= div) {
      const v = n / div;
      return `${v.toFixed(v >= 100 || Number.isInteger(v) ? 0 : 1)}${suffix}`;
    }
  }
  return Number.isInteger(n) ? String(n) : n.toFixed(1);
}
