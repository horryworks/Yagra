// SPDX-License-Identifier: AGPL-3.0-only
// Turning a stored URL/DNS monitor config into editable form fields and back.
//
// Until this existed the two configs were write-once: the add-node dialog created them and the
// node detail displayed them, so changing a URL's timeout or a DNS resolver meant deleting the
// node and recreating it. The `GET`/`PUT`/`DELETE` endpoints were there the whole time with no
// caller.
//
// Pure and in a `.ts` so the rules are unit-tested (Vitest never runs `.tsx`): every field is a
// string in the draft because that is what an `<input>` holds, and turning "" / "8" / "abc" back
// into the typed body is exactly the judgement worth testing. The component beside it renders.

import type {
  BodyMatch,
  DnsCheckConfig,
  DnsRecordType,
  ExpectedStatus,
  JsonExtract,
  UrlCheckConfig,
} from '../../types/api';

/** The default each optional field falls back to server-side, restated where the form needs a
 *  concrete value to show. Kept beside the drafts so "what does blank mean" has one answer. */
const URL_DEFAULTS = { method: 'GET', timeoutMs: 5000, verifyTls: true, followRedirects: true } as const;
const DNS_DEFAULTS = { recordType: 'A', resolverPort: 53, maxDepth: 8, timeoutMs: 3000 } as const;

/** How the operator says which HTTP statuses are healthy. Mirrors `ExpectedStatus`'s three arms. */
export const EXPECTED_STATUS_MODES = ['two_xx', 'exact', 'range'] as const;
export type ExpectedStatusMode = (typeof EXPECTED_STATUS_MODES)[number];

/** Whether the body keyword must be present or absent. Mirrors Rust's `BodyMatchMode`; an `as const`
 *  array because the modal builds a `t()` key from it (extensibility.md §4). */
export const BODY_MATCH_MODES = ['contains', 'not_contains'] as const;
export type BodyMatchMode = (typeof BODY_MATCH_MODES)[number];

/** Body-rule bounds, restated so the form can say *why* before a round trip.
 *
 *  ⚠️ A mirror of `yagra_common::{BODY_PATTERN_MAX_LEN, BODY_MAX_BYTES_RANGE,
 *  DEFAULT_BODY_MAX_BYTES, MAX_JSON_EXTRACTS, JSON_PATH_MAX_LEN, METRIC_NAME_MAX_LEN}` with nothing
 *  pinning the two together — the same deliberate copy the 100–599 status bounds above already are.
 *  The **server is authoritative**: it refuses an out-of-range rule regardless, so a drifted copy
 *  costs a worse error message, never a bad write. If these ever need to be enforced rather than
 *  explained, generate them instead of widening this block. */
const BODY_LIMITS = {
  patternMaxLen: 512,
  minBytes: 1024,
  maxBytes: 1024 * 1024,
  maxExtracts: 8,
  pathMaxLen: 256,
  metricMaxLen: 96,
} as const;
const BODY_DEFAULTS = { mode: 'contains', maxBytes: 64 * 1024 } as const;

/** Metric names a URL monitor reports on its own, which an extraction rule may not claim.
 *
 *  ⚠️ Mirrors `yagra_common::url_monitor_reserved_metrics()`. Claiming one would overwrite the
 *  node's own availability series with a number out of a JSON body; the server refuses it too, so a
 *  drifted copy here costs a late error message rather than a bad write. */
const RESERVED_METRICS: readonly string[] = [
  'http_up',
  'http_status_code',
  'http_response_time_ms',
  'ssl_cert_days_to_expiry',
  'http_body_match',
  'http_body_truncated',
];

/** The TSDB metric-name grammar (`[A-Za-z_:][A-Za-z0-9_:]*`), mirroring
 *  `yagra_common::is_valid_metric_name`. */
const METRIC_NAME_RE = /^[A-Za-z_:][A-Za-z0-9_:]*$/;

/** The URL-monitor edit form, as the inputs hold it. */
export interface UrlCheckDraft {
  url: string;
  method: 'GET' | 'HEAD' | 'POST';
  statusMode: ExpectedStatusMode;
  /** Comma-separated codes, for `exact`. */
  statusCodes: string;
  /** Inclusive bounds, for `range`. */
  statusLo: string;
  statusHi: string;
  verifyTls: boolean;
  followRedirects: boolean;
  timeoutMs: string;
  /** Bound auth credential id; empty string means none. */
  credentialId: string;
  /** Whether a response-body keyword rule is configured at all. Off is not the same as an empty
   *  pattern: off means the probe never reads the body. */
  bodyMatchEnabled: boolean;
  bodyPattern: string;
  bodyMode: BodyMatchMode;
  /** How many bytes of the body to read before giving up. Shared by both body features. */
  bodyMaxBytes: string;
  /** JSON extraction rules, as editable rows. An all-blank row is dropped rather than rejected —
   *  it is what an operator leaves behind after clicking "add" and changing their mind. */
  extracts: JsonExtractDraft[];
}

/** One extraction row, as the inputs hold it. */
export interface JsonExtractDraft {
  metric: string;
  path: string;
}

