// SPDX-License-Identifier: AGPL-3.0-only
import { describe, expect, it } from 'vitest';
import {
  buildHttpAuthSecret,
  emptyHttpAuth,
  httpAuthReady,
  isValidHeaderName,
  type HttpAuthState,
} from './httpAuthCredential';

const state = (over: Partial<HttpAuthState> = {}): HttpAuthState => ({
  ...emptyHttpAuth(),
  ...over,
});

describe('httpAuthReady', () => {
  it('requires both halves of a basic credential', () => {
    expect(httpAuthReady(state({ scheme: 'basic', username: 'probe', password: 'pw' }))).toBe(true);
    expect(httpAuthReady(state({ scheme: 'basic', username: 'probe' }))).toBe(false);
    expect(httpAuthReady(state({ scheme: 'basic', password: 'pw' }))).toBe(false);
    // A whitespace username is not a username.
    expect(httpAuthReady(state({ scheme: 'basic', username: '  ', password: 'pw' }))).toBe(false);
  });

  it('requires a token for bearer', () => {
    expect(httpAuthReady(state({ scheme: 'bearer', token: 'tok' }))).toBe(true);
    expect(httpAuthReady(state({ scheme: 'bearer', token: '   ' }))).toBe(false);
  });

  it('requires a usable header name and a value', () => {
    expect(httpAuthReady(state({ scheme: 'header', headerName: 'X-API-Key', headerValue: 'v' })))
      .toBe(true);
    expect(httpAuthReady(state({ scheme: 'header', headerName: 'X-API-Key' }))).toBe(false);
    expect(httpAuthReady(state({ scheme: 'header', headerName: 'Host', headerValue: 'v' }))).toBe(
      false,
    );
  });

  it('does not carry readiness across a scheme switch', () => {
    // Fields are kept so switching back does not lose typing, but a filled bearer token must not
    // make a basic credential look complete.
    const s = state({ scheme: 'basic', token: 'tok' });
    expect(httpAuthReady(s)).toBe(false);
  });
});

describe('isValidHeaderName', () => {
  it('accepts an RFC 7230 token', () => {
    expect(isValidHeaderName('X-API-Key')).toBe(true);
    expect(isValidHeaderName('X_Api_Key1')).toBe(true);
  });

  it('rejects the headers the probe owns, case-insensitively', () => {
    for (const n of ['Host', 'authorization', 'Connection', 'Transfer-Encoding', 'TE']) {
      expect(isValidHeaderName(n)).toBe(false);
    }
  });

  it('rejects separators, spaces and an empty or oversized name', () => {
    expect(isValidHeaderName('')).toBe(false);
    expect(isValidHeaderName('X Api Key')).toBe(false);
    expect(isValidHeaderName('X:Api')).toBe(false);
    expect(isValidHeaderName('X'.repeat(65))).toBe(false);
  });
});

describe('buildHttpAuthSecret', () => {
  it('emits the tagged document the backend parses', () => {
    expect(
      JSON.parse(buildHttpAuthSecret(state({ scheme: 'basic', username: ' probe ', password: 'pw' }))),
    ).toEqual({ scheme: 'basic', username: 'probe', password: 'pw' });

    expect(JSON.parse(buildHttpAuthSecret(state({ scheme: 'bearer', token: ' tok ' })))).toEqual({
      scheme: 'bearer',
      token: 'tok',
    });

    expect(
      JSON.parse(
        buildHttpAuthSecret(state({ scheme: 'header', headerName: ' X-API-Key ', headerValue: 'v' })),
      ),
    ).toEqual({ scheme: 'header', name: 'X-API-Key', value: 'v' });
  });

  it('never carries a field from another scheme into the document', () => {
    // The state holds every field so switching does not lose typing; the document must not.
    const doc = JSON.parse(
      buildHttpAuthSecret(state({ scheme: 'bearer', token: 'tok', password: 'leak-me' })),
    );
    expect(Object.keys(doc).sort()).toEqual(['scheme', 'token']);
  });

  it('does not trim a password or header value, which may legitimately have edge whitespace', () => {
    const doc = JSON.parse(
      buildHttpAuthSecret(state({ scheme: 'basic', username: 'u', password: ' pw ' })),
    );
    expect(doc.password).toBe(' pw ');
  });
});
