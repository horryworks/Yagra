// SPDX-License-Identifier: AGPL-3.0-only
// Single source of truth for the node-detail sub-tabs: the whitelist, the label key, and the
// count-pill / warning-dot rules.
//
// Consumed by all three surfaces that host the node detail so they can never drift:
//  - NodeDetail.tsx     — renders the tab bar from NODE_DETAIL_TAB_META and the body from a
//                         `Record<NodeDetailTab, ReactNode>` (both keyed by the union below, so a
//                         tab added here without a body is a *compile* error, not a runtime bounce)
//  - NodeDetailPage.tsx — the /nodes/:id full-page route (tab ↔ URL `?tab=`)
//  - NodesPage.tsx      — the /nodes split-view inline host (validates the pane's active tab)
//
// This used to be three separate literal arrays: when the Flow tab (ADR-031) was added, the
// split-view host's copy was missed, so the Flow button rendered but clicking it validated against
// a whitelist without 'flow' and bounced back to Overview. Sharing the array fixed the three hosts,
// but the tab bar and the body switch inside NodeDetail.tsx stayed hand-written literals — the same
// bug was still reachable one level down. Keying every per-tab concern off `NodeDetailTab` makes it
// structurally impossible: TypeScript requires each Record to be exhaustive.
//
// Adding a tab: extend NODE_DETAIL_TABS, add its NODE_DETAIL_TAB_META entry and its body element in
// NodeDetail.tsx (both enforced by the compiler), and add `tabs.<key>` to the `nodes` EN+JA locales.

import type { InterfaceRow, NodeState } from '../../types/api';

export const NODE_DETAIL_TABS = [
  'overview',
  'interfaces',
  'neighbors',
  'collection',
  'events',
  'flow',
] as const;

/** A valid node-detail sub-tab key. */
export type NodeDetailTab = (typeof NODE_DETAIL_TABS)[number];

/** Normalize an arbitrary tab string (URL param, stored state) to a known tab; unknown ⇒ 'overview'. */
export function normalizeNodeDetailTab(tab: string): NodeDetailTab {
  return (NODE_DETAIL_TABS as readonly string[]).includes(tab)
    ? (tab as NodeDetailTab)
    : 'overview';
}

/** Live node facts the tab bar decorates itself from. Kept free of React types so the badge/warn
 *  rules stay pure and unit-testable next to the whitelist they belong to. */
export interface NodeDetailTabStats {
  interfaces: InterfaceRow[];
  /** Templates attached to the node's profile; null while still loading. */
  collCount: number | null;
  hasCredential: boolean;
  state: NodeState;
}

/** Per-tab presentation rules. `labelKey` resolves in the `nodes` i18n namespace. */
export interface NodeDetailTabMeta {
  labelKey: string;
  /** Count pill after the label. Return null for "no pill" (unknown or zero). */
  badge?: (s: NodeDetailTabStats) => number | null;
  /** Warning dot after the label — the tab needs attention. */
  warn?: (s: NodeDetailTabStats) => boolean;
}

export const NODE_DETAIL_TAB_META: Record<NodeDetailTab, NodeDetailTabMeta> = {
  overview: { labelKey: 'tabs.overview' },
  interfaces: {
    labelKey: 'tabs.interfaces',
    badge: (s) => s.interfaces.length || null,
    warn: (s) => s.interfaces.some((r) => r.oper_status != null && r.oper_status !== 1),
  },
  // No badge: the count would need a second fetch on every tab-bar render, and adjacency is not
  // something a number in a pill answers ("2 neighbours" tells an operator nothing they wanted).
  neighbors: { labelKey: 'tabs.neighbors' },
  collection: {
    labelKey: 'tabs.collection',
    badge: (s) => s.collCount,
    // An SNMP-bound node we cannot currently reach: its collection is configured but not landing.
    warn: (s) => s.hasCredential && (s.state === 'unreachable' || s.state === 'unknown'),
  },
  events: { labelKey: 'tabs.events' },
  flow: { labelKey: 'tabs.flow' },
};