/** The DNS-monitor edit form, as the inputs hold it. */
export interface DnsCheckDraft {
  name: string;
  recordType: DnsRecordType;
  /** Blank ⇒ the poller's own system resolver. */
  resolver: string;
  resolverPort: string;
  maxDepth: string;
  timeoutMs: string;
}

/** Parse a positive integer field; `null` when blank or not a whole number ≥ 1. */
function positiveInt(raw: string): number | null {
  const t = raw.trim();
  if (!/^\d+$/.test(t)) return null;
  const n = Number(t);
  return n >= 1 ? n : null;
}

/** Split the stored `ExpectedStatus` union into the three flat fields the form edits. */
function statusFields(s: ExpectedStatus | undefined): Pick<
  UrlCheckDraft,
  'statusMode' | 'statusCodes' | 'statusLo' | 'statusHi'
> {
  const blank = { statusCodes: '', statusLo: '', statusHi: '' };
  if (!s || s.kind === 'two_xx') return { statusMode: 'two_xx', ...blank };
  if (s.kind === 'exact') {
    return { statusMode: 'exact', ...blank, statusCodes: s.codes.join(', ') };
  }
  return { statusMode: 'range', ...blank, statusLo: String(s.lo), statusHi: String(s.hi) };
}

/** A stored URL config → the editable draft. Absent optionals show their server-side default, so
 *  the form never presents an empty box that silently means something. */
export function urlDraftFrom(cfg: UrlCheckConfig): UrlCheckDraft {
  return {
    url: cfg.url,
    method: cfg.method ?? URL_DEFAULTS.method,
    ...statusFields(cfg.expected_status),
    verifyTls: cfg.verify_tls ?? URL_DEFAULTS.verifyTls,
    followRedirects: cfg.follow_redirects ?? URL_DEFAULTS.followRedirects,
    timeoutMs: String(cfg.timeout_ms ?? URL_DEFAULTS.timeoutMs),
    credentialId: cfg.credential ?? '',
    bodyMatchEnabled: cfg.body_match != null,
    // The inputs keep their defaults while the rule is off, so toggling it on presents a usable
    // form rather than blank boxes the operator has to guess the meaning of.
    bodyPattern: cfg.body_match?.pattern ?? '',
    bodyMode: cfg.body_match?.mode ?? BODY_DEFAULTS.mode,
    bodyMaxBytes: String(cfg.body_max_bytes ?? BODY_DEFAULTS.maxBytes),
    extracts: (cfg.json_extract ?? []).map((e) => ({ metric: e.metric, path: e.path })),
  };
}

/** A stored DNS config → the editable draft. */
export function dnsDraftFrom(cfg: DnsCheckConfig): DnsCheckDraft {
  return {
    name: cfg.name,
    recordType: cfg.record_type ?? DNS_DEFAULTS.recordType,
    resolver: cfg.resolver ?? '',
    resolverPort: String(cfg.resolver_port ?? DNS_DEFAULTS.resolverPort),
    maxDepth: String(cfg.max_depth ?? DNS_DEFAULTS.maxDepth),
    timeoutMs: String(cfg.timeout_ms ?? DNS_DEFAULTS.timeoutMs),
  };
}

/** Build the `expected_status` union from the flat fields, or name what is wrong with them. */
function expectedStatusFrom(d: UrlCheckDraft): { ok: ExpectedStatus | undefined } | { err: string } {
  if (d.statusMode === 'two_xx') return { ok: undefined };
  if (d.statusMode === 'exact') {
    const parts = d.statusCodes
      .split(',')
      .map((s) => s.trim())
      .filter((s) => s !== '');
    const codes = parts.map(positiveInt);
    if (parts.length === 0 || codes.some((c) => c === null || c < 100 || c > 599)) {
      return { err: 'statusCodes' };
    }
    return { ok: { kind: 'exact', codes: codes as number[] } };
  }
  const lo = positiveInt(d.statusLo);
  const hi = positiveInt(d.statusHi);
  if (lo === null || hi === null || lo < 100 || hi > 599 || lo > hi) return { err: 'statusRange' };
  return { ok: { kind: 'range', lo, hi } };
}

/** Build the body-keyword rule, or name what is wrong with it. `undefined` ⇒ no rule configured. */
function bodyMatchFrom(d: UrlCheckDraft): { ok: BodyMatch | undefined } | { err: string } {
  if (!d.bodyMatchEnabled) return { ok: undefined };
  const pattern = d.bodyPattern.trim();
  // An empty keyword is contained by every string: `contains` would always pass and `not_contains`
  // would always fail, so the monitor would stop reporting on anything real.
  if (pattern === '') return { err: 'bodyPatternRequired' };
  if (pattern.length > BODY_LIMITS.patternMaxLen) return { err: 'bodyPatternTooLong' };
  return { ok: { pattern, mode: d.bodyMode } };
}

/** Rows the operator actually filled in. A row where both boxes are blank is what "add" leaves
 *  behind when someone changes their mind, so it is dropped rather than rejected — but a row with
 *  only *one* box filled is a mistake and must fail loudly below. */
