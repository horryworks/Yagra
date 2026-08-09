// SPDX-License-Identifier: AGPL-3.0-only
// How an audit entry's stored `action` string is split for display, plus the CSV field encoder the
// export uses (re-exported from `lib/csv`, where it is shared with the Troubleshoot exports).
//
// Extracted from AuditPage.tsx so both are testable (Vitest never runs `.tsx`). The CSV encoding in
// particular is worth reaching: the audit log records attacker-influenceable text — a failed login
// stores the submitted username verbatim — and that text is exported to a file an operator opens
// in a spreadsheet, which will execute it given the chance.

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

/** Re-exported so `AuditPage` keeps one import for its row helpers.
 *
 *  The implementation moved to `lib/csv` once the Troubleshoot export turned out to hold a
 *  byte-identical copy — and neither copy neutralized a leading `=`, so a username submitted to a
 *  failed login could be planted as `=HYPERLINK(…)` and evaluated when an admin opened the export.
 *  Do not re-inline it here: that is the shape the bug had. */
export { csvField } from '../lib/csv';
