// SPDX-License-Identifier: AGPL-3.0-only
// The mark on a folder an integration made and still keeps (ADR-164 Inc.7).
//
// A folder an integration made does not behave like one an operator made: a Meraki organization's
// tree is deleted with the organization, and a NetBox folder is renamed and re-parented by the
// next sync whatever was typed over it. A tree that draws both kinds the same leaves that to be
// found out — by an edit that comes undone. The badge is the fact, on the row, before the edit.
// It says who keeps the folder *now*: forgetting a NetBox server leaves its folders behind as
// ordinary ones, and the server stops sending an origin for them.
//
// A `.ts` rather than two lines in `NodeTree.tsx`, because the one decision here — what to draw
// for a token this build has never heard of — is judgement, and Vitest never loads a `.tsx`.

import { GROUP_ORIGINS, type GroupOrigin, type NodeGroup } from '../types/api';
import type { BadgeBrand } from './brandBadge';

/** The badge text per origin.
 *
 *  Brand names, so deliberately not translated — they read the same in both locales, and the
 *  sentence that *explains* the badge is `nodes:tree.origin.<origin>`. A `Record` over the union:
 *  a third integration that starts keeping folders fails to compile here until it has a badge. */
export const GROUP_ORIGIN_BADGES: Record<GroupOrigin, string> = {
  meraki: 'Meraki',
  netbox: 'NetBox',
};

/** Whose colours each origin's badge wears (`lib/brandBadge.ts`): Meraki's green, NetBox's blue
 *  on white (user decisions, 2026-09-23). A `Record` so a third origin decides rather than
 *  inherits. */
export const GROUP_ORIGIN_BADGE_BRANDS: Record<GroupOrigin, BadgeBrand | null> = {
  meraki: 'meraki',
  netbox: 'netbox',
};

/** The origin to mark a folder with, or `null` for none.
 *
 *  `null` and an absent field are a folder a person made. The third case is why this is not a
 *  plain read of the field: a newer core can send an origin this build does not know (the WebUI
 *  and the core are not always the same version), and indexing the two maps with it would draw an
 *  empty badge titled with a raw `tree.origin.<token>` key. No badge is the honest answer — it
 *  says nothing, where the alternative says something false. */
export function groupOriginOf(group: Pick<NodeGroup, 'origin'>): GroupOrigin | null {
  const origin: string | null | undefined = group.origin;
  return GROUP_ORIGINS.find((known) => known === origin) ?? null;
}
