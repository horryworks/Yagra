// SPDX-License-Identifier: AGPL-3.0-only
// The one way Notification delivery picks the nodes a setting applies to (ADR-262 decision 1):
// every node, or only the nodes inside one of some folders (at any depth) or carrying one of some
// tags - either is enough. A routing rule and an alert-grouping exception both use it, so the two
// cannot drift into picking nodes differently again.
//
// `AppliesToPicker` is the dialog half (a switch, the folder list, the tag list, how many nodes it
// reaches); `AppliesToCell` is the list half (one chip per folder and tag).

import { useMemo } from 'react';
import { useTranslation } from 'react-i18next';
import { api } from '../../services/api';
import type { NodeGroup, NodeTags } from '../../types/api';
import { groupOptions } from '../../lib/nodeTree';
import { useLoad } from '../../lib/useLoad';
import { Badge } from '../ui/Badge';
import { CheckList } from '../ui/CheckList';
import { FieldGroup } from '../ui/FormLayout';
import { Inline } from '../ui/Page';
import { Segmented } from '../ui/Segmented';
import { tagChoices, tagsNoNodeCarries } from '../../pages/routingTags';
import { folderChoices, type AppliesTo } from './appliesToTarget';

interface PickerProps {
  value: AppliesTo;
  onChange: (next: AppliesTo) => void;
  /** Every folder, for the folder list. */
  groups: readonly NodeGroup[];
  /** The tags nodes carry; `null` when they could not be read (the saved tags are still listed). */
  nodeTags: NodeTags | null;
  /** What the setting named when the dialog opened, so a row unticked under the pointer stays. */
  initial: AppliesTo;
  /** The group label: "Nodes" on a rule, "Applies to" on an exception. */
  label: string;
  /** Whether "every node" can be chosen. An exception cannot: it would be the level itself. */
  allowAll: boolean;
}

export function AppliesToPicker({
  value,
  onChange,
  groups,
  nodeTags,
  initial,
  label,
  allowAll,
}: PickerProps) {
  const { t } = useTranslation('alertsConfig');
  const only = !allowAll || value.only;

  const folders = useMemo(() => folderChoices(groupOptions([...groups]), initial.groupIds), [
    groups,
    initial.groupIds,
  ]);
  const folderItems = useMemo(
    () =>
      folders.map((f) => ({
        key: f.id,
        label: f.label,
        mark: f.gone ? <Badge tone="warning">{t('appliesTo.goneFolder')}</Badge> : undefined,
      })),
    [folders, t],
  );
  const tags = useMemo(() => tagChoices(initial.tags, nodeTags), [initial.tags, nodeTags]);
  const tagItems = useMemo(
    () =>
      tags.map((c) => ({
        key: c.tag,
        label: c.tag,
        mark: c.gone ? <Badge tone="warning">{t('nodes:field.tagNoNode')}</Badge> : undefined,
        detail: c.nodes === null || c.gone ? undefined : t('appliesTo.nodes', { count: c.nodes }),
      })),
    [tags, t],
  );
  // Ticks keep each list's order, so what is saved does not depend on the order they were made in.
  const pickFolders = (next: Set<string>) =>
    onChange({ ...value, groupIds: folders.map((f) => f.id).filter((id) => next.has(id)) });
  const pickTags = (next: Set<string>) =>
    onChange({ ...value, tags: tags.map((c) => c.tag).filter((tag) => next.has(tag)) });

  // How many nodes the pick reaches, asked again whenever it changes. `null` while asking or after
  // a failed read, when the line is left out rather than showing a stale number.
  const empty = value.groupIds.length === 0 && value.tags.length === 0;
  const key = `${value.groupIds.join(',')}|${value.tags.join('\n')}|${only}`;
  const { data: reach } = useLoad<number | null>(
    () =>
      only && !empty
        ? api
            .listNodeTags({ tags: value.tags, groups: value.groupIds })
            .then((r) => r.reach ?? null)
        : Promise.resolve(null),
    [key],
    { initial: null },
  );

  return (
    <FieldGroup label={label}>
      {allowAll && (
        <Segmented
          options={[
            { value: 'all', label: t('appliesTo.allNodes') },
            { value: 'only', label: t('appliesTo.onlyThese') },
          ]}
          value={value.only ? 'only' : 'all'}
          onChange={(v) => onChange({ ...value, only: v === 'only' })}
        />
      )}
      {only && (
        <>
          {folderItems.length > 0 && (
            <FieldGroup label={t('appliesTo.folders')}>
              <CheckList
                items={folderItems}
                selected={new Set(value.groupIds)}
                onChange={pickFolders}
              />
            </FieldGroup>
          )}
          {tagItems.length > 0 && (
            <FieldGroup label={t('appliesTo.tags')}>
              <CheckList items={tagItems} selected={new Set(value.tags)} onChange={pickTags} />
            </FieldGroup>
          )}
          {empty ? (
            <p className="form-status">{t('appliesTo.reach', { count: 0 })}</p>
          ) : (
            reach !== null && <p className="form-status">{t('appliesTo.reach', { count: reach })}</p>
          )}
        </>
      )}
    </FieldGroup>
  );
}

/** One chip per folder (its path) and per tag, or "All nodes". A tag no node carries, or a folder
 *  that no longer exists, is marked: it reaches nothing, silently. */
export function AppliesToCell({
  groupIds,
  tags,
  groups,
  nodeTags,
}: {
  groupIds: readonly string[];
  tags: readonly string[];
  groups: readonly NodeGroup[];
  nodeTags: NodeTags | null;
}) {
  const { t } = useTranslation('alertsConfig');
  if (groupIds.length === 0 && tags.length === 0) {
    return <span className="muted">{t('appliesTo.allNodes')}</span>;
  }
  const paths = new Map(groupOptions([...groups]).map((o) => [o.id, o.path]));
  const goneTags = tagsNoNodeCarries(tags, nodeTags);
  return (
    <Inline gap={4}>
      {groupIds.map((id) => {
        const path = paths.get(id);
        return path !== undefined ? (
          <Badge key={id} tone="neutral">
            {path}
          </Badge>
        ) : (
          <Badge key={id} tone="warning">
            {t('appliesTo.goneFolder')}
          </Badge>
        );
      })}
      {tags.map((tag) =>
        goneTags?.has(tag) ? (
          <Badge key={tag} tone="warning">
            {tag} · {t('nodes:field.tagNoNode')}
          </Badge>
        ) : (
          <Badge key={tag} tone="tag">
            {tag}
          </Badge>
        ),
      )}
    </Inline>
  );
}
