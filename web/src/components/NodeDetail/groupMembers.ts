// SPDX-License-Identifier: AGPL-3.0-only
// What a folder's Members section can honestly say about its direct nodes (ADR-142).
//
// The subfolders come from the group skeleton and are always known. The direct nodes arrive from the
// lazy member cache (`pages/useLazyGroupMembers.ts`) and may not have yet — so "nothing in this
// folder" is a claim only a FINISHED fetch can make. Before it the list is still arriving, and after a
// failed one it is unknown. The pane used to say "No subfolders or nodes in this folder." in all
// three cases, under a header reading "240 nodes" from the server rollup.
//
// Kept out of `GroupDetail.tsx` so Vitest can reach it (`.claude/rules/testing.md`).

/** Where the selected folder's direct-member fetch stands. */
export type MemberFetch = 'loading' | 'failed' | 'loaded';

/** A failure is asked first: a failed key is never in the loaded set, and a folder nobody can load
 *  must not be drawn as one that is still arriving (ADR-125's `group-failed` row, same reasoning). */
export function memberFetchState(
  groupId: string,
  loaded: ReadonlySet<string>,
  failed: ReadonlySet<string>,
): MemberFetch {
  if (failed.has(groupId)) return 'failed';
  return loaded.has(groupId) ? 'loaded' : 'loading';
}

/** The line under the Members list, if any.
 *
 *  `empty` only once the fetch has finished with no row to show. Rows already on screen — the
 *  subfolders, which never wait on a fetch — stay drawn above a loading or failed line rather than
 *  being hidden behind it. */
export function membersTrailer(
  fetch: MemberFetch,
  rowCount: number,
): 'loading' | 'failed' | 'empty' | null {
  if (fetch === 'loading') return 'loading';
  if (fetch === 'failed') return 'failed';
  return rowCount === 0 ? 'empty' : null;
}
