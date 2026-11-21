// SPDX-License-Identifier: AGPL-3.0-only
// The one way Notification delivery picks the nodes a setting applies to (ADR-262 decisions 1
// and 10): every node, or only the nodes inside one of some folders (at any depth) and/or carrying
// one of some tags, joined by or / and. A routing rule and an alert-grouping exception both use
// it, so the two cannot drift into picking nodes differently again.
//
// `AppliesToPicker` is the dialog half (a switch, the folder list, or / and, the tag list, and the
// pick written out with how many nodes it reaches); `AppliesToCell` is the list half (one chip per
// folder and tag, joined the same way).

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
import {
  JOINS,
  folderChoices,
  joinMatters,
  reachDetail,
  shownNames,
  type AppliesTo,
  type Join,
  type ReachParts,
} from './appliesToTarget';

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
  const both = joinMatters({ ...value, only });
  const key = `${value.groupIds.join(',')}|${value.tags.join('\n')}|${only}|${value.join}`;
  const { data: reach } = useLoad<{ count: number; parts: ReachParts | null } | null>(
    () =>
      only && !empty
        ? api
            .listNodeTags({ tags: value.tags, groups: value.groupIds, join: value.join })
            .then((r) =>
              r.reach === undefined || r.reach === null
                ? null
                : { count: r.reach, parts: r.reach_parts ?? null },
            )
        : Promise.resolve(null),
    [key],
    { initial: null },
  );
  const folderPath = (id: string) => folders.find((f) => f.id === id)?.label ?? id;

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
          {both && (
            <Inline justify="center">
              <Segmented
                size="sm"
                ariaLabel={t('appliesTo.join')}
                options={JOINS.map((j) => ({ value: j, label: t(JOIN_LABEL[j]) }))}
                value={value.join}
                onChange={(v) => {
                  const j = JOINS.find((x) => x === v);
                  if (j) onChange({ ...value, join: j });
                }}
              />
            </Inline>
          )}
          {tagItems.length > 0 && (
            <FieldGroup label={t('appliesTo.tags')}>
              <CheckList items={tagItems} selected={new Set(value.tags)} onChange={pickTags} />
            </FieldGroup>
          )}
          {empty ? (
            <p className="form-status">{t('appliesTo.reach', { count: 0 })}</p>
          ) : (
            reach !== null && (
              <p className="form-status">
                <PickLine
                  groupNames={value.groupIds.map(folderPath)}
                  tags={value.tags}
                  join={value.join}
                />
                {' · '}
                {t('appliesTo.reach', { count: reach.count })}
                {both && reach.parts && (
                  <span className="muted">
                    {' · '}
                    {(() => {
                      const d = reachDetail(reach.parts, value.join);
                      return t(d.key, d.values);
                    })()}
                  </span>
                )}
              </p>
            )
          )}
        </>
      )}
    </FieldGroup>
  );
}

/** The word for each join, spelled out so no key is built at runtime. */
const JOIN_LABEL: Record<Join, 'appliesTo.or' | 'appliesTo.and'> = {
  or: 'appliesTo.or',
  and: 'appliesTo.and',
};

/** The pick written out: the first two folders, then the first two tags, the lists joined by the
 *  setting's or / and ("Japan or WORLD"), the rest counted. */
function PickLine({
  groupNames,
  tags,
  join,
}: {
  groupNames: readonly string[];
  tags: readonly string[];
  join: Join;
}) {
  const { t } = useTranslation('alertsConfig');
  const side = (names: readonly string[], moreKey: 'appliesTo.moreFolders' | 'appliesTo.moreTags') => {
    const { shown, more } = shownNames(names);
    return [shown.join(', '), more > 0 ? t(moreKey, { count: more }) : '']
      .filter((s) => s !== '')
      .join(' ');
  };
  const parts = [
    groupNames.length > 0 ? side(groupNames, 'appliesTo.moreFolders') : '',
    tags.length > 0 ? side(tags, 'appliesTo.moreTags') : '',
  ].filter((s) => s !== '');
  return (
    <span>
      {parts.map((p, i) => (
        <span key={p}>
          {i > 0 && <span className="muted"> {t(JOIN_LABEL[join])} </span>}
          {p}
        </span>
      ))}
    </span>
  );
}

/** One chip per folder (its path) and per tag, or "All nodes". A tag no node carries, or a folder
 *  that no longer exists, is marked: it reaches nothing, silently. */
export function AppliesToCell({
  groupIds,
  tags,
  join,
  groups,
  nodeTags,
}: {
  groupIds: readonly string[];
  tags: readonly string[];
  /** How the folders and the tags combine; said between them only when both are named. */
  join: Join;
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
      {groupIds.length > 0 && tags.length > 0 && (
        <span className="muted">{t(JOIN_LABEL[join])}</span>
      )}
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
