// SPDX-License-Identifier: AGPL-3.0-only
// Single source of truth for the node-detail sub-tab whitelist.
//
// Consumed by all three surfaces that host the node detail so they can never drift:
//  - NodeDetail.tsx     — validates the controlled `tab` prop and renders the tab bar/body switch
//  - NodeDetailPage.tsx — the /nodes/:id full-page route (tab ↔ URL `?tab=`)
//  - NodesPage.tsx      — the /nodes split-view inline host (validates the pane's active tab)
//
// Adding a tab in ONE place keeps the three in lockstep. This used to be three separate literal
// arrays: when the Flow tab (ADR-031) was added, the split-view host's copy was missed, so the Flow
// button rendered but clicking it validated against a whitelist without 'flow' and bounced back to
// Overview. A shared constant makes that class of bug structurally impossible.

export const NODE_DETAIL_TABS = ['overview', 'interfaces', 'collection', 'events', 'flow'] as const;

/** A valid node-detail sub-tab key. */
export type NodeDetailTab = (typeof NODE_DETAIL_TABS)[number];

/** Normalize an arbitrary tab string (URL param, stored state) to a known tab; unknown ⇒ 'overview'. */
export function normalizeNodeDetailTab(tab: string): NodeDetailTab {
  return (NODE_DETAIL_TABS as readonly string[]).includes(tab)
    ? (tab as NodeDetailTab)
    : 'overview';
}
