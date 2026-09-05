// SPDX-License-Identifier: AGPL-3.0-only
// Single source of truth for the node-detail sub-tabs: the whitelist, the label key, which nodes
// see each tab, and the count-pill / warning-dot rules.
//
// Visibility has TWO axes, and the second one is not the node's kind: a tab is offered when the
// node's kind is in its `kinds` list AND — if the tab is fed only by an SNMP walk — the node is
// actually polled over SNMP (ADR-119). A ping-only device is a `device` like any other, so the
// kind axis alone cannot tell it apart, and Interfaces and Neighbors are structurally empty on it
// for exactly the reason they are on a URL monitor.
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
// Adding a tab: extend NODE_DETAIL_TABS, add its NODE_DETAIL_TAB_META entry (including `kinds`
// and `needsSnmp`, both required so the two visibility questions get answered) and
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

/** Only an ordinary device. `scheduler/assemble.rs::assemble_node_jobs` returns *before* the ICMP and SNMP
 *  branches for a URL or DNS monitor (one HTTP/DNS job and nothing else), and a Meraki node emits
 *  no per-node job at all — it is polled by the org collector. So there is never an ifTable walk,
 *  never a CDP/LLDP walk, and never a NetFlow export to attribute. These tabs are not "empty for
 *  now"; they are structurally unreachable, which is why hiding them loses nothing.
 *
 *  ⚠️ For the two SNMP-fed tabs this list is necessary and **not sufficient**: a ping-only node is
 *  a `device` and passes it. That half is `needsSnmp`. */
const DEVICE_ONLY: readonly NodeKind[] = ['device'];

/**
 * What the tab rules ask about a node. Both facts come off the `NodeDetail` the pane has already
 * loaded, so deciding which tabs to draw costs no extra fetch.
 *
 * 🚨 `snmpConfigured` is the **server's** answer (`NodeDetail.snmp_configured`), never
 * `!!node.credential_id`. The scheduler falls back to the deployment-wide `YAGRA_SNMP_COMMUNITY`
 * for nodes with no bound credential, so on such a deployment a null `credential_id` still means a
 * device that is walked and has interface rows — and hiding its tabs would hide real data with
 * nothing on screen to say why (ADR-119 決定 2).
 */
export interface NodeDetailSubject {
  kind: NodeKind;
  snmpConfigured: boolean;
}

/** Live node facts the tab bar decorates itself from. Kept free of React types so the badge/warn
 *  rules stay pure and unit-testable next to the whitelist they belong to. */
export interface NodeDetailTabStats {
  interfaces: InterfaceRow[];
  /** Templates attached to the node's profile; null while still loading. */
  collCount: number | null;
  /** The same fact as [`NodeDetailSubject.snmpConfigured`], read from the same field — so the
   *  warning dot and the visible tab set cannot answer "is this node polled over SNMP" differently. */
  hasSnmp: boolean;
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
  /** True when every row this tab can show comes from an SNMP walk, so a node with no SNMP
   *  configured would be offered the tab only to find it structurally empty.
   *
   *  **Required for the same reason `kinds` is** — a new tab cannot compile until someone decides,
   *  and the visible set stays *derived* from these two rather than becoming a third hand-written
   *  map.
   *
   *  ⚠️ Answer it about the tab's **data source**, not about which nodes usually have rows. Events
   *  and Flow are `false`: syslog, traps and NetFlow are attributed by the device's address, so a
   *  ping-only node can legitimately have both (ADR-119 決定 1). */
  needsSnmp: boolean;
  /** Count pill after the label. Return null for "no pill" (unknown or zero). */
  badge?: (s: NodeDetailTabStats) => number | null;
  /** Warning dot after the label — the tab needs attention. */
  warn?: (s: NodeDetailTabStats) => boolean;
}

