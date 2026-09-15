// SPDX-License-Identifier: AGPL-3.0-only
// The signed-in account's pins on the inventory tree (ADR-146): loaded on sign-in, changed at once
// on screen, and put back if the server refuses.
//
// Not persisted in the browser. The server is the only home a pin has (`user_pins`), and a local copy
// would be a second answer to "what is pinned" that the next machine does not share.
//
// `status` decides whether any pin control is drawn at all. `unavailable` covers a core older than
// ADR-146 (404) as well as a first load that failed — in both, a press would go nowhere, and a
// control that cannot work is not drawn (ADR-056).

import { create } from 'zustand';
import { api, getToken } from './services/api';
import { withMember } from './lib/pins';
import type { NodeSummary } from './types/api';

export type PinsStatus = 'idle' | 'loading' | 'ready' | 'unavailable';

interface PinsState {
  status: PinsStatus;
  /** Pinned folder ids the caller may see. */
  groupIds: ReadonlySet<string>;
  /** Pinned node ids. Changed at once by a toggle; `nodes` catches up on the re-read after it. */
  nodeIds: ReadonlySet<string>;
  /** The pinned nodes as full rows, from the server. */
  nodes: readonly NodeSummary[];
  /** Read the pins. Signed out ⇒ does nothing. */
  load: () => Promise<void>;
  /** Forget everything. Call on sign-out, so the next account starts empty. */
  reset: () => void;
  /** Pin or unpin a node. Rejects (after putting the mark back) when the server refuses. */
  setNodePinned: (nodeId: string, on: boolean) => Promise<void>;
  /** Pin or unpin a folder. Rejects (after putting the mark back) when the server refuses. */
  setGroupPinned: (groupId: string, on: boolean) => Promise<void>;
}

const EMPTY: ReadonlySet<string> = new Set();

/** Bumped on every load and every reset, so an answer that arrives after sign-out — or after a newer
 *  load — is dropped instead of writing the previous account's pins over the next one's. */
let generation = 0;

export const usePinsStore = create<PinsState>()((set, get) => ({
  status: 'idle',
  groupIds: EMPTY,
  nodeIds: EMPTY,
  nodes: [],

  load: async () => {
    if (!getToken()) return;
    const mine = ++generation;
    if (get().status !== 'ready') set({ status: 'loading' });
    try {
      const pins = await api.getPins();
      if (mine !== generation) return;
      set({
        status: 'ready',
        groupIds: new Set(pins.group_ids),
        nodeIds: new Set(pins.nodes.map((n) => n.id)),
        nodes: pins.nodes,
      });
    } catch {
      if (mine !== generation) return;
      // A re-read that fails keeps what is on screen: only a store that never loaded hides the
      // controls. Hiding them after a transient error would take a button out from under a finger.
      if (get().status !== 'ready') set({ status: 'unavailable' });
    }
  },

  reset: () => {
    generation += 1;
    set({ status: 'idle', groupIds: EMPTY, nodeIds: EMPTY, nodes: [] });
  },

  setNodePinned: async (nodeId, on) => {
    set((s) => ({ nodeIds: withMember(s.nodeIds, nodeId, on) }));
    try {
      await (on ? api.pinNode(nodeId) : api.unpinNode(nodeId));
    } catch (e) {
      set((s) => ({ nodeIds: withMember(s.nodeIds, nodeId, !on) }));
      throw e;
    }
    // A newly pinned node has to arrive as a full row, or Pinned only has no folder to put it in.
    await get().load();
  },

  setGroupPinned: async (groupId, on) => {
    set((s) => ({ groupIds: withMember(s.groupIds, groupId, on) }));
    try {
      await (on ? api.pinGroup(groupId) : api.unpinGroup(groupId));
    } catch (e) {
      set((s) => ({ groupIds: withMember(s.groupIds, groupId, !on) }));
      throw e;
    }
  },
}));
