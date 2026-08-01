// SPDX-License-Identifier: AGPL-3.0-only
// The URL/DNS monitor edit form's judgement: round-tripping a stored config through the draft, and
// what a blank or malformed field means. These rules decide what a `PUT` replaces, so a mistake
// here silently reconfigures live monitoring rather than failing loudly.

import { describe, expect, it } from 'vitest';
import {
  dnsBodyFrom,
  dnsDraftFrom,
  urlBodyFrom,
  urlDraftFrom,
  type DnsCheckDraft,
  type UrlCheckDraft,
} from './checkConfigForm';

const urlDraft = (over: Partial<UrlCheckDraft> = {}): UrlCheckDraft => ({
  url: 'https://example.test/health',
  method: 'GET',
  statusMode: 'two_xx',
  statusCodes: '',
  statusLo: '',
  statusHi: '',
  verifyTls: true,
  followRedirects: true,
  timeoutMs: '5000',
  ...over,
});

const dnsDraft = (over: Partial<DnsCheckDraft> = {}): DnsCheckDraft => ({
  name: 'example.test',
  recordType: 'A',
  resolver: '',
  resolverPort: '53',
  maxDepth: '8',
  timeoutMs: '3000',
  ...over,
});

/** Unwrap the ok-branch, failing loudly with the error key when the draft was rejected. */
function body<T>(r: { body: T } | { error: string }): T {
  if ('error' in r) throw new Error(`unexpectedly rejected: ${r.error}`);
  return r.body;
}

describe('urlDraftFrom', () => {
  it('shows the server-side defaults rather than empty boxes', () => {
    // An absent optional means "the default", so the form must say what that default is — a blank
    // timeout box would read as "no timeout".
    const d = urlDraftFrom({ url: 'https://a.test' });
    expect(d).toMatchObject({
      method: 'GET',
      timeoutMs: '5000',
      verifyTls: true,
      followRedirects: true,
      statusMode: 'two_xx',
    });
  });

  it('flattens each expected-status arm into its own fields', () => {
    expect(urlDraftFrom({ url: 'u', expected_status: { kind: 'two_xx' } }).statusMode).toBe('two_xx');

    const exact = urlDraftFrom({ url: 'u', expected_status: { kind: 'exact', codes: [200, 204] } });
    expect(exact).toMatchObject({ statusMode: 'exact', statusCodes: '200, 204' });

    const range = urlDraftFrom({ url: 'u', expected_status: { kind: 'range', lo: 200, hi: 299 } });
    expect(range).toMatchObject({ statusMode: 'range', statusLo: '200', statusHi: '299' });
  });

  it('round-trips a fully-specified config unchanged', () => {
    const cfg = {
      url: 'https://api.test/health',
      method: 'HEAD' as const,
      expected_status: { kind: 'exact' as const, codes: [200] },
      verify_tls: false,
      follow_redirects: false,
      timeout_ms: 1234,
    };
    expect(body(urlBodyFrom(urlDraftFrom(cfg)))).toEqual(cfg);
  });
});

