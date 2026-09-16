// SPDX-License-Identifier: AGPL-3.0-only
// A tree long enough to scroll: sixty nodes in the Ungrouped bucket.
//
// ⚠️ **The default mock is three nodes, and a three-row tree cannot scroll.** ADR-073's rule ("count
// the gestures against the real data, not against the layout") applies to a fixture too: against a
// tree that fits its pane, every "did it scroll / did it stay put" assertion reports the same thing a
// dead selector would. `treeScrollStill.spec.ts` (a press must not move the tree) and
// `treeKeyboard.spec.ts` (a key must bring its row into view) need the same sixty rows, and this is
// the one copy of them.

import { defaultBodyFor, MOCK_PREFIX, type Json } from './openapi';
import type { components } from '../../src/api/schema';

export const RUN_LENGTH = 60;

/** The name of the i-th node, zero-padded so the rows sort and read in order. */
export const runNodeName = (i: number): string => `${MOCK_PREFIX}node-${String(i).padStart(2, '0')}`;

/** The id of the i-th node. */
export const runNodeId = (i: number): string => `00000000-0000-4000-8000-a${String(i).padStart(11, '0')}`;

/** The generated member row, repeated into an ungrouped bucket long enough to virtualize. Built from
 *  the generated shape rather than hand-written, so a change to `NodeSummary` reaches here.
 *  ⚠️ `group_id: null` is what files them under Ungrouped — the generator fills every nullable uuid,
 *  and a node claiming a folder nobody opened is a node with no row. */
const RUN = (() => {
  const body = defaultBodyFor('/api/v1/nodes/by-group') as components['schemas']['GroupNodes'];
  const [template] = body.nodes;
  return {
    ...body,
    nodes: Array.from({ length: RUN_LENGTH }, (_, i) => ({
      ...template,
      id: runNodeId(i),
      name: runNodeName(i),
      group_id: null,
      sort_order: i + 1,
    })),
  };
})();

/** The `/api/v1/nodes/by-group` override.
 *
 *  ⚠️ One call per open folder plus one for the bucket, all landing on this key — so the answer has
 *  to depend on the query. Returning the sixty to a folder as well would put every id in the flat
 *  row list twice, which is a broken tree, not a taller one. */
export const ungroupedRunByGroup = (url: URL): Json =>
  (url.searchParams.get('group') ? { ...RUN, nodes: [] } : RUN) as unknown as Json;
