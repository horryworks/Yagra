// SPDX-License-Identifier: AGPL-3.0-only
// When the node page reads itself again after "Poll now" (ADR-149).
//
// The first read picks up what a poll answers within seconds — liveness and the scalar values. The
// second is for what the same press also reads and takes longer: the device's identity (OS version,
// serial number) rides the scalar job, and the names of its vendor-table rows are walked after the
// table job, for up to 20 s on the poller. Core writes a result as soon as it arrives, so the only
// wait is the poll itself.

/** Milliseconds after a successful "Poll now" at which the page re-reads, in order. */
export const POLL_NOW_REFRESH_MS = [4_000, 30_000] as const;
