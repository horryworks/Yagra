// SPDX-License-Identifier: AGPL-3.0-only
// The node-detail header's sub line — the one-line "what am I looking at" under the node's name.
//
// It was `<address> · <vendor model | unknown device>` for every kind, which is only true of an
// ordinary device. A DNS monitor using the system resolver has no address of its own (the API
// stores 0.0.0.0 — `checks.rs`'s `NO_ADDRESS`) and no vendor, so the line read
// `0.0.0.0 · unknown device`: two facts that are not merely missing but meaningless for that kind,
// while the one fact that identifies it — the name it resolves — was nowhere on the line.
//
// Pure so Vitest can reach it (`testing.md`: `.tsx` tests never run). `t` is threaded in as an
// argument rather than pulled from a hook, matching `lib/pool.ts::polledByLabel`.

import type { TFunction } from 'i18next';
import type { NodeDetail } from '../../types/api';
import { addressText, hasAddress } from '../../lib/nodeAddress';

export interface SubLinePart {
  /** Stable key for the React list — the fact this part states, not its text. */
  id: 'address' | 'device' | 'url' | 'dnsName' | 'recordType' | 'merakiSite';
  text: string;
  /** Render in the monospace family (addresses, URLs, hostnames — `ui-conventions.md`). */
  mono?: boolean;
}

/** The identity parts of the header sub line, in reading order. The caller joins them with the
 *  `·` separator and appends the "seen …" clause, which is kind-independent.
 *
 *  Never empty: every kind states at least one thing about itself. */
export function nodeSubLineParts(node: NodeDetail, t: TFunction): SubLinePart[] {
  switch (node.kind) {
    case 'url':
      // The URL is the monitor's whole identity; `address` is only the host's resolved IP, kept
      // server-side because `nodes.address` is INET NOT NULL. It is not what the operator named.
      return node.url_check
        ? [{ id: 'url', text: node.url_check.url, mono: true }]
        : [{ id: 'address', text: node.address, mono: true }];
    case 'dns': {
      if (!node.dns_check) return [{ id: 'address', text: node.address, mono: true }];
      const parts: SubLinePart[] = [{ id: 'dnsName', text: node.dns_check.name, mono: true }];
      // `record_type` is optional on the wire (the backend defaults it to A). Absent means the
      // record type is simply not stated, not that the line should read "undefined".
      if (node.dns_check.record_type) {
        parts.push({ id: 'recordType', text: node.dns_check.record_type });
      }
      return parts;
    }
    case 'device':
    case 'wireless_ap':
      return deviceParts(node, t);
    case 'meraki': {
      // Which organization and network it sits in (ADR-185) — the one thing the Dashboard knows
      // about a Meraki device that the address and the model do not say.
      const site = merakiSiteText(node.meraki_site);
      return site ? [...deviceParts(node, t), { id: 'merakiSite', text: site }] : deviceParts(node, t);
    }
  }
}

function deviceParts(node: NodeDetail, t: TFunction): SubLinePart[] {
  return [
    {
      id: 'address',
      // A mesh repeater, or any node stored at the unspecified address, says it has none
      // rather than showing `0.0.0.0` (ADR-175).
      text: addressText(node.address, t, { meshRepeater: node.meraki_repeater }),
      mono: hasAddress(node.address),
    },
    {
      id: 'device',
      text: [node.vendor, node.model].filter(Boolean).join(' ') || t('detail.unknownDevice'),
    },
  ];
}

/** `Organization / Network`, or the organization alone when no sync has named the network yet.
 *  `null` when there is nothing to say — no binding, or a detail from a core that predates it. */
export function merakiSiteText(site: NodeDetail['meraki_site']): string | null {
  if (!site) return null;
  return [site.org_name, site.network_name].filter(Boolean).join(' / ') || null;
}
