// SPDX-License-Identifier: AGPL-3.0-only
// The one place a screen gets the in-app URL of an entity it links to (ADR-184 increment 8).
//
// Two different pages answer for a node, and a link picks one on purpose:
//
//  - `nodeHref(id)` → `/nodes/:id`, the **node detail page**: one node, full width, its tabs in the
//    URL. Use it when the operator wants to read or act on that node — a search result, a map box,
//    a row in a list of nodes elsewhere.
//  - `nodesPageHref(sel)` → `/nodes?sel=…`, the **All nodes page** (the inventory tree) opened with
//    a node or a folder selected in its right-hand pane. Use it when where the thing is filed
//    matters — a folder, or a node the operator is meant to find among its neighbours in the tree.
//
// The two are not interchangeable: `/nodes/:id` has no tree, and `/nodes?sel=` needs the tree's
// own selection encoding (`node:<id>` / `group:<id>`). `entityHref.test.ts` fails the build for a
// non-test source that spells `/nodes/<id>` by hand.
//
// Paths under `services/` that name `/nodes/{id}/…` are API endpoints, not links, and are typed
// from the OpenAPI document instead (ADR-035).

/** The node detail page (`/nodes/:id`). The id is a Yagra node uuid. */
export function nodeHref(id: string): string {
  return `/nodes/${id}`;
}

export { nodesPageHref, topologyMapHref } from './treeSelection';
export { merakiOrgPath } from '../pages/integrations/merakiOrgRow';
