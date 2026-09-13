// SPDX-License-Identifier: AGPL-3.0-only
// Presentation helpers. Colors resolve to theme CSS variables (ui-conventions) — no hardcoded hex
// here, so the theme stays the single source of truth. User-facing words (state/http/relative-time
// labels, units of time) resolve through the global i18next instance so they follow the active
// language without every call site needing a hook; importing i18n here also guarantees it is
// initialized (English is bundled synchronously) before any helper runs, including in tests.
// Reactivity on a language switch comes from the App-level `useTranslation` re-render cascade.

import i18n from '../i18n';
import { intlLocale } from './locale';
import metricUnits from '../api/metricUnits.json';
import type { Tone } from '../components/ui/Badge';
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

/** Severity -> `Badge` tone, beside the label and the color var that answer the same question
 *  for the other two channels.
 *
 *  Shared because the two screens that render a severity badge kept their own maps and had
 *  already disagreed: Event rules used `info`, Routing used `neutral`, and they sit next to each
 *  other in the same nav section. `info` wins — `badge-info` resolves to `--severity-info`, the
 *  same token `severityColorVar` returns, so the badge and the chart agree about what an
 *  informational alert looks like. `neutral` was also the tone Routing gives a rule with **no**
 *  severity, which made a rule routing only info indistinguishable from one routing everything. */
export const SEVERITY_TONE: Record<Severity, Tone> = {
  critical: 'critical',
  warning: 'warning',
  info: 'info',
};

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

/** A scheduled boundary — a maintenance window's start/end, a mute's expiry — in the browser's
 *  locale and zone, to the minute. Suppression is scheduled to the minute, so seconds would be
 *  noise on a row an operator scans.
 *
 *  Shared because the Maintenance page, the Mutes page and the inventory tree's release panel all
 *  render the same instants and had two byte-identical local copies between them; a third would
 *  have been the point where they started to disagree (`extensibility.md` §3). */
