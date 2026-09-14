// SPDX-License-Identifier: AGPL-3.0-only
// A folder inside a folder, with one node in the inner one — for the specs that need an ancestor.
//
// The bootstrap mock flattens every folder to the root (`bootstrap.ts`), so a spec that needs a
// two-level path has to bring its own. More than one does (`nodeDetailLinks`, `treeFilterCollapse`),
// and this is the one copy of the ids, the names and the two mock answers they share.

import { defaultBodyFor, MOCK_PREFIX, type Json } from './openapi';

export const PARENT_ID = '00000000-0000-4000-8000-0000000000c1';
export const CHILD_ID = '00000000-0000-4000-8000-0000000000c2';
export const NODE_ID = '00000000-0000-4000-8000-0000000000c3';
export const PARENT_NAME = `${MOCK_PREFIX}region`;
export const CHILD_NAME = `${MOCK_PREFIX}site`;
export const NODE_NAME = `${MOCK_PREFIX}member`;

/** Parent → child, built from the generated group row so a change to its shape reaches this too. */
export function twoLevelGroups(): Json {
  const [template] = defaultBodyFor('/api/v1/node-groups') as Record<string, Json>[];
  return [
    { ...template, id: PARENT_ID, name: PARENT_NAME, parent_id: null, group_type: 'generic' },
    { ...template, id: CHILD_ID, name: CHILD_NAME, parent_id: PARENT_ID, group_type: 'generic' },
  ] as unknown as Json;
}

/** The server rollup for the same tree: one healthy node directly in the child folder.
 *
 *  🚨 **A spec that browses needs this; one that only selects a folder does not.** The tree asks for
 *  a folder's members only when the rollup says it has some (ADR-125) — the generated body names
 *  no real folder, so both folders read 0, their arrows are disabled, and nothing is fetched.
 *  `nodeDetailLinks` never noticed, because selecting a folder fetches it regardless. */
export function groupSummary(): Json {
  const empty = { critical: 0, maintenance: 0, ok: 0, unknown: 0, unreachable: 0, warning: 0 };
  return { groups: { [PARENT_ID]: empty, [CHILD_ID]: { ...empty, ok: 1 } } } as unknown as Json;
}

/** One node, filed in the child folder. Mirrors the two forms `bootstrap.ts` answers: the batch
 *  form echoes the folders it was asked about in `answered` (without it the tree re-queues the
 *  folder forever, ADR-125), and the single-group form carries no echo. */
export function membersByGroup(url: URL): Json {
  const body = defaultBodyFor('/api/v1/nodes/by-group') as {
    nodes: Record<string, Json>[];
    answered?: string[];
  };
  const member = { ...body.nodes[0], id: NODE_ID, name: NODE_NAME, group_id: CHILD_ID, sort_order: 1 };
  const batch = url.searchParams.get('groups');
  if (batch) {
    const asked = batch.split(',').filter(Boolean);
    return {
      nodes: asked.includes(CHILD_ID) ? [member] : [],
      truncated: false,
      answered: asked,
    } as unknown as Json;
  }
  delete body.answered;
  const nodes = url.searchParams.get('group') === CHILD_ID ? [member] : [];
  return { ...body, nodes } as unknown as Json;
}
