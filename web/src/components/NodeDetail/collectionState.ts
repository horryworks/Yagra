// SPDX-License-Identifier: AGPL-3.0-only
// What the Collection tab's status card says about a node.

import type { NodeMetricEntry } from '../../types/api';

/** `ok` / `failing` / `none` are about the device. `unknown` and `loading` are about **Yagra's own
 *  read** — it failed, or it has not answered yet — and neither may borrow a device verdict. */
export const COLLECTION_STATES = ['ok', 'failing', 'none', 'unknown', 'loading'] as const;
export type CollectionState = (typeof COLLECTION_STATES)[number];

/**
 * The card's verdict, from whether the node is walked at all and what its metric inventory says.
 *
 * 🚨 **`metrics === null` means the inventory could not be read, and it is not `[]`.** The two used
 * to be one: the shared cache turned a failed `/nodes/{id}/metrics` into an empty list, and an
 * empty list on an SNMP node reads as "nothing is flowing" — so a TSDB that was briefly unreachable
 * painted a healthy device's card red with "Collection failing". That is a claim about the device
 * derived from a failure to ask.
 *
 * `undefined` is "not answered yet". It used to fall through to `failing` as well, so every SNMP
 * node's card flashed red for one round trip on the way to green.
 *
 * `hasSnmp` is the server's answer (`node.snmp_configured`), never `!!node.credential_id` — see the
 * call site.
 */
export function collectionState(
  hasSnmp: boolean,
  metrics: readonly NodeMetricEntry[] | null | undefined,
): CollectionState {
  if (!hasSnmp) return 'none';
  if (metrics === undefined) return 'loading';
  if (metrics === null) return 'unknown';
  return metrics.some((m) => m.status !== 'no_data') ? 'ok' : 'failing';
}
