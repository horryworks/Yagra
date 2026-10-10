// SPDX-License-Identifier: AGPL-3.0-only
// Small comparators shared by stores that hold server records (ADR-202).

/** Newest first, by the server's creation time. Report runs and Troubleshoot jobs list this way. */
export function byNewestCreated(a: { created_ms: number }, b: { created_ms: number }): number {
  return b.created_ms - a.created_ms;
}