describe('urlBodyFrom', () => {
  it('sends every field, because a PUT replaces rather than patches', () => {
    // Omitting a field here would reset it to the server default instead of keeping what the
    // operator is looking at.
    const b = body(urlBodyFrom(urlDraft({ verifyTls: false })));
    expect(Object.keys(b).sort()).toEqual(
      ['expected_status', 'follow_redirects', 'method', 'timeout_ms', 'url', 'verify_tls'].sort(),
    );
    expect(b.verify_tls).toBe(false);
  });

  it('leaves expected_status undefined for the any-2xx default', () => {
    expect(body(urlBodyFrom(urlDraft())).expected_status).toBeUndefined();
  });

  it('rejects a missing or schemeless URL', () => {
    expect(urlBodyFrom(urlDraft({ url: '   ' }))).toEqual({ error: 'urlRequired' });
    expect(urlBodyFrom(urlDraft({ url: 'example.test' }))).toEqual({ error: 'urlScheme' });
  });

  it('trims the URL', () => {
    expect(body(urlBodyFrom(urlDraft({ url: '  https://a.test  ' }))).url).toBe('https://a.test');
  });

  it('rejects a non-positive or non-numeric timeout', () => {
    for (const timeoutMs of ['', '0', '-5', 'abc', '1.5']) {
      expect(urlBodyFrom(urlDraft({ timeoutMs }))).toEqual({ error: 'timeout' });
    }
  });

  it('parses an exact code list and rejects a malformed one', () => {
    expect(body(urlBodyFrom(urlDraft({ statusMode: 'exact', statusCodes: '200, 201 ,204' })))
      .expected_status).toEqual({ kind: 'exact', codes: [200, 201, 204] });
    // Empty, out of range, or not a number at all.
    for (const statusCodes of ['', '  ', '99', '600', '20x']) {
      expect(urlBodyFrom(urlDraft({ statusMode: 'exact', statusCodes }))).toEqual({
        error: 'statusCodes',
      });
    }
  });

  it('parses a status range and rejects an inverted or out-of-bounds one', () => {
    expect(body(urlBodyFrom(urlDraft({ statusMode: 'range', statusLo: '200', statusHi: '299' })))
      .expected_status).toEqual({ kind: 'range', lo: 200, hi: 299 });
    const bad = [
      { statusLo: '299', statusHi: '200' }, // inverted
      { statusLo: '99', statusHi: '200' }, // below 100
      { statusLo: '200', statusHi: '600' }, // above 599
      { statusLo: '', statusHi: '299' }, // blank
    ];
    for (const over of bad) {
      expect(urlBodyFrom(urlDraft({ statusMode: 'range', ...over }))).toEqual({
        error: 'statusRange',
      });
    }
  });
});

describe('dnsDraftFrom', () => {
  it('shows the server-side defaults and an empty resolver for "system"', () => {
    const d = dnsDraftFrom({ name: 'a.test' });
    expect(d).toMatchObject({
      recordType: 'A',
      resolver: '',
      resolverPort: '53',
      maxDepth: '8',
      timeoutMs: '3000',
    });
  });

  it('round-trips a fully-specified config unchanged', () => {
    const cfg = {
      name: 'api.example.test',
      record_type: 'AAAA' as const,
      resolver: '1.1.1.1',
      resolver_port: 5353,
      max_depth: 4,
      timeout_ms: 2000,
    };
    expect(body(dnsBodyFrom(dnsDraftFrom(cfg)))).toEqual(cfg);
  });
});

describe('dnsBodyFrom', () => {
  it('normalizes the name the way the server stores it', () => {
    // Otherwise reopening the dialog shows a different string than the one just saved.
    expect(body(dnsBodyFrom(dnsDraft({ name: '  API.Example.Test.  ' }))).name).toBe(
      'api.example.test',
    );
  });

  it('rejects an empty name', () => {
    expect(dnsBodyFrom(dnsDraft({ name: '  ' }))).toEqual({ error: 'dnsNameRequired' });
  });

  it('sends null for a blank resolver so the poller uses its own', () => {
    // `''` would ask the poller to resolve against no server at all.
    expect(body(dnsBodyFrom(dnsDraft({ resolver: '   ' }))).resolver).toBeNull();
    expect(body(dnsBodyFrom(dnsDraft({ resolver: ' 9.9.9.9 ' }))).resolver).toBe('9.9.9.9');
  });

  it('rejects an out-of-range port and a non-positive depth or timeout', () => {
    expect(dnsBodyFrom(dnsDraft({ resolverPort: '0' }))).toEqual({ error: 'resolverPort' });
    expect(dnsBodyFrom(dnsDraft({ resolverPort: '65536' }))).toEqual({ error: 'resolverPort' });
    expect(dnsBodyFrom(dnsDraft({ maxDepth: '0' }))).toEqual({ error: 'maxDepth' });
    expect(dnsBodyFrom(dnsDraft({ timeoutMs: 'abc' }))).toEqual({ error: 'timeout' });
  });
});
