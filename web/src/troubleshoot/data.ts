// Troubleshoot — static catalog data and finding fixtures (lifted from the design handoff
// `TOOLS`/`METHODS`/`KINDS`/`ANOMS` arrays). These are the deep-diagnostic tools and their
// sample reports. There is no jobs/findings API yet, so the report data is mock; once the
// backend lands, the run list and Anomaly findings come from it (see handoff "Data needs").
//
// Method and anomaly-kind colors come from the categorical SERIES palette (tokens.css) — they
// classify, they are NOT status and NOT brand. Reference the token, never a raw hex.

/** Analysis technique behind a tool — colors it on the card (categorical, series palette). */
export type Method = 'stat' | 'ml' | 'topo' | 'probe';

export interface MethodMeta {
  label: string;
  /** A `var(--series-N)` token — categorical, carries no status meaning. */
  color: string;
}

export const METHODS: Record<Method, MethodMeta> = {
  stat: { label: 'Statistical', color: 'var(--series-1)' },
  ml: { label: 'ML', color: 'var(--series-2)' },
  topo: { label: 'Topology', color: 'var(--series-3)' },
  probe: { label: 'Active probe', color: 'var(--series-4)' },
};

export interface ToolLatest {
  text: string;
  when: string;
}

export interface Tool {
  /** Stable id for keys / drawer selection. */
  id: string;
  /** Monospace monogram tile (2 chars). */
  mono: string;
  name: string;
  method: Method;
  /** Compute depth 1–5 (drives the pip indicator). */
  depth: number;
  /** Human estimate, e.g. "~2–4 min". */
  est: string;
  /** Scope hint shown under the drawer's scope field. */
  scope: string;
  desc: string;
  /** "Surfaces …" reveal line. */
  reveal: string;
  /** Most recent run summary, if any. */
  latest?: ToolLatest;
  /** Report route, only for tools whose report screen exists (Anomaly today). */
  reportPath?: string;
}

export const TOOLS: Tool[] = [
  {
    id: 'anomaly',
    mono: 'An',
    name: 'Anomaly Detection',
    method: 'ml',
    depth: 4,
    est: '~2–4 min',
    scope: 'node group · metric families',
    desc: 'Learns each metric’s own baseline and flags statistically significant deviations — slow drifts and odd shapes that fixed thresholds never trip.',
    reveal: 'spikes · level shifts · stuck counters · seasonality breaks',
    latest: { text: '23 anomalies · 8 nodes', when: '8m ago' },
    reportPath: '/troubleshoot/anomaly',
  },
  {
    id: 'correlation',
    mono: 'Co',
    name: 'Event Correlation',
    method: 'stat',
    depth: 3,
    est: '~1–3 min',
    scope: 'incident time window',
    desc: 'Cross-correlates metrics, alerts and config events inside an incident window to surface what actually moved together — not just what alarmed.',
    reveal: 'co-moving series · lead/lag relationships',
  },
  {
    id: 'capacity',
    mono: 'Cf',
    name: 'Capacity Forecast',
    method: 'stat',
    depth: 4,
    est: '~2–5 min',
    scope: 'interfaces / resources · 90d',
    desc: 'Projects interface and resource utilization forward with confidence bands to estimate when each will hit exhaustion.',
    reveal: 'time-to-exhaustion · growth rate',
    latest: { text: '3 links < 60d', when: '8m ago' },
  },
  {
    id: 'flap',
    mono: 'Fl',
    name: 'Flap Analysis',
    method: 'stat',
    depth: 2,
    est: '~1–2 min',
    scope: 'links / interfaces · 24h–7d',
    desc: 'Scans link and interface state churn to surface flapping that averages out to “up” on a dashboard but quietly drops sessions.',
    reveal: 'flap rate · churn windows',
    latest: { text: '5 interfaces flapping', when: '15m ago' },
  },
];

export function toolById(id: string): Tool | undefined {
  return TOOLS.find((t) => t.id === id);
}

// ---- Analysis runs (async jobs) ------------------------------------------------------------

export type RunState = 'running' | 'done' | 'failed';

/** A completed run's headline result: a bold count + trailing text (modeled as data, not HTML,
 *  so it renders through JSX without dangerouslySetInnerHTML — device/result text is untrusted). */
export interface RunFindings {
  count: number;
  text: string;
}

export interface Run {
  id: string;
  tool: string;
  mono: string;
  scope: string;
  state: RunState;
  /** running: 0–100 progress. */
  pct?: number;
  /** running: current phase caption. */
  phase?: string;
  /** running: ETA caption. */
  eta?: string;
  /** running: when it started (relative). */
  started?: string;
  /** done: headline findings. */
  findings?: RunFindings;
  /** failed: reason. */
  err?: string;
  /** done/failed: when it finished (relative). */
  when?: string;
  /** Report route for a viewable result (Anomaly today); absent ⇒ no report screen yet. */
  reportPath?: string;
}

