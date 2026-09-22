// SPDX-License-Identifier: AGPL-3.0-only
// What each `NodeKind` looks like and behaves like in the UI: the badge that distinguishes it in a
// list, the key that names it in prose, and the metric that says "this monitor is alive".
//
// One `Record<NodeKind, …>` rather than three parallel maps, so a new kind is one entry the
// compiler demands and not three places to forget (`extensibility.md` §1). This is the *display*
// side of `NodeKind`; `pages/monitorKinds.ts` is the deliberately smaller "what may an operator
// create by hand" registry (no `meraki` — nobody types one in), and the two are kept separate on
// purpose: sharing their strings would couple two lists that answer different questions and blur
// what `monitorKinds.test.ts`'s subset assertion is protecting.

import type { NodeKind } from '../types/api';
import type { BadgeBrand } from './brandBadge';

export interface NodeKindSpec {
  /** Short badge shown after a node's name. `null` = no badge: the ordinary device is the
   *  *unmarked default*, so a 50k-row inventory tree does not grow a badge on every line, and the
   *  badge means "this one is not a normal device". */
  readonly badge: string | null;
  /** Whose colours the badge wears: `null` for Yagra's own accent, or a third party's mark
   *  (`lib/brandBadge.ts`). Required, so a new kind decides rather than inherits. */
  readonly badgeBrand: BadgeBrand | null;
  /** `nodes`-namespace key naming the kind in prose (badge tooltip). Total over `NodeKind`. */
  readonly labelKey: string;
  /** The metric whose newest sample means "we heard from this monitor". Each kind is polled over a
   *  different protocol, so there is no shared one: an ICMP RTT series is empty for a URL monitor
   *  (it is never pinged) and for a Meraki device (the org collector polls it, not a per-node job).
   *  Used for the header's "seen …" clause; only `device`'s is also charted. */
  readonly livenessMetric: string;
}

export const NODE_KIND_SPEC: Record<NodeKind, NodeKindSpec> = {
  // An access point imported from its wireless controller (ADR-064): never polled itself, so its
  // liveness is what the controller serving it reports.
  wireless_ap: {
    badge: 'AP',
    badgeBrand: null,
    labelKey: 'kind.wireless_ap',
    livenessMetric: 'wlan_ap_up',
  },
  meraki: {
    badge: 'Meraki',
    badgeBrand: 'meraki',
    labelKey: 'kind.meraki',
    livenessMetric: 'meraki_device_up',
  },
  url: { badge: 'URL', badgeBrand: null, labelKey: 'kind.url', livenessMetric: 'http_up' },
  dns: { badge: 'DNS', badgeBrand: null, labelKey: 'kind.dns', livenessMetric: 'dns_up' },
  device: { badge: null, badgeBrand: null, labelKey: 'kind.device', livenessMetric: 'icmp_rtt_ms' },
};

// The badge strings are literals, not i18n keys, on purpose: `URL`, `DNS` and `Meraki` are the same
// text in English and Japanese (the hardcoded `Meraki` badge this generalizes already was), and
// routing three unchanging ASCII tokens through `t()` would add six locale entries that can only
// ever drift. The tooltip beside them *is* translated, which is what carries the meaning.

/** The product type the Meraki Dashboard gives an access point (an MR), as the API carries it on
 *  `NodeSummary.meraki_product_type` and `NodeDetail.meraki_device.product_type`. */
export const MERAKI_ACCESS_POINT_PRODUCT = 'wireless';

/** Whether a Meraki product type is an access point — read case-blind, as core reads it. */
export function isMerakiAccessPoint(productType: string | null | undefined): boolean {
  return productType?.trim().toLowerCase() === MERAKI_ACCESS_POINT_PRODUCT;
}

/** One badge after a node's name. */
export interface NodeBadge {
  readonly text: string;
  /** Whose colours it wears (`lib/brandBadge.ts`); `null` for Yagra's own accent. */
  readonly brand: BadgeBrand | null;
  /** `nodes`-namespace key for the tooltip that says what the badge means. */
  readonly labelKey: string;
}

/**
 * Every badge a node wears after its name, in order — the one answer the tree, the folder view, the
 * node header and the move dialog all draw from, so none of them can forget one.
 *
 * Its kind's badge first (none for an ordinary device). Then, for a Meraki access point, "AP"
 * beside "Meraki" (ADR-168 決定 11, the user's decision): an MR stays `kind: meraki` — its
 * liveness and its screens are the Meraki ones — so the kind alone cannot say it is an access
 * point. The "AP" wears Yagra's accent, like the controller-walked AP's badge, because it names
 * what the device is rather than whose it is.
 */
export function nodeBadges(node: {
  kind: NodeKind;
  merakiProductType?: string | null;
}): NodeBadge[] {
  const spec = NODE_KIND_SPEC[node.kind];
  const out: NodeBadge[] = [];
  if (spec.badge) {
    out.push({ text: spec.badge, brand: spec.badgeBrand, labelKey: spec.labelKey });
  }
  if (node.kind === 'meraki' && isMerakiAccessPoint(node.merakiProductType)) {
    out.push({ text: 'AP', brand: null, labelKey: 'kindBadge.accessPoint' });
  }
  return out;
}
