// SPDX-License-Identifier: AGPL-3.0-only
// The claims each kind card makes on the Overview's generic sections (ADR-046 Inc.8).
//
// The failure these pin is a metric drawn twice: once by the card, once under a family heading
// below it. The other direction matters as much — a claim for a metric the card does not draw
// hides it from the page — which is why the DNS card's two undrawn metrics are asserted absent.

import { describe, expect, it } from 'vitest';
import type { NodeDetail } from '../../types/api';
import { DNS_CARD, ICMP_LOSS_METRIC, kindCardClaims, MERAKI_CARD, URL_CARD } from './overviewClaims';

type Subject = Parameters<typeof kindCardClaims>[0];

const node = (over: Partial<NodeDetail>): Subject =>
  ({ kind: 'device', url_check: null, dns_check: null, meraki_device: null, ...over }) as Subject;

describe('kindCardClaims', () => {
  it('a device claims the ICMP pair — the liveness metric and the loss share', () => {
    expect([...kindCardClaims(node({ kind: 'device' }))].sort()).toEqual([ICMP_LOSS_METRIC, 'icmp_rtt_ms']);
  });

  it('a URL monitor claims its card metrics and every operator-named extraction', () => {
    const n = node({
      kind: 'url',
      url_check: { url: 'https://example.com/', json_extract: [{ metric: 'ymock_queue_depth' }] } as NodeDetail['url_check'],
    });
    const claimed = kindCardClaims(n);
    for (const m of Object.values(URL_CARD)) expect(claimed.has(m), m).toBe(true);
    expect(claimed.has('ymock_queue_depth')).toBe(true);
    // Not pinged, so the ICMP card is not mounted and must not claim.
    expect(claimed.has('icmp_rtt_ms')).toBe(false);
  });

  it('a DNS monitor claims only what its card draws', () => {
    const claimed = kindCardClaims(node({ kind: 'dns', dns_check: { record_type: 'A' } as NodeDetail['dns_check'] }));
    expect([...claimed].sort()).toEqual([...Object.values(DNS_CARD)].sort());
    // Drawn nowhere on the card, so they must stay visible below it.
    expect(claimed.has('dns_answer_count')).toBe(false);
    expect(claimed.has('dns_chain_length')).toBe(false);
  });

  it('a Meraki device claims its four card metrics', () => {
    const claimed = kindCardClaims(
      node({ kind: 'meraki', meraki_device: { serial: 'Q2XX' } as NodeDetail['meraki_device'] }),
    );
    expect([...claimed].sort()).toEqual([...Object.values(MERAKI_CARD)].sort());
  });

  it('claims nothing for a kind whose card is not mounted', () => {
    // `OverviewTab` gates each card on its config row, so a URL node with no `url_check` shows no
    // card — and a claim here would hide its metrics from the whole page.
    expect(kindCardClaims(node({ kind: 'url', url_check: null })).size).toBe(0);
    expect(kindCardClaims(node({ kind: 'dns', dns_check: null })).size).toBe(0);
    expect(kindCardClaims(node({ kind: 'meraki', meraki_device: null })).size).toBe(0);
  });
});