export const NODE_DETAIL_TAB_META: Record<NodeDetailTab, NodeDetailTabMeta> = {
  overview: { labelKey: 'tabs.overview', kinds: ALL_KINDS, needsSnmp: false },
  // The ifTable walk is the only writer of `interfaces`, so with no SNMP there is nothing to list —
  // and nothing to explain either, which is why the tab goes rather than growing an empty state.
  interfaces: {
    labelKey: 'tabs.interfaces',
    kinds: DEVICE_ONLY,
    needsSnmp: true,
    badge: (s) => s.interfaces.length || null,
    warn: (s) => s.interfaces.some((r) => r.oper_status != null && r.oper_status !== 1),
  },
  // No badge: the count would need a second fetch on every tab-bar render, and adjacency is not
  // something a number in a pill answers ("2 neighbours" tells an operator nothing they wanted).
  neighbors: { labelKey: 'tabs.neighbors', kinds: DEVICE_ONLY, needsSnmp: true },
  // Every kind: since ADR-046 this tab is the node's *metric inventory*, not its SNMP collection
  // set, so it is the only place a URL monitor's http_* / extracted values or a DNS monitor's
  // dns_up / dns_resolve_ms can be charted at all.
  // Never hidden by the SNMP axis, and that is load-bearing rather than incidental: since ADR-046
  // this tab is the node's metric *inventory*, so a ping-only node's `icmp_rtt_ms` lives here — and
  // it is where the screen says `ICMP-only node`, which is the answer to "where did Interfaces go"
  // (ADR-055 R6, ADR-119 決定 4). Hide it and the removal becomes unexplained.
  collection: {
    labelKey: 'tabs.collection',
    kinds: ALL_KINDS,
    needsSnmp: false,
    // `|| null` per the doc above: a URL/DNS monitor's built-in profile attaches no SNMP templates,
    // and a bare `Collection 0` pill reads as a fault rather than as "not applicable".
    // ⚠️ Follow-up, deliberately not fixed here: this counts the profile's templates while the tab
    // body lists the ADR-046 inventory, so the pill and the screen answer different questions.
    // Reconciling them means pulling `fetchNodeMetrics` onto the tab-bar render path.
    badge: (s) => s.collCount || null,
    // An SNMP-bound node we cannot currently reach: its collection is configured but not landing.
    warn: (s) => s.hasSnmp && (s.state === 'unreachable' || s.state === 'unknown'),
  },
  // Meraki keeps this — Meraki appliances do export syslog, and an operator can bind a webhook
  // source straight to a node. URL and DNS monitors do not: a DNS monitor's address is 0.0.0.0
  // when it uses the system resolver, so nothing can correlate to it.
  // ⚠️ Known hazard worth a thread for whoever traces "my syslog disappeared": `repo.rs`'s
  // address_map is built with `or_insert` — first row wins — and a URL monitor's `address` is its
  // *target's* resolved IP. A monitor can therefore shadow a real inventory device and be
  // attributed its syslog/traps. Hiding Events here hides the symptom of that misattribution too;
  // the events themselves stay reachable at /events?node_id=<id>.
  // ⚠️ `needsSnmp: false` on both, deliberately. Neither is fed by a walk: syslog and traps are
  // attributed by the device's source address and NetFlow by the exporter's, so a ping-only node
  // can have rows in either. Hiding them with the SNMP axis was asked for and declined on that
  // evidence (ADR-119 決定 1).
  events: { labelKey: 'tabs.events', kinds: ['device', 'meraki'], needsSnmp: false },
  flow: { labelKey: 'tabs.flow', kinds: DEVICE_ONLY, needsSnmp: false },
};

/** The tabs this node shows, in `NODE_DETAIL_TABS` order.
 *
 *  Derived, never declared: ordering comes from the array and membership from each entry's
 *  `kinds` and `needsSnmp`, so there is no second list to forget. A `Record<NodeKind,
 *  NodeDetailTab[]>` would be keyed by the wrong set — adding one tab would mean editing four
 *  rows, and missing one of them is precisely the ADR-031 bug this file exists to prevent. The
 *  SNMP axis makes that worse rather than better: keyed by kind × SNMP it would be eight rows.
 *
 *  The two axes are ANDed and neither subsumes the other — a URL monitor fails `kinds` for
 *  Interfaces, a ping-only device passes `kinds` and fails `needsSnmp`. */
export function visibleNodeDetailTabs(node: NodeDetailSubject): readonly NodeDetailTab[] {
  return NODE_DETAIL_TABS.filter((tab) => {
    const meta = NODE_DETAIL_TAB_META[tab];
    return meta.kinds.includes(node.kind) && (node.snmpConfigured || !meta.needsSnmp);
  });
}

/** Normalize, then reject a tab this node does not show.
 *
 *  `node === null` means "not known yet" and behaves exactly like [`normalizeNodeDetailTab`] —
 *  the two URL hosts never load the node, and `NodeDetail` renders a loading pane until it has.
 *  Without the check a bookmarked `?tab=flow` on a DNS node, or `?tab=interfaces` on a ping-only
 *  one, would index into the (exhaustive) body map and paint a tab whose button is not in the bar:
 *  the mirror image of the bug in the header comment. Falling back to `overview` is safe for every
 *  node — `tabs.test.ts` pins that. */
export function resolveNodeDetailTab(
  tab: string,
  node: NodeDetailSubject | null,
): NodeDetailTab {
  const normalized = normalizeNodeDetailTab(tab);
  if (node === null) return normalized;
  return visibleNodeDetailTabs(node).includes(normalized) ? normalized : 'overview';
}
