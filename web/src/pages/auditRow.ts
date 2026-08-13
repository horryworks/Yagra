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
 *  being forced into a method/path shape it does not have.
 *
 *  ⚠️ Sign-in is matched by **prefix**, not equality. `api/session.rs` writes `auth.login` for a
 *  local sign-in and `auth.login.ldap`, `auth.login.ldap_unavailable` and `auth.login.ldap_conflict`
 *  for the directory paths — and says so: *"`auth.login` prefix, so an existing `LIKE 'auth.login%'`
 *  query still finds everything."* The equality this replaces meant every LDAP and OIDC sign-in
 *  rendered as a raw `auth.login.ldap` chip instead of the localized label, and the "Sign in" filter
 *  never matched one. The backend's `AuditAction::Login` uses the same prefix, so the chip and the
 *  filter now agree by construction. */
export function parseAction(action: string): ParsedAction {
  if (action.startsWith('auth.login')) return { method: 'SIGN IN', path: null, login: true };
  const sp = action.indexOf(' ');
  if (sp < 0) return { method: action, path: null, login: false };
  return { method: action.slice(0, sp), path: action.slice(sp + 1), login: false };
}

// `csvField` was re-exported here for `AuditPage`, and is not any more: the audit export moved to
// the server (`GET /api/v1/audit/export.csv`), so this page encodes no CSV at all. The encoder
// still lives in `lib/csv` for the Troubleshoot report export, and the history behind it is worth
// keeping: there were once two byte-identical copies and neither neutralized a leading `=`, so a
// username submitted to a *failed* login could be planted as `=HYPERLINK(…)` and evaluated when an
// admin opened the export. The Rust side had a third copy with the same hole
// (`crates/yagra-core/src/csv.rs` is now the one, mirrored against `lib/csv.ts` by a test).
// Do not re-inline an encoder here — that is the shape the bug had, three times.
