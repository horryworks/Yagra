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
  credentialId: '',
  bodyMatchEnabled: false,
  bodyPattern: '',
  bodyMode: 'contains',
  bodyMaxBytes: '65536',
  extracts: [],
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
      credential: null,
      body_match: null,
      json_extract: [],
      body_max_bytes: 65536,
    };
    expect(body(urlBodyFrom(urlDraftFrom(cfg)))).toEqual(cfg);
  });

  it('reads a stored body rule back into the form, and its absence as "off"', () => {
    const off = urlDraftFrom({ url: 'https://a.test' });
    expect(off.bodyMatchEnabled).toBe(false);
    // The inputs still carry usable defaults while the rule is off, so switching it on presents a
    // form rather than blank boxes whose meaning the operator has to guess.
    expect(off).toMatchObject({ bodyMode: 'contains', bodyMaxBytes: '65536', bodyPattern: '' });

    const on = urlDraftFrom({
      url: 'https://a.test',
      body_match: { pattern: 'Database unavailable', mode: 'not_contains' },
      body_max_bytes: 4096,
    });
    expect(on).toMatchObject({
      bodyMatchEnabled: true,
      bodyPattern: 'Database unavailable',
      bodyMode: 'not_contains',
      bodyMaxBytes: '4096',
    });
  });

  it('reads stored extraction rules back as editable rows', () => {
    expect(urlDraftFrom({ url: 'https://a.test' }).extracts).toEqual([]);
    const d = urlDraftFrom({
      url: 'https://a.test',
      json_extract: [
        { metric: 'queue_depth', path: 'data.queue.depth' },
        { metric: 'workers', path: 'workers.active' },
      ],
    });
    expect(d.extracts).toEqual([
      { metric: 'queue_depth', path: 'data.queue.depth' },
      { metric: 'workers', path: 'workers.active' },
    ]);
  });
});

