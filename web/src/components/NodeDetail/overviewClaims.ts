// SPDX-License-Identifier: AGPL-3.0-only
// Which metrics the node Overview's kind-specific cards draw (ADR-046 Inc.8).
//
// The generic sections below Device health subtract these, the same way they subtract what
// Device health resolved onto (`claimedMetrics`): a value already charted by the ICMP, URL, DNS
// or Meraki card must not be charted a second time under a family heading. The cards read their
// metric names from here too, so the claim and the fetch cannot drift apart — which is how the
// URL card's metrics and the ICMP round-trip time came to be drawn twice for as long as the
// generic section existed: it subtracted Device health's claims and nobody else's.
//
// A `.ts` on purpose: Vitest never executes a `.tsx` (`testing.md`).

import { NODE_KIND_SPEC } from '../../lib/nodeKind';
import type { NodeDetail } from '../../types/api';
import { overviewShowsIcmp } from './overviewFacts';
import { extractMetricKey, metricsFromKey } from './urlExtracts';

/** The ICMP card's second metric. Its first is the device kind's liveness metric (`NODE_KIND_SPEC`). */
export const ICMP_LOSS_METRIC = 'icmp_loss_pct';

/** The URL-monitor card's metrics, by what each one is on the card. */
export const URL_CARD = {
  up: 'http_up',
  status: 'http_status_code',
  cert: 'ssl_cert_days_to_expiry',
  responseMs: 'http_response_time_ms',
  bodyMatch: 'http_body_match',
  bodyTruncated: 'http_body_truncated',
} as const;

/**
 * The DNS-monitor card's metrics.
 *
 * `dns_answer_count` and `dns_chain_length` are deliberately absent: the card does not draw them,
 * so they stay visible — under the DNS monitor heading of the generic sections, right below the
 * card. Claiming a metric a card does not draw would hide it from the page altogether.
 */
export const DNS_CARD = { up: 'dns_up', resolveMs: 'dns_resolve_ms' } as const;

/** The Meraki card's metrics: the device's availability and, for an MX, each WAN uplink's state and
 *  average rates (ADR-164 増分 13). Per-uplink loss, latency and the failed flag are not drawn here
 *  — a Meraki node has no Interfaces tab, so they show on the Collection tab. */
export const MERAKI_CARD = {
  up: 'meraki_device_up',
  sentBps: 'meraki_uplink_sent_bps',
  recvBps: 'meraki_uplink_recv_bps',
  status: 'meraki_uplink_status',
  vpnHubsReachable: 'meraki_vpn_hubs_reachable',
  vpnHubsUnreachable: 'meraki_vpn_hubs_unreachable',
  vpnSpokesUnreachable: 'meraki_vpn_spokes_unreachable',
  // A Meraki access point's (ADR-168): the same names a controller-walked AP publishes.
  clients: 'wlan_ap_client_count',
  ssids: 'wlan_ap_ssid_count',
  radioUtil: 'wlan_radio_channel_util_pct',
  radioNonWifi: 'wlan_radio_non_wifi_util_pct',
} as const;

/**
 * What the kind-specific cards on `node`'s Overview draw, and so what the generic sections must
 * not draw again.
 *
 * Conditional on the node, not on the kind alone: `OverviewTab` mounts a card only when the
 * kind's config row is present, so a URL node with no `url_check` has no card and claims nothing
 * — its metrics, if any, stay visible below. The URL card also draws every operator-named
 * extraction (`json_extract`), and those names are known to nothing but the check itself.
 */
export function kindCardClaims(
  node: Pick<NodeDetail, 'kind' | 'url_check' | 'dns_check' | 'meraki_device'>,
): Set<string> {
  const out = new Set<string>();
  if (overviewShowsIcmp(node.kind)) {
    out.add(NODE_KIND_SPEC[node.kind].livenessMetric);
    out.add(ICMP_LOSS_METRIC);
  }
  if (node.kind === 'url' && node.url_check) {
    for (const m of Object.values(URL_CARD)) out.add(m);
    for (const m of metricsFromKey(extractMetricKey(node.url_check.json_extract ?? []))) out.add(m);
  }
  if (node.kind === 'dns' && node.dns_check) {
    for (const m of Object.values(DNS_CARD)) out.add(m);
  }
  if (node.kind === 'meraki' && node.meraki_device) {
    for (const m of Object.values(MERAKI_CARD)) out.add(m);
  }
  return out;
}