function filledExtracts(d: UrlCheckDraft): JsonExtractDraft[] {
  return d.extracts.filter((e) => e.metric.trim() !== '' || e.path.trim() !== '');
}

/** Build the extraction rules, or name the first thing wrong with them. */
function jsonExtractFrom(d: UrlCheckDraft): { ok: JsonExtract[] } | { err: string } {
  const rows = filledExtracts(d);
  if (rows.length === 0) return { ok: [] };
  if (rows.length > BODY_LIMITS.maxExtracts) return { err: 'tooManyExtracts' };
  const out: JsonExtract[] = [];
  const seen = new Set<string>();
  for (const row of rows) {
    const metric = row.metric.trim();
    const path = row.path.trim();
    if (metric === '') return { err: 'extractMetricRequired' };
    if (path === '') return { err: 'extractPathRequired' };
    if (!METRIC_NAME_RE.test(metric) || metric.length > BODY_LIMITS.metricMaxLen) {
      return { err: 'extractMetricName' };
    }
    // Claiming one of these would overwrite the node's own availability series with a number the
    // monitored service chose — monitoring reporting whatever it is told.
    if (RESERVED_METRICS.includes(metric)) return { err: 'extractMetricReserved' };
    // Two rules writing one series makes the value depend on rule order, which is not something an
    // operator should have to reason about.
    if (seen.has(metric)) return { err: 'extractMetricDuplicate' };
    if (path.length > BODY_LIMITS.pathMaxLen) return { err: 'extractPathTooLong' };
    seen.add(metric);
    out.push({ metric, path });
  }
  return { ok: out };
}

/** The draft → the `PUT` body, or the first reason it cannot be sent.
 *
 *  Everything is sent explicitly rather than omitted-when-default: this is a *replace*, so leaving
 *  a field out would reset it to the server default instead of keeping what the operator sees. */
export function urlBodyFrom(d: UrlCheckDraft): { body: UrlCheckConfig } | { error: string } {
  const url = d.url.trim();
  if (url === '') return { error: 'urlRequired' };
  if (!/^https?:\/\//i.test(url)) return { error: 'urlScheme' };
  const timeout_ms = positiveInt(d.timeoutMs);
  if (timeout_ms === null) return { error: 'timeout' };
  const status = expectedStatusFrom(d);
  if ('err' in status) return { error: status.err };
  const bodyMatch = bodyMatchFrom(d);
  if ('err' in bodyMatch) return { error: bodyMatch.err };
  const extracts = jsonExtractFrom(d);
  if ('err' in extracts) return { error: extracts.err };
  // The budget belongs to the monitor, not to either feature, so it is validated whenever it is
  // sent — a value stored today is already sane when a rule is added tomorrow.
  const body_max_bytes = positiveInt(d.bodyMaxBytes);
  if (
    body_max_bytes === null ||
    body_max_bytes < BODY_LIMITS.minBytes ||
    body_max_bytes > BODY_LIMITS.maxBytes
  ) {
    return { error: 'bodyMaxBytes' };
  }
  // A HEAD response has no body, so neither feature could ever produce a reading — the monitor
  // would alert forever and look like a real outage. The server refuses this too; saying it here
  // says why before a round trip.
  if (d.method === 'HEAD' && (bodyMatch.ok !== undefined || extracts.ok.length > 0)) {
    return { error: 'bodyMatchNeedsBody' };
  }
  return {
    body: {
      url,
      method: d.method,
      expected_status: status.ok,
      verify_tls: d.verifyTls,
      follow_redirects: d.followRedirects,
      timeout_ms,
      // Explicit like every other field, because this PUT is a *replace*: omitting the binding
      // cleared it. That was invisible while nothing consumed the credential — the moment one is
      // used, editing a timeout would have logged the monitor out.
      credential: d.credentialId === '' ? null : d.credentialId,
      // `null`, not omitted, for the same reason — this is how a rule gets *removed*.
      body_match: bodyMatch.ok ?? null,
      json_extract: extracts.ok,
      body_max_bytes,
    },
  };
}

/** The draft → the `PUT` body, or the first reason it cannot be sent. */
export function dnsBodyFrom(d: DnsCheckDraft): { body: DnsCheckConfig } | { error: string } {
  // The server stores the name normalized; normalizing here too keeps the field the operator
  // reopens identical to the one they saved.
  const name = d.name.trim().toLowerCase().replace(/\.$/, '');
  if (name === '') return { error: 'dnsNameRequired' };
  const resolver_port = positiveInt(d.resolverPort);
  if (resolver_port === null || resolver_port > 65535) return { error: 'resolverPort' };
  const max_depth = positiveInt(d.maxDepth);
  if (max_depth === null) return { error: 'maxDepth' };
  const timeout_ms = positiveInt(d.timeoutMs);
  if (timeout_ms === null) return { error: 'timeout' };
  return {
    body: {
      name,
      record_type: d.recordType,
      // Blank ⇒ the poller's system resolver. `''` would ask it to resolve against no server.
      resolver: d.resolver.trim() || null,
      resolver_port,
      max_depth,
      timeout_ms,
    },
  };
}