describe('urlBodyFrom', () => {
  it('sends every field, because a PUT replaces rather than patches', () => {
    // Omitting a field here would reset it to the server default instead of keeping what the
    // operator is looking at.
    const b = body(urlBodyFrom(urlDraft({ verifyTls: false })));
    expect(Object.keys(b).sort()).toEqual(
      [
        'body_match',
        'body_max_bytes',
        'credential',
        'expected_status',
        'follow_redirects',
        'json_extract',
        'method',
        'timeout_ms',
        'url',
        'verify_tls',
      ].sort(),
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

describe('urlBodyFrom credential binding (regression)', () => {
  it('sends the credential explicitly, because the PUT is a replace', () => {
    // The body omitted `credential` while its own doc comment said every field is sent explicitly
    // *because* this is a replace. Editing any other field therefore cleared the binding — silent
    // while nothing consumed it, and an instant logout for the monitor once something did.
    const body = urlBodyFrom(urlDraft({ credentialId: 'cred-1', timeoutMs: '9000' }));
    expect('body' in body && body.body.credential).toBe('cred-1');
  });

  it('clears the binding with null rather than by omission', () => {
    const body = urlBodyFrom(urlDraft({ credentialId: '' }));
    expect('body' in body && body.body.credential).toBeNull();
  });

  it('round-trips a bound credential through draft and body', () => {
    const draft = urlDraftFrom({
      url: 'https://example.test/health',
      method: 'GET',
      expected_status: { kind: 'two_xx' },
      verify_tls: true,
      follow_redirects: true,
      timeout_ms: 5000,
      credential: 'cred-9',
    });
    expect(draft.credentialId).toBe('cred-9');
    const body = urlBodyFrom(draft);
    expect('body' in body && body.body.credential).toBe('cred-9');
  });
});

describe('urlBodyFrom body keyword rule (ADR-047 Inc.2)', () => {
  it('sends null when the rule is off, so a stored rule can be removed', () => {
    // The PUT is a replace. If "off" omitted the field instead, turning a content check *off*
    // would be impossible through the only endpoint that edits it — it would keep polling with a
    // rule the operator can no longer see in the form.
    expect(body(urlBodyFrom(urlDraft())).body_match).toBeNull();
    // And a filled-in pattern still sends null while the toggle is off: what the boxes hold is not
    // what is configured.
    expect(body(urlBodyFrom(urlDraft({ bodyPattern: 'ok' }))).body_match).toBeNull();
  });

  it('builds the rule from its fields, trimming the keyword', () => {
    const b = body(
      urlBodyFrom(
        urlDraft({
          bodyMatchEnabled: true,
          bodyPattern: '  "status":"ok"  ',
          bodyMode: 'not_contains',
          bodyMaxBytes: '4096',
        }),
      ),
    );
    expect(b.body_match).toEqual({ pattern: '"status":"ok"', mode: 'not_contains' });
    // The budget belongs to the monitor, not to the rule — one body, one read, one budget.
    expect(b.body_max_bytes).toBe(4096);
  });

  it('refuses an empty keyword', () => {
    // Every string contains "", so `contains` would always pass and `not_contains` always fail —
    // a monitor that has stopped reporting on anything real while still looking configured.
    for (const bodyPattern of ['', '   ']) {
      expect(urlBodyFrom(urlDraft({ bodyMatchEnabled: true, bodyPattern }))).toEqual({
        error: 'bodyPatternRequired',
      });
    }
  });

  it('refuses a read budget outside the accepted range', () => {
    for (const bodyMaxBytes of ['0', '512', '2097152', '', 'abc']) {
      expect(urlBodyFrom(urlDraft({ bodyMaxBytes }))).toEqual({ error: 'bodyMaxBytes' });
    }
    // The boundaries themselves are accepted.
    for (const bodyMaxBytes of ['1024', '1048576']) {
      expect(body(urlBodyFrom(urlDraft({ bodyMaxBytes }))).body_max_bytes).toBe(
        Number(bodyMaxBytes),
      );
    }
  });

  it('refuses a body rule on a HEAD request', () => {
    // A HEAD response has no body, so the rule could never be satisfied and the monitor would
    // alert forever — indistinguishable from a real outage.
    expect(
      urlBodyFrom(urlDraft({ method: 'HEAD', bodyMatchEnabled: true, bodyPattern: 'ok' })),
    ).toEqual({ error: 'bodyMatchNeedsBody' });
    // Extraction on HEAD is refused for the same reason.
    expect(
      urlBodyFrom(
        urlDraft({ method: 'HEAD', extracts: [{ metric: 'q', path: 'queue.depth' }] }),
      ),
    ).toEqual({ error: 'bodyMatchNeedsBody' });
    // HEAD is still fine with neither — this must not have broken plain liveness monitors.
    expect(body(urlBodyFrom(urlDraft({ method: 'HEAD' }))).body_match).toBeNull();
  });
});

describe('urlBodyFrom JSON extraction (ADR-047 Inc.3)', () => {
  const withRows = (...rows: { metric: string; path: string }[]) =>
    urlBodyFrom(urlDraft({ extracts: rows }));

  it('sends the rules it was given, trimmed', () => {
    const b = body(withRows({ metric: '  queue_depth ', path: '  data.queue.depth ' }));
    expect(b.json_extract).toEqual([{ metric: 'queue_depth', path: 'data.queue.depth' }]);
  });

  it('drops a row the operator left completely blank', () => {
    // What "add rule" leaves behind when someone changes their mind. Rejecting it would make the
    // dialog unsavable for a reason the operator cannot see.
    const b = body(withRows({ metric: '', path: '' }, { metric: 'q', path: 'queue.depth' }));
    expect(b.json_extract).toEqual([{ metric: 'q', path: 'queue.depth' }]);
    expect(body(urlBodyFrom(urlDraft())).json_extract).toEqual([]);
  });

  it('refuses a half-filled row rather than silently dropping it', () => {
    // The dangerous sibling of the case above: dropping this would discard a rule the operator
    // believes they configured, and nothing would ever be recorded for it.
    expect(withRows({ metric: 'q', path: '' })).toEqual({ error: 'extractPathRequired' });
    expect(withRows({ metric: '', path: 'queue.depth' })).toEqual({
      error: 'extractMetricRequired',
    });
  });

  it('refuses a metric name the TSDB could not be queried for', () => {
    for (const metric of ['1queue', 'has space', 'has-dash', 'dot.ted', 'q!']) {
      expect(withRows({ metric, path: 'a.b' })).toEqual({ error: 'extractMetricName' });
    }
    expect(body(withRows({ metric: 'ns:queue_1', path: 'a.b' })).json_extract).toHaveLength(1);
  });

  it('refuses a name the monitor already reports', () => {
    // The specific hazard: this would overwrite the node's own availability series with a number
    // out of the monitored service's JSON.
    for (const metric of ['http_up', 'http_status_code', 'http_body_match']) {
      expect(withRows({ metric, path: 'a.b' })).toEqual({ error: 'extractMetricReserved' });
    }
  });

  it('refuses two rules writing the same series', () => {
    // Otherwise the recorded value depends on rule order, which is not something an operator
    // should have to reason about.
    expect(withRows({ metric: 'q', path: 'a' }, { metric: 'q', path: 'b' })).toEqual({
      error: 'extractMetricDuplicate',
    });
  });

  it('caps how many rules one monitor may carry', () => {
    const many = Array.from({ length: 9 }, (_, i) => ({ metric: `m${i}`, path: 'a.b' }));
    expect(urlBodyFrom(urlDraft({ extracts: many }))).toEqual({ error: 'tooManyExtracts' });
    expect(body(urlBodyFrom(urlDraft({ extracts: many.slice(0, 8) }))).json_extract).toHaveLength(8);
  });
});
