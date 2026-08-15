// SPDX-License-Identifier: AGPL-3.0-only
// Single source of truth for the node-detail sub-tabs: the whitelist, the label key, which node
// kinds see each tab, and the count-pill / warning-dot rules.
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
// Adding a tab: extend NODE_DETAIL_TABS, add its NODE_DETAIL_TAB_META entry (including `kinds`) and
// its body element in NodeDetail.tsx (all enforced by the compiler), and add `tabs.<key>` to the
// `nodes` EN+JA locales.

import { NODE_KINDS, type InterfaceRow, type NodeKind, type NodeState } from '../../types/api';

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

/** Every kind — the tabs that mean something whatever the node is. */
const ALL_KINDS: readonly NodeKind[] = NODE_KINDS;

/** Only an ordinary device. `scheduler.rs::assemble_node_jobs` returns *before* the ICMP and SNMP
 *  branches for a URL or DNS monitor (one HTTP/DNS job and nothing else), and a Meraki node emits
 *  no per-node job at all — it is polled by the org collector. So there is never an ifTable walk,
 *  never a CDP/LLDP walk, and never a NetFlow export to attribute. These tabs are not "empty for
 *  now"; they are structurally unreachable, which is why hiding them loses nothing. */
const DEVICE_ONLY: readonly NodeKind[] = ['device'];

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
  /** Which node kinds see this tab. **Required on purpose** — a new tab cannot compile without
   *  someone deciding who it is for, which is the same "the compiler demands an entry" rule the
   *  rest of this file rests on. `visibleNodeDetailTabs` is *derived* from these lists rather than
   *  being a second hand-written map, because a hardcoded list that also feeds a visibility guard
   *  makes the guard the dangerous copy (ui-conventions.md, the `metricCards.ts` lesson). */
  kinds: readonly NodeKind[];
  /** Count pill after the label. Return null for "no pill" (unknown or zero). */
  badge?: (s: NodeDetailTabStats) => number | null;
  /** Warning dot after the label — the tab needs attention. */
  warn?: (s: NodeDetailTabStats) => boolean;
}

export const NODE_DETAIL_TAB_META: Record<NodeDetailTab, NodeDetailTabMeta> = {
  overview: { labelKey: 'tabs.overview', kinds: ALL_KINDS },
  interfaces: {
    labelKey: 'tabs.interfaces',
    kinds: DEVICE_ONLY,
    badge: (s) => s.interfaces.length || null,
    warn: (s) => s.interfaces.some((r) => r.oper_status != null && r.oper_status !== 1),
  },
  // No badge: the count would need a second fetch on every tab-bar render, and adjacency is not
  // something a number in a pill answers ("2 neighbours" tells an operator nothing they wanted).
  neighbors: { labelKey: 'tabs.neighbors', kinds: DEVICE_ONLY },
  // Every kind: since ADR-046 this tab is the node's *metric inventory*, not its SNMP collection
  // set, so it is the only place a URL monitor's http_* / extracted values or a DNS monitor's
  // dns_up / dns_resolve_ms can be charted at all.
  collection: {
    labelKey: 'tabs.collection',
    kinds: ALL_KINDS,
    // `|| null` per the doc above: a URL/DNS monitor's built-in profile attaches no SNMP templates,
    // and a bare `Collection 0` pill reads as a fault rather than as "not applicable".
    // ⚠️ Follow-up, deliberately not fixed here: this counts the profile's templates while the tab
    // body lists the ADR-046 inventory, so the pill and the screen answer different questions.
    // Reconciling them means pulling `fetchNodeMetrics` onto the tab-bar render path.
    badge: (s) => s.collCount || null,
    // An SNMP-bound node we cannot currently reach: its collection is configured but not landing.
    warn: (s) => s.hasCredential && (s.state === 'unreachable' || s.state === 'unknown'),
  },
  // Meraki keeps this — Meraki appliances do export syslog, and an operator can bind a webhook
  // source straight to a node. URL and DNS monitors do not: a DNS monitor's address is 0.0.0.0
  // when it uses the system resolver, so nothing can correlate to it.
  // ⚠️ Known hazard worth a thread for whoever traces "my syslog disappeared": `repo.rs`'s
  // address_map is built with `or_insert` — first row wins — and a URL monitor's `address` is its
  // *target's* resolved IP. A monitor can therefore shadow a real inventory device and be
  // attributed its syslog/traps. Hiding Events here hides the symptom of that misattribution too;
  // the events themselves stay reachable at /events?node_id=<id>.
  events: { labelKey: 'tabs.events', kinds: ['device', 'meraki'] },
  flow: { labelKey: 'tabs.flow', kinds: DEVICE_ONLY },
};

/** The tabs this kind shows, in `NODE_DETAIL_TABS` order.
 *
 *  Derived, never declared: ordering comes from the array and membership from each entry's
 *  `kinds`, so there is no second list to forget. A `Record<NodeKind, NodeDetailTab[]>` would be
 *  keyed by the wrong set — adding one tab would mean editing four rows, and missing one of them
 *  is precisely the ADR-031 bug this file exists to prevent. */
export function visibleNodeDetailTabs(kind: NodeKind): readonly NodeDetailTab[] {
  return NODE_DETAIL_TABS.filter((tab) => NODE_DETAIL_TAB_META[tab].kinds.includes(kind));
}

/** Normalize, then reject a tab this kind does not show.
 *
 *  `kind === null` means "not known yet" and behaves exactly like [`normalizeNodeDetailTab`] —
 *  the two URL hosts never load the node, and `NodeDetail` renders a loading pane until it has.
 *  Without the kind check a bookmarked `?tab=flow` on a DNS node would index into the (exhaustive)
 *  body map and paint a tab whose button is not in the bar: the mirror image of the bug in the
 *  header comment. Falling back to `overview` is safe for every kind — `tabs.test.ts` pins that. */
export function resolveNodeDetailTab(tab: string, kind: NodeKind | null): NodeDetailTab {
  const normalized = normalizeNodeDetailTab(tab);
  if (kind === null) return normalized;
  return visibleNodeDetailTabs(kind).includes(normalized) ? normalized : 'overview';
}
