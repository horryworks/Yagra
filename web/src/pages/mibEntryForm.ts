// SPDX-License-Identifier: AGPL-3.0-only
/** The MIB-repository add dialog's pure half: what makes a catalog entry submittable.
 *
 *  This is the *only* validation between an operator and the catalog every collection set picks
 *  from — a malformed OID stored here becomes a metric that silently never reports, on every node
 *  whose profile references it. So it lives in a `.ts`: Vitest here runs `environment: 'node'` with
 *  `include: ['src/ **\/*.test.ts']`, and inside `MibRepositoryPage.tsx` the rule was unreachable by
 *  any test. */

/** A dotted-decimal object identifier: one or more numeric arcs, no leading/trailing dot, no
 *  symbolic names. Deliberately stricter than "starts with a digit" — the backend stores the string
 *  verbatim and the poller feeds it straight to the SNMP client. */
export const OID_RE = /^[0-9]+(\.[0-9]+)*$/;

/** Whether a typed OID is well-formed. Surrounding whitespace is trimmed first, because that is
 *  what the submit path stores — accepting `" 1.3.6 "` here and sending `"1.3.6"` is the same
 *  entry, not a laxer rule. */
export function isValidOid(oid: string): boolean {
  return OID_RE.test(oid.trim());
}

/** The dialog's submit gate: a non-empty metric name **and** a well-formed OID. The other fields
 *  are selects with a default, and vendor is optional, so those two are the whole rule. */
export function mibEntryReady(metricName: string, oid: string): boolean {
  return metricName.trim().length > 0 && isValidOid(oid);
}
