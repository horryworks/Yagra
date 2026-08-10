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

export interface NodeKindSpec {
  /** Short badge shown after a node's name. `null` = no badge: the ordinary device is the
   *  *unmarked default*, so a 50k-row inventory tree does not grow a badge on every line, and the
   *  badge means "this one is not a normal device". */
  readonly badge: string | null;
  /** `nodes`-namespace key naming the kind in prose (badge tooltip). Total over `NodeKind`. */
  readonly labelKey: string;
  /** The metric whose newest sample means "we heard from this monitor". Each kind is polled over a
   *  different protocol, so there is no shared one: an ICMP RTT series is empty for a URL monitor
   *  (it is never pinged) and for a Meraki device (the org collector polls it, not a per-node job).
   *  Used for the header's "seen …" clause; only `device`'s is also charted. */
  readonly livenessMetric: string;
}

export const NODE_KIND_SPEC: Record<NodeKind, NodeKindSpec> = {
  meraki: {
    badge: 'Meraki',
    labelKey: 'kind.meraki',
    livenessMetric: 'meraki_device_up',
  },
  url: { badge: 'URL', labelKey: 'kind.url', livenessMetric: 'http_up' },
  dns: { badge: 'DNS', labelKey: 'kind.dns', livenessMetric: 'dns_up' },
  device: { badge: null, labelKey: 'kind.device', livenessMetric: 'icmp_rtt_ms' },
};

// The badge strings are literals, not i18n keys, on purpose: `URL`, `DNS` and `Meraki` are the same
// text in English and Japanese (the hardcoded `Meraki` badge this generalizes already was), and
// routing three unchanging ASCII tokens through `t()` would add six locale entries that can only
// ever drift. The tooltip beside them *is* translated, which is what carries the meaning.
