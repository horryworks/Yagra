// SPDX-License-Identifier: AGPL-3.0-only
// What to tell the operator after a Meraki import (ADR-164): how many devices became nodes, and
// where they went — into folders by IP range, or under the organization's own folder.
//
// Here and not in the page because Vitest never runs a `.tsx`, and the judgement below is the part
// that can be wrong while looking fine: "3 filed by IP range" over a request that matched nothing
// would describe a deployment the operator does not have.

import type { MerakiImported } from '../../types/api';

/** One sentence of the message. The page joins them with a space. */
export interface MerakiImportMessagePart {
  key: string;
  args: Record<string, unknown>;
}

/**
 * The sentences, in order. `folder` is the organization's folder — everything that was not filed
 * by IP range went somewhere under it.
 *
 * 🚨 **The ambiguity sentence is separate, and only appears when there is one.** Folded into "under
 * the organization's folder" it would read as "no range covers these", when the truth is that two
 * folders carry overlapping ranges — which is a thing to go and fix.
 */
export function merakiImportMessage(
  result: MerakiImported,
  folder: string,
): MerakiImportMessagePart[] {
  const count = result.imported;
  // What was not imported, and why — said whether or not anything else was (ADR-164 決定 39). An MX
  // waiting for its LAN read is not "already monitored", so it must not fall into that sentence.
  const skipped: MerakiImportMessagePart[] = [];
  if (result.waiting_lan > 0) {
    skipped.push({ key: 'meraki.import.waitingLan', args: { count: result.waiting_lan } });
  }
  if (result.bound_elsewhere > 0) {
    skipped.push({ key: 'meraki.import.boundElsewhere', args: { count: result.bound_elsewhere } });
  }
  if (count === 0) {
    return skipped.length > 0 ? skipped : [{ key: 'meraki.import.doneNone', args: {} }];
  }
  const parts: MerakiImportMessagePart[] = [{ key: 'meraki.import.done', args: { count } }];
  const matched = result.filed.matched;
  const rest = count - matched;
  if (matched === 0) {
    parts.push({ key: 'meraki.import.filed.none', args: { folder } });
  } else if (rest === 0) {
    parts.push({ key: 'meraki.import.filed.all', args: {} });
  } else {
    parts.push({ key: 'meraki.import.filed.some', args: { matched, rest, folder } });
  }
  if (result.filed.ambiguous > 0) {
    parts.push({ key: 'meraki.import.filed.ambiguous', args: { count: result.filed.ambiguous } });
  }
  return [...parts, ...skipped];
}