/** Seed runs mirroring the handoff `RUNS` array (two live, two done, one failed). */
export const INITIAL_RUNS: Run[] = [
  {
    id: 'r1',
    tool: 'Anomaly Detection',
    mono: 'An',
    scope: 'group: Matsumoto / core (18 nodes) · 24h',
    state: 'running',
    pct: 62,
    phase: 'Scoring residuals…',
    eta: '~1m 20s',
    started: '2m ago',
    reportPath: '/troubleshoot/anomaly',
  },
  {
    id: 'r2',
    tool: 'Event Correlation',
    mono: 'Co',
    scope: 'edge-tok-fw01 · incident #4821 · ±30 min',
    state: 'running',
    pct: 34,
    phase: 'Cross-correlating 312 series…',
    eta: '~1m 50s',
    started: '1m ago',
  },
  {
    id: 'r3',
    tool: 'Capacity Forecast',
    mono: 'Cf',
    scope: 'WAN uplinks (32 interfaces) · 90d',
    state: 'done',
    findings: { count: 3, text: 'links < 60d to exhaustion' },
    when: '8m ago',
  },
  {
    id: 'r4',
    tool: 'Flap Analysis',
    mono: 'Fl',
    scope: 'access switches (44 nodes) · 7d',
    state: 'done',
    findings: { count: 5, text: 'interfaces flapping · 2 chronic' },
    when: '15m ago',
  },
  {
    id: 'r5',
    tool: 'Anomaly Detection',
    mono: 'An',
    scope: 'role: edge firewalls (9) · 24h',
    state: 'failed',
    err: 'baseline gap — <14d history on 3 nodes',
    when: '41m ago',
    reportPath: '/troubleshoot/anomaly',
  },
];

// ---- Anomaly findings (Anomaly Detection report) -------------------------------------------

/** Anomaly shape — colors the kind chip and the redrawn anomalous segment (series palette). */
export type Kind = 'spike' | 'level' | 'drift' | 'flat' | 'season';

export interface KindMeta {
  label: string;
  color: string;
}

export const KINDS: Record<Kind, KindMeta> = {
  spike: { label: 'Spike', color: 'var(--series-4)' },
  level: { label: 'Level shift', color: 'var(--series-5)' },
  drift: { label: 'Trend drift', color: 'var(--series-6)' },
  flat: { label: 'Stuck / flatline', color: 'var(--series-3)' },
  season: { label: 'Seasonality break', color: 'var(--series-2)' },
};

/** Severity derived from score: ≥90 critical, ≥75 warning, else info (handoff §4). */
export type Severity = 'crit' | 'warn' | 'info';

export function severityFor(score: number): Severity {
  if (score >= 90) return 'crit';
  if (score >= 75) return 'warn';
  return 'info';
}

export interface Anomaly {
  score: number;
  sev: Severity;
  node: string;
  metric: string;
  kind: Kind;
  when: string;
  dur: string;
  /** Deterministic chart seed (the prototype's hand-drawn sparkline shape). */
  seed: number;
}

export const ANOMS: Anomaly[] = [
  { score: 98, sev: 'crit', node: 'edge-tok-fw01', metric: 'icmp_rtt_ms', kind: 'spike', when: 'today 03:12', dur: '6 min', seed: 11 },
  { score: 94, sev: 'crit', node: 'dist-mat-sw03', metric: 'ifInErrors · Gi0/3', kind: 'level', when: 'today 02:48', dur: 'ongoing', seed: 23 },
  { score: 91, sev: 'crit', node: 'core-mat-rt01', metric: 'cpu_load_1m', kind: 'drift', when: 'since 22:00', dur: '5h', seed: 31 },
  { score: 88, sev: 'warn', node: 'acc-osk-sw12', metric: 'ifHCOutOctets · Gi1/14', kind: 'flat', when: 'today 01:05', dur: '3h', seed: 42 },
  { score: 84, sev: 'warn', node: 'dist-nag-sw01', metric: 'mem_used_pct', kind: 'drift', when: '3d', dur: 'slow leak', seed: 53 },
  { score: 79, sev: 'warn', node: 'core-osk-rt02', metric: 'tcp_retrans', kind: 'season', when: 'today 09:30', dur: '40 min', seed: 64 },
  { score: 73, sev: 'info', node: 'acc-osk-sw07', metric: 'temp_celsius', kind: 'spike', when: 'today 08:14', dur: '12 min', seed: 75 },
  { score: 68, sev: 'info', node: 'dist-nag-sw04', metric: 'disk_io_wait', kind: 'level', when: 'today 06:20', dur: '2h', seed: 86 },
  { score: 61, sev: 'info', node: 'edge-osk-fw02', metric: 'ifOutDiscards · Te0/1', kind: 'season', when: 'today 07:45', dur: '25 min', seed: 97 },
];
