// SPDX-License-Identifier: AGPL-3.0-only
// How an audit entry's stored `action` string is split for display, and how a cell is quoted for
// CSV export.
//
// Extracted from AuditPage.tsx so both are testable (Vitest never runs `.tsx`). The CSV quoting in
// particular is worth reaching: the audit log records attacker-influenceable text — a failed login
// stores the submitted username verbatim — and that text is exported to a file an operator opens
// in a spreadsheet.

/** An `action` split into what it was and what it acted on. */
export interface ParsedAction {
  method: string;
  path: string | null;
  /** The synthetic sign-in entry, which has no path and is labelled rather than shown verbatim. */
  login: boolean;
}

/** Split `"POST /api/v1/nodes"` into its method and path.
 *
 *  Anything without a space is a bare action name (`auth.logout`) and is shown as-is rather than
 *  being forced into a method/path shape it does not have. */
export function parseAction(action: string): ParsedAction {
  if (action === 'auth.login') return { method: 'SIGN IN', path: null, login: true };
  const sp = action.indexOf(' ');
  if (sp < 0) return { method: action, path: null, login: false };
  return { method: action.slice(0, sp), path: action.slice(sp + 1), login: false };
}

/** Quote a CSV field (RFC 4180): wrap in quotes, double any embedded quote.
 *
 *  Unconditional quoting rather than "quote when it contains a comma": the values here include
 *  usernames a caller chose, so deciding per value is a rule that can be got wrong once and
 *  corrupt every row after it. */
export function csvField(v: string | number): string {
  return `"${String(v).replace(/"/g, '""')}"`;
}
