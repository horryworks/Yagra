// SPDX-License-Identifier: AGPL-3.0-only
// Resolve a referenced entity (node / group / profile / threshold scope) to its human name for
// table cells. Policy (design-system §4.1 / ui-conventions): the visible primary is ALWAYS the
// name — the raw UUID is exposed only on hover (tooltip + copy), never as the cell's main text.
// Falls back to the raw id when the name can't be resolved (e.g. a deleted reference). Defined
// once here so every list reuses the same treatment instead of hand-rolling per-page resolvers.

import { useCallback, useEffect, useRef, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { api } from '../../services/api';
import type { NodeGroup, ProfileSummary, ScopeLevel } from '../../types/api';
import { createNameBatcher, type NameBatcher } from './entityNameBatch';
import { IconButton } from './IconButton';
import { CopyIcon } from './icons';

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

  /** Resolve a threshold scope id by its level. A `group`-scoped threshold's id is a tag value
   *  (already human-readable), so it falls through `groupName` unchanged when it isn't a folder id.
   *  A `global` rule has no id to resolve — it targets every node — so it resolves to the empty
   *  string and the caller shows the level badge alone. */
  const scopeName = useCallback(
    (level: ScopeLevel, id: string) =>
      level === 'global'
        ? ''
        : level === 'node'
          ? nodeName(id)
          : level === 'profile'
            ? profileName(id)
            : groupName(id),
    [nodeName, profileName, groupName],
  );

  return { groups, profiles, nodeName, groupName, profileName, scopeName };
}

/** Render a resolved entity name; the raw id is available only on hover (tooltip + copy). When the
 *  name couldn't be resolved (it equals the id, or no id was supplied) the raw handle is shown in
 *  mono so a UUID reads as an id rather than prose. */
export function EntityName({ name, id }: { name: string; id?: string }) {
  if (!isEntityResolved(name, id)) {
    const raw = name || id || '—';
    return (
      <span className="yt-entity-raw mono" title={raw}>
        {raw}
      </span>
    );
  }
  return <EntityNameResolved name={name} id={id as string} />;
}

function EntityNameResolved({ name, id }: { name: string; id: string }) {
  const { t } = useTranslation();
  const [copied, setCopied] = useState(false);
  const copy = () => {
    void navigator.clipboard?.writeText(id);
    setCopied(true);
    setTimeout(() => setCopied(false), 1200);
  };
  return (
    <span className="yt-entity" title={id}>
      <span className="yt-entity-name">{name}</span>
      <IconButton
        className="yt-entity-copy"
        title={copied ? t('copy.copied') : t('copy.copyId')}
        onClick={copy}
      >
        <CopyIcon />
      </IconButton>
    </span>
  );
}