export function formatScheduleTime(iso: string): string {
  return new Date(iso).toLocaleString(undefined, {
    year: 'numeric',
    month: '2-digit',
    day: '2-digit',
    hour: '2-digit',
    minute: '2-digit',
  });
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

/** Format a packets-per-second rate with SI-ish units (k/M/G), or `—` when unknown. The pps
 *  counterpart to [`formatBps`] (ADR-060), and also the unit of the error and discard charts —
 *  IF-MIB counts errored and discarded *frames*, never their octets, so those two have no
 *  bits-per-second form.
 *
 *  A packet rate is three to four orders of magnitude below the bit rate of the same traffic
 *  (a frame is hundreds to ~1500 bytes, so thousands of bits), which is the quickest way to tell
 *  at a glance that the chart really switched units rather than re-drawing the same series.
 *
 *  ⚠️ **Unlike `formatBps` this keeps a decimal in the base unit.** Bit rates are large enough that
 *  a fraction of a bit/sec is noise, but an error rate of 0.4/s is a real signal that would render
 *  as a flat `0 pps` under that rule — and the dock only shows those tiles *because* the value is
 *  non-zero, so rounding it away would contradict the reason it is on screen. */
export function formatPps(pps: number | null): string {
  if (pps == null) return '—';
  const units = ['pps', 'kpps', 'Mpps', 'Gpps', 'Tpps'];
  let v = pps;
  let u = 0;
  while (v >= 1000 && u < units.length - 1) {
    v /= 1000;
    u += 1;
  }
  return `${v.toFixed(v >= 100 || Number.isInteger(v) ? 0 : 1)} ${units[u]}`;
}

/** Format an optical power level in dBm, or `—` when the port reports none (ADR-062).
 *
 *  ⚠️ **None of the formatters above can stand in for this one.** dBm is logarithmic, so it must
 *  never be SI-scaled — "−7.4 dBm" has no kilo- or milli- form, and `formatBps`-style scaling would
 *  invent one. It is also normally **negative**, which the other numeric formatters here handle
 *  badly: `formatBytes` returns `—` for anything below zero, and the `while (v >= 1000)` loops in
 *  `formatBps`/`formatPps` never run, so a negative would print with the base unit attached.
 *
 *  Always one decimal, including on whole numbers. Half a dB is a meaningful change in a link
 *  budget, so dropping the decimal for round values would make a degrading link look like a stable
 *  one on exactly the ticks where it crossed an integer. */
export function formatDbm(dbm: number | null | undefined): string {
  if (dbm == null || !Number.isFinite(dbm)) return '—';
  return `${dbm.toFixed(1)} dBm`;
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

/** {@link formatBytes} without the space or the `B`, for a chart's value axis: `13.4M`, `214M`, `512`.
 *
 *  ⚠️ **The same 1024 base as the legend beside it, and that is the point.** `formatSi` is
 *  1000-based, so an axis tick reading `14M` over a legend reading `13.4 MB` for the same value
 *  looks like two different measurements. And the full form does not fit: uPlot's value axis is
 *  50 px wide, `214 MB` is clipped at its leading digit, and a clipped leading digit reads as a
 *  number ten times smaller (ADR-137 決定 10). */
export function formatBytesAxis(bytes: number | null): string {
  const full = formatBytes(bytes);
  return full === '—' ? full : full.replace(' ', '').replace(/B$/, '');
}

/** The built-in memory sources. Declared here, next to the arithmetic that switches on it, and
 *  imported by the card registry that lists each source's inputs — it was written out twice, once
 *  as this function's parameter type and once as the registry's, which is two places to add a
 *  source and only one of them makes the arithmetic handle it. */
export type MemId = 'huawei' | 'cisco' | 'cisco-cemp' | 'cisco-cpu' | 'ucd';

/** Per-source memory math: normalize a node's raw metric values (keyed by metric name) to used
 *  and total **bytes**, plus the derived utilization %. Each built-in source exposes a different
 *  pair of inputs, all reducible to used+total:
 *   - `huawei`: HUAWEI-MEMORY-MIB total + free bytes -> used = total - free.
 *   - `cisco` : CISCO-MEMORY-POOL used + free bytes -> total = used + free.
 *   - `cisco-cemp`: CISCO-ENHANCED-MEMPOOL 64-bit used + free bytes, same arithmetic. A separate
 *     id rather than more candidates on `cisco` because a device answers one family or the other:
 *     ciscoMemoryPool is 2960X/3560-era, cempMemPool is Catalyst 9000, Nexus, IOS-XR and ASA, and
 *     no measured device answered both (ADR-070).
 *   - `cisco-cpu`: CISCO-PROCESS cpmCPUMemoryUsed/Free, in **kilobytes** — the third family, and
 *     the fallback for a Catalyst 9000 whose cempMemPool rows are absent.
 *   - `ucd`   : UCD real-memory total + avail KB -> used = total - avail (scaled to bytes).
 *  `unitToBytes` scales the inputs to bytes (1 for byte OIDs, 1024 for KB). Fields are null when
 *  the inputs needed for them are missing/non-finite; `pct` needs both used and a positive total. */
export function deriveMem(
  id: MemId,
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
  } else if (id === 'cisco' || id === 'cisco-cemp' || id === 'cisco-cpu') {
    const prefix =
      id === 'cisco' ? 'cisco_mem' : id === 'cisco-cemp' ? 'cisco_cemp_mem' : 'cisco_cpu_mem';
    const u = num(`${prefix}_used`);
    const f = num(`${prefix}_free`);
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
 *  (the labels themselves are localized) — an unknown metric falls back to its raw name.
 *
 *  Exported so `i18nEnumKeys.test.ts` can pin it to the locale files: listing a name here without
 *  adding its strings does not fall back, it shows the operator the literal key
 *  `format:scalar.<name>` — worse than the raw metric name the fallback would have given. */
export const KNOWN_SCALARS = new Set<string>([
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

/** How a scalar metric is named: a localized label when Yagra knows it, else its raw metric name
 *  (which renders mono, being OID-ish). Split out of [`scalarDisplay`] because the Overview's
 *  metric cards need the name without having a value to render yet. */
export function scalarLabel(metric: string): { label: string; known: boolean } {
  const known = KNOWN_SCALARS.has(metric);
  return { label: known ? i18n.t(`format:scalar.${metric}`) : metric, known };
}

/** What a metric's number *is*, as a kind and a payload — or `null` when it has no unit.
 *
 *  Read straight off `api/metricUnits.json`, which is **generated** from
 *  `crates/yagra-core/src/metric_meaning.rs` (ADR-046 Inc.7). Nothing here is hand-kept in step
 *  with Rust, and there is deliberately no rule from the metric's *name*: Inc.6 決定 J refused a
 *  `_pct` / `_ms` suffix rule because `huawei_cpu_usage` and `huawei_mem_usage` are percentages
 *  with no suffix at all. All 108 rows were written by hand, in Rust.
 *
 *  ⚠️ **This lives here rather than in `metricMeaning.ts`** — that module imports this one, so the
 *  dependency cannot run the other way. It is also the right address: the per-metric display
 *  registry (`KNOWN_SCALARS`, `scalarLabel`, `scalarValueFormat`) is already here. */
export function metricUnit(
  metric: string,
): { kind: 'symbol' | 'counted' | 'scaled'; unit: string } | null {
  for (const kind of ['symbol', 'counted', 'scaled'] as const) {
    const unit = (metricUnits as Record<string, Record<string, string>>)[kind]?.[metric];
    if (unit != null) return { kind, unit };
  }
  return null;
}

/** The suffix to put after a metric's value, already localized — or `null` for no suffix.
 *
 *  `symbol` is the same in every language and goes through verbatim. `counted` is a **noun**
 *  (`sessions`, `users`, `days`) and is looked up under `format:unit.*`, which is why anything
 *  language-dependent is a counted unit in Rust and not a symbol. `scaled` returns `null`: those
 *  values are rescaled by [`scalarValueFormat`], which owns the whole rendered string, so a suffix
 *  here would print twice (`15.6 GB kilobytes`). */
export function metricUnitSuffix(metric: string): string | null {
  const u = metricUnit(metric);
  if (!u) return null;
  if (u.kind === 'symbol') return u.unit;
  if (u.kind === 'counted') return i18n.t(`format:unit.${u.unit}`);
  return null;
}

/** Is this metric a 0–100 percentage, so a chart of it should be pinned to that range?
 *
 *  ADR-046 Inc.6 決定 J deliberately left every generic card auto-fitting, on the grounds that
 *  "nothing in the API says a metric is a percentage". Inc.7 makes something say so, and the
 *  reason to act on it is **consistency rather than accuracy**: `huawei_cpu_usage` is pinned to
 *  0–100 when Device health resolves onto it and auto-fitted when it falls through to the generic
 *  list, so today the same metric on two devices draws two different charts.
 *
 *  Returns a boolean, not a `MetricScale`: that union belongs to `metricCards.ts`, and spelling it
 *  again here would be a second copy of it. */
export function isPercentMetric(metric: string): boolean {
  const u = metricUnit(metric);
  return u?.kind === 'symbol' && u.unit === '%';
}

/** Units that sit tight against the number; everything else takes a space.
 *
 *  Two members, and both are conventions rather than choices: `50%` and `12/s` are wrong with a
 *  space, `2 ms` and `59 °C` are wrong without one. Kept as a set in one place rather than as a
 *  leading space baked into each unit string — a `' ms'` in a 108-row table is a difference nobody
 *  can see while reading it. */
const TIGHT_UNITS = new Set(['%', '/s']);

/** Join an already-formatted number to its unit, with the spacing that unit takes. */
export function withUnit(value: string, unit: string | null | undefined): string {
  if (!unit) return value;
  return TIGHT_UNITS.has(unit) ? `${value}${unit}` : `${value} ${unit}`;
}

/** The formatter for a scalar whose **stored number is not the number to show**, or `undefined`
 *  when the raw value is the value.
 *
 *  **Membership is not a list here** — it is every metric Rust marked `MetricUnit::Scaled`, read
 *  off the generated table. A second hand-written copy of "which metrics need converting" is
 *  exactly the mirror that rots, and it would rot in the direction that shows an odometer as a
 *  rate (the ADR-012 accident, which ADR-046 Inc.6 already had to undo once).
 *
 *  🚨 **The rescaled number is not the number a threshold rule takes.** A bound is written in the
 *  metric's stored unit (`alerts/rules.rs::lowest_bound`), so the card can read `15.6 GB` where the
 *  rule takes `16000000`, and `1.00` where it takes `100`. That was already true of
 *  `snmp_sys_uptime_ticks` alone; ADR-046 Inc.7 made it true of seventeen metrics, deliberately,
 *  and the deferral to fix the rule side carries its unblocking condition in `backlog.md`.
 *
 *  Exported rather than inlined into [`scalarDisplay`] because the Overview's metric card needs the
 *  same rule for its headline and its hover readout. */
export function scalarValueFormat(metric: string): ((v: number) => string) | undefined {
  const u = metricUnit(metric);
  if (u?.kind !== 'scaled') return undefined;
  switch (u.unit) {
    case 'bytes':
      return (v) => formatBytes(v);
    // Not `formatKb`: that one is the Meraki windowed-usage gauge and keeps a decimal
    // unconditionally. These are memory and swap totals, and they have to read the same way the
    // MEMORY card beside them does.
    case 'kilobytes':
      return (v) => formatBytes(v * 1024);
    case 'hundredths of a second':
      return formatUptimeTicks;
    // A load average is dimensionless, so there is no suffix to add — only the ÷100 that laLoadInt
    // needs. Two decimals because that is how every other tool prints one (`1.00`, not `1`).
    case 'hundredths of a load average':
      return (v) => (Number.isFinite(v) ? (v / 100).toFixed(2) : '—');
    default:
      return undefined;
  }
}

/** A known scalar gets a localized label + formatted value (and renders in the UI font, not mono);
 *  an unknown one keeps its raw OID-ish metric name + numeric value (mono). */
export function scalarDisplay(metric: string, value: number): {
  label: string;
  value: string;
  known: boolean;
} {
  const { label, known } = scalarLabel(metric);
  const fmt = scalarValueFormat(metric);
  // The unit goes on here too, not only on the Overview card, so the Collection tab and the
  // Overview do not disagree about the same metric on the same screen. A rescaled value never
  // doubles up: `metricUnitSuffix` returns null for exactly the metrics `fmt` is non-undefined for.
  return {
    label,
    value: withUnit(fmt ? fmt(value) : String(value), metricUnitSuffix(metric)),
    known,
  };
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
  | {
      kind: 'metric';
      metric: string;
      condition: string | null;
      observed: string | null;
      /** SNMP ifIndex when the alert is about one port rather than the node (ADR-076).
       *  A number, not a name: the alert carries the index, and the name is resolved by the
       *  surface that has the node's interface roster. `null` is the ordinary node-level case. */
      ifindex: number | null;
    };

export function alertWhat(row: {
  metric?: string | null;
  direction?: string | null;
  threshold_value?: number | null;
  observed_value?: number | null;
  ifindex?: number | null;
}): AlertWhat {
  if (!row.metric) return { kind: 'none' };
  if (row.metric === LIVENESS_METRIC) return { kind: 'liveness' };
  // Both numbers go through `formatSi`, and both used to be interpolated raw. A metric sample is a
  // double: the live PoC alert this was found on read `(was 83.86047908238002)`, seventeen
  // significant digits of a memory percentage, which on a phone crowds the metric name and the
  // bound — the two parts that actually say what is wrong — off the line. Rounding is not cosmetic
  // here: this span IS the explanation, so anything that costs it legibility costs it its job.
  //
  // The direction is a token off the wire (`above`/`below`), not English prose, and it was being
  // printed verbatim. It has had localized labels since thresholds got a UI; the reason it needs a
  // template rather than `${dir} ${value}` is word order — English puts the direction first
  // ("above 80"), Japanese puts it last ("80 を上回る"), so the two locales cannot share one
  // concatenation.
  const condition =
    row.direction && row.threshold_value != null
      ? i18n.t('format:alertCondition', {
          direction: i18n.t(`alertsConfig:thresholds.direction.${row.direction}`, {
            defaultValue: row.direction,
          }),
          value: formatSi(row.threshold_value),
        })
      : null;
  const observed =
    row.observed_value != null
      ? i18n.t('format:alertObservedWas', { value: formatSi(row.observed_value) })
      : null;
  return {
    kind: 'metric',
    metric: row.metric,
    condition,
    observed,
    ifindex: row.ifindex ?? null,
  };
}

/** Same question for a **live** alert. The two carry the identical fact in two shapes: history
 *  flattens the breach into `direction`/`threshold_value`/`observed_value` columns (migration 0036)
 *  while the live `Alert` keeps `breach` nested. Adapt rather than duplicate `alertWhat`, so the
 *  triage screen and the history log can never describe the same breach differently. */
export function alertWhatOf(alert: {
  metric?: string | null;
  breach?: { value?: number; threshold?: number | null; direction?: string } | null;
  ifindex?: number | null;
}): AlertWhat {
  return alertWhat({
    metric: alert.metric,
    direction: alert.breach?.direction,
    threshold_value: alert.breach?.threshold,
    observed_value: alert.breach?.value,
    ifindex: alert.ifindex,
  });
}

/** An autonomous-system label: `AS15169 · GOOGLE` (with name) or `AS15169` (number only), or
 *  `null` when the ASN is unknown/absent (0) — callers omit the AS line in that case. The org
 *  name is device/registry data, shown verbatim (not localized). */
export function formatAsn(asn?: number, name?: string | null): string | null {
  if (!asn) return null;
  return name ? `AS${asn} · ${name}` : `AS${asn}`;
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

export const agoSec = (sec: number | null): string =>
  sec == null ? '—' : relativeTime(new Date(sec * 1000).toISOString());

export const toRfc3339 = (local: string) => new Date(local).toISOString();

export function timeValue(hour: number, minute: number): string {
  return `${String(hour).padStart(2, '0')}:${String(minute).padStart(2, '0')}`;
}
