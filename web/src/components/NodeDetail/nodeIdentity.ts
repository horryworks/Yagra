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

export interface SubLinePart {
  /** Stable key for the React list — the fact this part states, not its text. */
  id: 'address' | 'device' | 'url' | 'dnsName' | 'recordType';
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
    case 'meraki':
      return [
        { id: 'address', text: node.address, mono: true },
        {
          id: 'device',
          text: [node.vendor, node.model].filter(Boolean).join(' ') || t('detail.unknownDevice'),
        },
      ];
  }
}
