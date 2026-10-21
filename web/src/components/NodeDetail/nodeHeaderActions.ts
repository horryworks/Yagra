// SPDX-License-Identifier: AGPL-3.0-only
// Which actions the node detail header draws, and where (ADR-209 decision 8): four buttons the
// operator reaches for most, and the rest behind "More ▾". The same set in the split pane and on
// the full page — before this, Move appeared only in the pane and Delete only on the page, and
// maintenance, mute, pool, Rediscover and "name from device" only in the tree's right-click menu.
//
// Pure, so the gating is tested: Vitest does not run a `.tsx`. The permissions are the ones each
// action's handler checks (ADR-056), the same the tree's menu is gated on.

import { canMoveByPrefix } from '../NodeTree/nodeTreeMenu';
import { deviceNameOffer } from './nodeEditForm';
import type { NodeDetail, NodeGroup } from '../../types/api';

/** Always-visible buttons, in this order. */
export const NODE_HEADER_PRIMARY = ['edit', 'maintenance', 'mute', 'pollNow'] as const;
export type NodeHeaderPrimary = (typeof NODE_HEADER_PRIMARY)[number];

/** "More ▾", in this order. `delete` is last, after a gap, because it cannot be undone. */
export const NODE_HEADER_MORE = [
  'openDetail',
  'map',
  'pin',
  'upstream',
  'move',
  'moveByPrefix',
  'pool',
  'rediscover',
  'nameFromDevice',
  'delete',
] as const;
export type NodeHeaderMore = (typeof NODE_HEADER_MORE)[number];

export interface NodeHeaderCaps {
  /** `manage_config` — edit, poll, move, pool, Rediscover, rename, delete. */
  canConfig: boolean;
  /** `manage_maintenance`. */
  canMaintenance: boolean;
  /** `ack_alerts` — what a mute needs. */
  canMute: boolean;
  /** The account's pins have loaded; until then a pin would press into nothing. */
  pinsReady: boolean;
  /** The split pane, which can open the node's own page; the page cannot open itself. */
  inline: boolean;
}

export function nodeHeaderActions(
  caps: NodeHeaderCaps,
  node: Pick<NodeDetail, 'kind' | 'sys_name' | 'name'>,
  groups: readonly NodeGroup[],
): { primary: NodeHeaderPrimary[]; more: NodeHeaderMore[] } {
  const primaryShown: Record<NodeHeaderPrimary, boolean> = {
    edit: caps.canConfig,
    maintenance: caps.canMaintenance,
    mute: caps.canMute,
    pollNow: caps.canConfig,
  };
  const device = node.kind === 'device';
  const moreShown: Record<NodeHeaderMore, boolean> = {
    openDetail: caps.inline,
    map: true,
    pin: caps.pinsReady,
    upstream: caps.canConfig,
    move: caps.canConfig,
    moveByPrefix: canMoveByPrefix(groups, caps.canConfig),
    pool: caps.canConfig,
    // Both re-read what the device itself says, so only a device has an answer to give.
    rediscover: caps.canConfig && device,
    nameFromDevice: caps.canConfig && deviceNameOffer(node, node.name) !== null,
    delete: caps.canConfig,
  };
  return {
    primary: NODE_HEADER_PRIMARY.filter((k) => primaryShown[k]),
    more: NODE_HEADER_MORE.filter((k) => moreShown[k]),
  };
}
