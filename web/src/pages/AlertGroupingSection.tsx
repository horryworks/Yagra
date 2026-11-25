// SPDX-License-Identifier: AGPL-3.0-only
// Alert grouping (ADR-236), a section of Alerts ▸ Notification delivery: how strongly the alerts of
// nodes that go down together are rolled up into one - the deployment's level and, right under it,
// its exceptions by folder and tag (ADR-262 decision 2; the weakest exception reaching a node wins).
// Each level shows what it would do to the outage open now. The judgement is in `alertGrouping.ts`.

import { useMemo, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { api, errMsg } from '../services/api';
import {
  GROUPING_LEVELS,
  type GroupingException,
  type GroupingLevel,
  type NodeGroup,
  type NodeTags,
} from '../types/api';
import { useLoad } from '../lib/useLoad';
import { DataTable, type Column } from '../components/ui/DataTable';
import { Button } from '../components/ui/Button';
import { EmptyState } from '../components/ui/EmptyState';
import { Modal } from '../components/ui/Modal';
import { Field, Select } from '../components/ui/Field';
import { Fields } from '../components/ui/FormLayout';
import { Segmented } from '../components/ui/Segmented';
import { InfoTip } from '../components/ui/InfoTip';
import { Inline, Stack } from '../components/ui/Page';
import { SectionHead } from '../components/ui/Section';
import { OverflowMenu } from '../components/ui/OverflowMenu';
import { EditIcon, TrashIcon } from '../components/ui/icons';
import { rowActionsWidth } from '../lib/rowActions';
import { useSubmit } from '../lib/useSubmit';
import { done } from '../lib/submitState';
import { FormError, FormFooter } from '../components/ui/FormFooter';
import { formatDuration } from '../lib/format';
import { AppliesToCell, AppliesToPicker } from '../components/AppliesTo/AppliesTo';
import {
  appliesToFrom,
  namesNothing,
  type AppliesTo,
} from '../components/AppliesTo/appliesToTarget';
import { isGroupingLevel, notifiedAt, replayAt } from './alertGrouping';

const EMPTY = {
  level: 'off' as GroupingLevel,
  wait: false,
  exceptions: [] as GroupingException[],
  now: { down: 0, notified: [] },
  last_7_days: null,
};
const WAIT_OPTIONS = ['now', 'wait'] as const;

export function AlertGroupingSection({
  canSystem,
  onError,
}: {
  canSystem: boolean;
  onError: (m: string) => void;
}) {
  const { t } = useTranslation('alertsConfig');
  const grouping = useLoad(() => api.getAlertGrouping(), [], { initial: EMPTY });
  const { data: groups } = useLoad<NodeGroup[]>(() => api.listNodeGroups(), [], { initial: [] });
  const { data: nodeTags } = useLoad<NodeTags | null>(() => api.listNodeTags(), [], {
    initial: null,
  });
  // `null` = closed; `'new'` = adding; an exception = editing it.
  const [editing, setEditing] = useState<GroupingException | 'new' | null>(null);
  const cfg = grouping.data;
  const rows = cfg.exceptions;

  const levelLabel = (l: GroupingLevel) => t(`grouping.level.${l}`);
  const setLevel = (v: string) => {
    if (!isGroupingLevel(v)) return;
    api
      .setAlertGroupingLevel(v)
      .then(grouping.reload)
      .catch((e: unknown) => onError(errMsg(e, t('grouping.err.level'))));
  };
  const setWait = (v: string) =>
    api
      .setAlertGroupingWait(v === 'wait')
      .then(grouping.reload)
      .catch((e: unknown) => onError(errMsg(e, t('grouping.err.wait'))));
  const replay = replayAt(cfg, cfg.level);
  const setExceptionLevel = (x: GroupingException, v: string) => {
    if (!isGroupingLevel(v)) return;
    api
      .updateGroupingException(x.id, { group_ids: x.group_ids, tags: x.tags, level: v })
      .then(grouping.reload)
      .catch((e: unknown) => onError(errMsg(e, t('grouping.err.level'))));
  };
  const remove = (x: GroupingException) =>
    api
      .deleteGroupingException(x.id)
      .then(grouping.reload)
      .catch((e: unknown) => onError(errMsg(e, t('grouping.err.remove'))));

  const columns = useMemo<Column<GroupingException>[]>(
    () => [
      {
        key: 'target',
        header: t('appliesTo.title'),
        width: '1.6fr',
        render: (x) => (
          <AppliesToCell
            groupIds={x.group_ids}
            tags={x.tags}
            join={x.join}
            tagMatch={x.tag_match}
            groups={groups}
            nodeTags={nodeTags}
          />
        ),
      },
      {
        key: 'level',
        header: t('grouping.cols.level'),
        width: '200px',
        render: (x) => (
          <Select
            value={x.level}
            disabled={!canSystem}
            aria-label={t('grouping.cols.level')}
            onChange={(e) => setExceptionLevel(x, e.target.value)}
          >
            {GROUPING_LEVELS.map((l) => (
              <option key={l} value={l}>
                {levelLabel(l)}
              </option>
            ))}
          </Select>
        ),
      },
      {
        key: 'actions',
        header: t('grouping.cols.actions'),
        width: rowActionsWidth(2),
        align: 'right',
        render: (x) =>
          canSystem ? (
            <span className="ytable-actions">
              <OverflowMenu
                actions={[
                  {
                    label: t('grouping.edit'),
                    icon: <EditIcon />,
                    onClick: () => setEditing(x),
                  },
                  {
                    label: t('grouping.remove'),
                    icon: <TrashIcon />,
                    danger: true,
                    onClick: () => void remove(x),
                  },
                ]}
              />
            </span>
          ) : null,
      },
    ],
    // The handlers are rebuilt every render; what they read is listed instead.
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [t, canSystem, groups, nodeTags],
  );

  return (
    <Stack as="section" gap={12}>
      <Stack gap={8} className="routing-grouping">
        <SectionHead
          level={2}
          title={
            <Inline as="span" gap={6}>
              {t('grouping.title')}
              <InfoTip infoKey="alertsConfig:grouping.info" label={t('grouping.title')} />
            </Inline>
          }
        />
        {canSystem ? (
          <Segmented
            ariaLabel={t('grouping.title')}
            value={cfg.level}
            onChange={setLevel}
            options={GROUPING_LEVELS.map((l) => ({
              value: l,
              label:
                cfg.now.down > 0
                  ? t('grouping.levelWithCount', { level: levelLabel(l), count: notifiedAt(cfg, l) })
                  : levelLabel(l),
            }))}
          />
        ) : (
          <strong>{levelLabel(cfg.level)}</strong>
        )}
        {canSystem ? (
          <Segmented
            ariaLabel={t('grouping.waitTitle')}
            value={cfg.wait ? 'wait' : 'now'}
            onChange={setWait}
            options={WAIT_OPTIONS.map((w) => ({ value: w, label: t(`grouping.wait.${w}`) }))}
          />
        ) : (
          <strong>{t(`grouping.wait.${cfg.wait ? 'wait' : 'now'}`)}</strong>
        )}
        <p className="form-status">
          {cfg.now.down > 0
            ? t('grouping.now', { down: cfg.now.down, count: notifiedAt(cfg, cfg.level) })
            : t('grouping.nowNone')}
        </p>
        {replay && (
          <p className="form-status">
            {replay.fired === 0
              ? t('grouping.last7None')
              : t(replay.maxDelaySecs == null ? 'grouping.last7' : 'grouping.last7Delay', {
                  fired: replay.fired,
                  count: replay.atOnce,
                  waiting: replay.waiting,
                  delay: formatDuration(replay.maxDelaySecs ?? 0, { units: 1 }),
                })}
            {replay.truncated && ` ${t('grouping.last7Partial')}`}
          </p>
        )}
      </Stack>
      {/* The exceptions sit inside the grouping, indented under the level they change, so they
          read as part of it rather than as a setting of their own (ADR-262 decision 2). */}
      <Stack gap={8} className="routing-grouping-exceptions">
        <SectionHead
          title={
            <Inline as="span" gap={6}>
              {t('grouping.exceptions')}
              <InfoTip
                infoKey="alertsConfig:grouping.exceptionsInfo.info"
                label={t('grouping.exceptions')}
              />
            </Inline>
          }
          actions={
            canSystem && (
              <Button variant="primary" onClick={() => setEditing('new')}>
                {t('grouping.add')}
              </Button>
            )
          }
        />
        <DataTable
          tableId="settings.alertGroupingExceptions"
          rows={rows}
          columns={columns}
          rowKey={(x) => x.id}
          loading={grouping.loading}
          empty={
            <EmptyState
              text={t('grouping.empty')}
              action={
                canSystem && (
                  <Button variant="primary" onClick={() => setEditing('new')}>
                    {t('grouping.add')}
                  </Button>
                )
              }
            />
          }
        />
      </Stack>
      {editing !== null && (
        <ExceptionModal
          exception={editing === 'new' ? undefined : editing}
          groups={groups}
          nodeTags={nodeTags}
          onClose={() => setEditing(null)}
          onDone={() => {
            setEditing(null);
            grouping.reload();
          }}
        />
      )}
    </Stack>
  );
}

/** Add an exception, or - given `exception` - replace what it applies to and its level. */
function ExceptionModal({
  exception,
  groups,
  nodeTags,
  onClose,
  onDone,
}: {
  exception?: GroupingException;
  groups: NodeGroup[];
  nodeTags: NodeTags | null;
  onClose: () => void;
  onDone: () => void;
}) {
  const { t } = useTranslation('alertsConfig');
  // An exception always names something, so the switch to "every node" is not offered.
  const [initial] = useState<AppliesTo>(() => ({
    ...appliesToFrom(
      exception?.group_ids ?? [],
      exception?.tags ?? [],
      exception?.join,
      exception?.tag_match,
    ),
    only: true,
  }));
  const [target, setTarget] = useState<AppliesTo>(initial);
  const [level, setLevel] = useState<GroupingLevel>(exception?.level ?? 'off');
  const form = useSubmit({ errorFallback: t('grouping.err.save'), onDone });
  const empty = namesNothing(target);

  const submit = () => {
    if (empty) return;
    const body = {
      group_ids: target.groupIds,
      tags: target.tags,
      join: target.join,
      tag_match: target.tagMatch,
      level,
    };
    form.submit(() =>
      (exception
        ? api.updateGroupingException(exception.id, body)
        : api.createGroupingException(body)
      ).then(() => done()),
    );
  };

  return (
    <Modal
      title={exception ? t('grouping.modal.editTitle') : t('grouping.modal.title')}
      onClose={onClose}
      footer={
        <FormFooter
          form={form}
          onClose={onClose}
          onSubmit={submit}
          submitLabel={exception ? t('grouping.modal.save') : t('grouping.modal.add')}
          canSubmit={!empty}
        />
      }
    >
      <Fields>
        <AppliesToPicker
          label={t('appliesTo.title')}
          value={target}
          onChange={setTarget}
          groups={groups}
          nodeTags={nodeTags}
          initial={initial}
          allowAll={false}
        />
        <Field label={t('grouping.cols.level')}>
          <Select
            id="grouping-level"
            value={level}
            onChange={(e) => isGroupingLevel(e.target.value) && setLevel(e.target.value)}
          >
            {GROUPING_LEVELS.map((l) => (
              <option key={l} value={l}>
                {t(`grouping.level.${l}`)}
              </option>
            ))}
          </Select>
        </Field>
        <FormError form={form} />
      </Fields>
    </Modal>
  );
}
