// SPDX-License-Identifier: AGPL-3.0-only
// Resolving a referenced entity's id to its human name. The rendering half is `EntityName.tsx`; this
// half is kept apart so that file exports only components (react-refresh's rule), and so the pure
// resolvers sit in a module a test runs.

import { useCallback, useEffect, useRef, useState } from 'react';
import { api } from '../../services/api';
import type { NodeGroup, ProfileSummary, ScopeLevel } from '../../types/api';
import { splitInterfaceScopeId } from '../../lib/interfaceScope';
import { createNameBatcher, type NameBatcher } from './entityNameBatch';

/** Resolve an id to a name from a `{id,name}[]` list, falling back to the raw id when no match is
 *  found (so a deleted/unknown reference degrades to the only handle we have). Pure — unit-tested. */
export function resolveName(list: { id: string; name: string }[], id: string): string {
  return list.find((e) => e.id === id)?.name ?? id;
}

/** Whether a name actually resolved (so the cell shows it as primary text with the id on hover),
 *  vs. an unresolved reference (no id, or the name fell back to the raw id). Pure — unit-tested. */
export function isEntityResolved(name: string, id?: string): boolean {
  return id != null && id !== '' && id !== name;
}

/** Returns id→name resolvers for node / group / profile references. Each resolver returns the raw
 *  id unchanged when no match is found (so a deleted/unknown reference degrades to the only thing we
 *  have).
 *
 *  Group and profile inventories are bounded, so they're fetched once eagerly. **Node** names
 *  resolve lazily and in batches: a table only references the ids on its current page, and the fleet
 *  can far exceed a single list page — the old eager `listNodes()` capped at 100, so a reference to
 *  the 101st+ node silently fell back to a raw UUID (S12). `nodeName()` enqueues any unseen id and
 *  `entityNameBatch.ts` resolves it via the whole-fleet `node-names` endpoint.
 *
 *  ⚠️ The batch is scheduled by the enqueue, **not** by an effect here, and that is a fix rather
 *  than a style choice: the ids are enqueued while the *cells* render, which for a virtualized list
 *  is a commit this component does not take part in — so the effect that used to send the request
 *  never ran and the row kept showing a raw UUID. See `entityNameBatch.ts` for the full account. */
export function useEntityNames() {
  const [groups, setGroups] = useState<NodeGroup[]>([]);
  const [profiles, setProfiles] = useState<ProfileSummary[]>([]);
  const [nodeNames, setNodeNames] = useState<Record<string, string>>({});

  useEffect(() => {
    api.listNodeGroups().then(setGroups).catch(() => undefined);
    api.listProfiles().then(setProfiles).catch(() => undefined);
  }, []);

  // One batcher per hook instance, created on first render (the lazy-ref idiom — `useState`'s
  // initializer would work too, but nothing ever sets it). Its dependencies are all stable:
  // `api.getNodeNames` is module-level and `setNodeNames` is a setState.
  const batcher = useRef<NameBatcher | null>(null);
  if (batcher.current === null) {
    batcher.current = createNameBatcher({
      fetchNames: (ids) => api.getNodeNames(ids),
      // A macrotask, not a microtask: everything a whole render pass asks about coalesces into one
      // request, and the flush lands after the commit rather than part-way through rendering.
      schedule: (flush) => {
        setTimeout(flush, 0);
      },
      onResolved: (entries) =>
        setNodeNames((prev) => {
          const next = { ...prev };
          for (const e of entries) next[e.id] = e.name;
          return next;
        }),
    });
  }

  const nodeName = useCallback(
    (id: string) => {
      const hit = nodeNames[id];
      if (hit != null) return hit;
      // Ask for an unseen id; return the raw id until the batch resolves it.
      batcher.current?.request(id);
      return id;
    },
    [nodeNames],
  );
  const groupName = useCallback((id: string) => resolveName(groups, id), [groups]);
  const profileName = useCallback((id: string) => resolveName(profiles, id), [profiles]);

  /** Resolve a threshold scope id by its level. A `global` rule has no id to resolve — it targets
   *  every node — so it resolves to the empty string and the caller shows the level badge alone.
   *
   *  ⚠️ The two group levels are named separately rather than sharing a fallthrough: `group_id` is
   *  a folder in the inventory tree and always resolves, while `group` is a **tag value** that is
   *  already human-readable and is handed to `groupName` only so an older rule carrying a folder
   *  id still reads as a name. Collapsing them would work today by coincidence. */
  const scopeName = useCallback(
    (level: ScopeLevel, id: string) => {
      switch (level) {
        case 'global':
          return '';
        case 'node':
          return nodeName(id);
        case 'profile':
          return profileName(id);
        case 'group_id':
        case 'group':
          return groupName(id);
        // `<node-uuid>:<ifindex>` (ADR-076). Only the node half is resolvable here — this hook
        // holds the fleet's node, group and profile names, not any node's interface roster, and
        // fetching one per row would be a request per rule. The port is shown as its index, which
        // is also what the alert itself carries.
        case 'interface': {
          const [node, port] = splitInterfaceScopeId(id);
          return port === null ? nodeName(id) : `${nodeName(node)} · #${port}`;
        }
      }
    },
    [nodeName, profileName, groupName],
  );

  return { groups, profiles, nodeName, groupName, profileName, scopeName };
}
