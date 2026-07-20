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
 *  the 101st+ node silently fell back to a raw UUID (S12). `nodeName()` enqueues any unseen id and an
 *  effect batch-resolves it via the whole-fleet `node-names` endpoint. */
export function useEntityNames() {
  const [groups, setGroups] = useState<NodeGroup[]>([]);
  const [profiles, setProfiles] = useState<ProfileSummary[]>([]);
  const [nodeNames, setNodeNames] = useState<Record<string, string>>({});
  const pending = useRef<Set<string>>(new Set());
  const requested = useRef<Set<string>>(new Set());

  useEffect(() => {
    api.listNodeGroups().then(setGroups).catch(() => undefined);
    api.listProfiles().then(setProfiles).catch(() => undefined);
  }, []);

  // After a render that referenced not-yet-resolved node ids, resolve them in one batch. Ids are
  // marked requested (whether or not they come back) so an unresolved reference — e.g. a deleted
  // node — is asked for once, never re-enqueued into a fetch loop. No dependency array: the enqueue
  // happens during the same render's `nodeName()` calls, so this must run after every commit and
  // early-returns when nothing is pending.
  useEffect(() => {
    if (pending.current.size === 0) return;
    const ids = [...pending.current];
    pending.current.clear();
    ids.forEach((id) => requested.current.add(id));
    api
      .getNodeNames(ids)
      .then((entries) => {
        if (entries.length === 0) return;
        setNodeNames((prev) => {
          const next = { ...prev };
          for (const e of entries) next[e.id] = e.name;
          return next;
        });
      })
      .catch(() => undefined);
  });

  const nodeName = useCallback(
    (id: string) => {
      const hit = nodeNames[id];
      if (hit != null) return hit;
      // Enqueue an unseen id once; return the raw id until the batch resolves it.
      if (id && !requested.current.has(id)) pending.current.add(id);
      return id;
    },
    [nodeNames],
  );
  const groupName = useCallback((id: string) => resolveName(groups, id), [groups]);
  const profileName = useCallback((id: string) => resolveName(profiles, id), [profiles]);

  /** Resolve a threshold scope id by its level. A `group`-scoped threshold's id is a tag value
   *  (already human-readable), so it falls through `groupName` unchanged when it isn't a folder id. */
  const scopeName = useCallback(
    (level: ScopeLevel, id: string) =>
      level === 'node' ? nodeName(id) : level === 'profile' ? profileName(id) : groupName(id),
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
