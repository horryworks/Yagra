// Resolve a referenced entity (node / group / profile / threshold scope) to its human name for
// table cells. Policy (design-system §4.1 / ui-conventions): the visible primary is ALWAYS the
// name — the raw UUID is exposed only on hover (tooltip + copy), never as the cell's main text.
// Falls back to the raw id when the name can't be resolved (e.g. a deleted reference). Defined
// once here so every list reuses the same treatment instead of hand-rolling per-page resolvers.

import { useCallback, useEffect, useState } from 'react';
import { api } from '../../services/api';
import type { NodeGroup, NodeSummary, ProfileSummary, ScopeLevel } from '../../types/api';
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

/** Loads the node / group / profile inventories once and returns id→name resolvers. Each resolver
 *  returns the raw id unchanged when no match is found (so a deleted/unknown reference degrades to
 *  the only thing we have). Follows the existing per-page pattern (one fetch of each list). */
export function useEntityNames() {
  const [nodes, setNodes] = useState<NodeSummary[]>([]);
  const [groups, setGroups] = useState<NodeGroup[]>([]);
  const [profiles, setProfiles] = useState<ProfileSummary[]>([]);

  useEffect(() => {
    api.listNodes().then(setNodes).catch(() => undefined);
    api.listNodeGroups().then(setGroups).catch(() => undefined);
    api.listProfiles().then(setProfiles).catch(() => undefined);
  }, []);

  const nodeName = useCallback((id: string) => resolveName(nodes, id), [nodes]);
  const groupName = useCallback((id: string) => resolveName(groups, id), [groups]);
  const profileName = useCallback((id: string) => resolveName(profiles, id), [profiles]);

  /** Resolve a threshold scope id by its level. A `group`-scoped threshold's id is a tag value
   *  (already human-readable), so it falls through `groupName` unchanged when it isn't a folder id. */
  const scopeName = useCallback(
    (level: ScopeLevel, id: string) =>
      level === 'node' ? nodeName(id) : level === 'profile' ? profileName(id) : groupName(id),
    [nodeName, profileName, groupName],
  );

  return { nodes, groups, profiles, nodeName, groupName, profileName, scopeName };
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
        title={copied ? 'Copied' : 'Copy id'}
        onClick={copy}
      >
        <CopyIcon />
      </IconButton>
    </span>
  );
}
