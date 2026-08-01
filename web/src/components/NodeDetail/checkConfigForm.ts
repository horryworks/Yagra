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

import type { DnsCheckConfig, DnsRecordType, ExpectedStatus, UrlCheckConfig } from '../../types/api';

/** The default each optional field falls back to server-side, restated where the form needs a
 *  concrete value to show. Kept beside the drafts so "what does blank mean" has one answer. */
const URL_DEFAULTS = { method: 'GET', timeoutMs: 5000, verifyTls: true, followRedirects: true } as const;
const DNS_DEFAULTS = { recordType: 'A', resolverPort: 53, maxDepth: 8, timeoutMs: 3000 } as const;

/** How the operator says which HTTP statuses are healthy. Mirrors `ExpectedStatus`'s three arms. */
export const EXPECTED_STATUS_MODES = ['two_xx', 'exact', 'range'] as const;
export type ExpectedStatusMode = (typeof EXPECTED_STATUS_MODES)[number];

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

/** Why a draft cannot be sent. A key in the `nodes` namespace under `checkEdit.err.`, or `null`
 *  when the draft is valid — the caller resolves it with `t()` so this layer stays i18n-free. */
export type DraftError = string | null;

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
  return {
    body: {
      url,
      method: d.method,
      expected_status: status.ok,
      verify_tls: d.verifyTls,
      follow_redirects: d.followRedirects,
      timeout_ms,
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
