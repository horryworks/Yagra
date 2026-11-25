// SPDX-License-Identifier: AGPL-3.0-only
// The one way Notification delivery picks the nodes a setting applies to (ADR-262 decisions 1
// and 10): every node, or only the nodes inside one of some folders (at any depth) and/or carrying
// one (or every one, ADR-267) of some tags, joined by or / and. A routing rule and an
// alert-grouping exception both use it, so the two cannot drift into picking nodes differently
// again.
//
// `AppliesToPicker` is the dialog half (a switch, the folder list, or / and, the tag list, has any
// / has all under it, and the pick written out with how many nodes it reaches); `AppliesToCell` is
// the list half (one chip per folder and tag, joined the same way).

import { Fragment, useMemo } from 'react';
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
import { TAG_MATCHES } from '../../types/api';
import { TAG_MATCH_LABEL, TAG_WORD } from '../../lib/tagMatch';
import {
  JOINS,
  bracketsTags,
  folderChoices,
  joinMatters,
  reachDetail,
  shownNames,
  spaceBefore,
  tagMatchMatters,
  tagSideText,
  type AppliesTo,
  type Join,
  type ReachParts,
  type TagMatch,
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
  const several = tagMatchMatters({ ...value, only });
  const key = `${value.groupIds.join(',')}|${value.tags.join('\n')}|${only}|${value.join}|${value.tagMatch}`;
  const { data: reach } = useLoad<{ count: number; parts: ReachParts | null } | null>(
    () =>
      only && !empty
        ? api
            .listNodeTags({
              tags: value.tags,
              groups: value.groupIds,
              join: value.join,
              tagMatch: value.tagMatch,
            })
            .then((r) =>
              r.reach === undefined || r.reach === null
                ? null
                : { count: r.reach, parts: r.reach_parts ?? null },
            )
        : Promise.resolve(null),
    [key],
    { initial: null },
  );
  // A folder that no longer exists is named as such, never by its raw id.
  const folderPath = (id: string) => {
    const f = folders.find((x) => x.id === id);
    return f && !f.gone ? f.label : t('appliesTo.goneFolder');
  };

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
          {/* Under the list, so a row ticked under the pointer does not move (ADR-267 decision 6). */}
          {several && (
            <Segmented
              size="sm"
              ariaLabel={t('common:tagMatch.label')}
              options={TAG_MATCHES.map((m) => ({ value: m, label: t(TAG_MATCH_LABEL[m]) }))}
              value={value.tagMatch}
              onChange={(v) => {
                const m = TAG_MATCHES.find((x) => x === v);
                if (m) onChange({ ...value, tagMatch: m });
              }}
            />
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
                  tagMatch={value.tagMatch}
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

/** The pick written out: the first two folders, then the first two tags joined by or / and
 *  (ADR-267), the lists joined by the setting's or / and ("Japan or (core and 24x7)"), the rest
 *  counted. */
function PickLine({
  groupNames,
  tags,
  join,
  tagMatch,
}: {
  groupNames: readonly string[];
  tags: readonly string[];
  join: Join;
  tagMatch: TagMatch;
}) {
  const { t } = useTranslation('alertsConfig');
  const { shown, more } = shownNames(groupNames);
  const folderSide = [shown.join(', '), more > 0 ? t('appliesTo.moreFolders', { count: more }) : '']
    .filter((s) => s !== '')
    .join(' ');
  const tagSide = tagSideText(tags, {
    word: t(TAG_WORD[tagMatch]),
    bracket: bracketsTags(groupNames.length, tags.length),
    open: t('common:tagMatch.open'),
    close: t('common:tagMatch.close'),
    more: (count) => t('appliesTo.moreTags', { count }),
  });
  const parts = [groupNames.length > 0 ? folderSide : '', tags.length > 0 ? tagSide : ''].filter(
    (s) => s !== '',
  );
  return (
    <span>
      {parts.map((p, i) => (
        <span key={p}>
          {i > 0 && (
            <span className="muted">
              {' '}
              {t(JOIN_LABEL[join])}
              {spaceBefore(p)}
            </span>
          )}
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
  tagMatch,
  groups,
  nodeTags,
}: {
  groupIds: readonly string[];
  tags: readonly string[];
  /** How the folders and the tags combine; said between them only when both are named. */
  join: Join;
  /** How the tags combine among themselves; said between every two (ADR-267 decision 6). */
  tagMatch: TagMatch;
  groups: readonly NodeGroup[];
  nodeTags: NodeTags | null;
}) {
  const { t } = useTranslation('alertsConfig');
  if (groupIds.length === 0 && tags.length === 0) {
    return <span className="muted">{t('appliesTo.allNodes')}</span>;
  }
  const paths = new Map(groupOptions([...groups]).map((o) => [o.id, o.path]));
  const goneTags = tagsNoNodeCarries(tags, nodeTags);
  const bracket = bracketsTags(groupIds.length, tags.length);
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
      {bracket && <span className="muted">{t('common:tagMatch.open')}</span>}
      {tags.map((tag, i) => (
        <Fragment key={tag}>
          {i > 0 && <span className="muted">{t(TAG_WORD[tagMatch])}</span>}
          {goneTags?.has(tag) ? (
            <Badge tone="warning">
              {tag} · {t('nodes:field.tagNoNode')}
            </Badge>
          ) : (
            <Badge tone="tag">{tag}</Badge>
          )}
        </Fragment>
      ))}
      {bracket && <span className="muted">{t('common:tagMatch.close')}</span>}
    </Inline>
  );
}
