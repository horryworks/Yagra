// SPDX-License-Identifier: AGPL-3.0-only
// Alert grouping (ADR-236), a section of Alerts ▸ Notification delivery: how strongly the alerts of
// nodes that go down together are rolled up into one, for the deployment and per folder. Each level
// shows what it would do to the outage open now. The judgement is in `alertGrouping.ts`.

import { useMemo, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { api, errMsg } from '../services/api';
import { GROUPING_LEVELS, type GroupingLevel, type NodeGroup } from '../types/api';
import { useLoad } from '../lib/useLoad';
import { DataTable, type Column } from '../components/ui/DataTable';
import { ListToolbar } from '../components/ui/ListToolbar';
import { columnLabels } from '../lib/listToolbar';
import { useClientFilters } from '../lib/useClientFilters';
import { useClientSort } from '../lib/useClientSort';
import { ClippedText } from '../components/ui/ClippedText';
import { Button } from '../components/ui/Button';
import { EmptyState } from '../components/ui/EmptyState';
import { Modal } from '../components/ui/Modal';
import { Select } from '../components/ui/Field';
import { Segmented } from '../components/ui/Segmented';
import { InfoTip } from '../components/ui/InfoTip';
import { GroupPicker } from '../components/ui/GroupPicker';
import { OverflowMenu } from '../components/ui/OverflowMenu';
import { TrashIcon } from '../components/ui/icons';
import { rowActionsWidth } from '../lib/rowActions';
import { groupOptions } from '../lib/nodeTree';
import { useSubmit } from '../lib/useSubmit';
import { done } from '../lib/submitState';
import { FormError, FormFooter } from '../components/ui/FormFooter';
import { folderLevelRows, isGroupingLevel, notifiedAt, type FolderLevelRow } from './alertGrouping';

const EMPTY = { level: 'off' as GroupingLevel, folders: [], now: { down: 0, notified: [] } };

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
  const [adding, setAdding] = useState(false);
  const cfg = grouping.data;
  const rows = useMemo(() => folderLevelRows(cfg, groups), [cfg, groups]);

  const levelLabel = (l: GroupingLevel) => t(`grouping.level.${l}`);
  const setLevel = (v: string) => {
    if (!isGroupingLevel(v)) return;
    api
      .setAlertGroupingLevel(v)
      .then(grouping.reload)
      .catch((e: unknown) => onError(errMsg(e, t('grouping.err.level'))));
  };
  const setFolder = (groupId: string, v: string) => {
    if (!isGroupingLevel(v)) return;
    api
      .setFolderGroupingLevel(groupId, v)
      .then(grouping.reload)
      .catch((e: unknown) => onError(errMsg(e, t('grouping.err.level'))));
  };
  const clearFolder = (groupId: string) =>
    api
      .clearFolderGroupingLevel(groupId)
      .then(grouping.reload)
      .catch((e: unknown) => onError(errMsg(e, t('grouping.err.remove'))));

  const columns = useMemo<Column<FolderLevelRow>[]>(
    () => [
      {
        key: 'folder',
        sortValue: (r) => r.name,
        header: t('grouping.cols.folder'),
        width: '1.4fr',
        render: (r) => <ClippedText className="yt-name-txt" text={r.name} />,
      },
      {
        key: 'level',
        sortValue: (r) => GROUPING_LEVELS.indexOf(r.level),
        header: t('grouping.cols.level'),
        width: '200px',
        render: (r) => (
          <Select
            value={r.level}
            disabled={!canSystem}
            aria-label={t('grouping.cols.level')}
            onChange={(e) => setFolder(r.groupId, e.target.value)}
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
        width: rowActionsWidth(1),
        align: 'right',
        render: (r) =>
          canSystem ? (
            <span className="ytable-actions">
              <OverflowMenu
                actions={[
                  {
                    label: t('grouping.remove'),
                    icon: <TrashIcon />,
                    danger: true,
                    onClick: () => void clearFolder(r.groupId),
                  },
                ]}
              />
            </span>
          ) : null,
      },
    ],
    // The handlers are rebuilt every render; what they read is listed instead.
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [t, canSystem],
  );
  const filtering = useClientFilters(columns, rows, { prefix: 'grouping.' });
  const sorting = useClientSort(columns, filtering.shown, { prefix: 'grouping.' });

  return (
    <section className="routing-rules-section">
      <div className="routing-grouping">
        <h2 className="table-section-title">
          {t('grouping.title')}
          <InfoTip infoKey="alertsConfig:grouping.info" label={t('grouping.title')} />
        </h2>
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
        <p className="form-status">
          {cfg.now.down > 0
            ? t('grouping.now', { down: cfg.now.down, count: notifiedAt(cfg, cfg.level) })
            : t('grouping.nowNone')}
        </p>
      </div>
      <ListToolbar
        list={filtering}
        labels={columnLabels(columns)}
        count={{
          shown: filtering.shown.length,
          total: rows.length,
          noun: (n) => t('noun.folder', { count: n }),
        }}
        leading={<h3 className="table-section-title">{t('grouping.folders')}</h3>}
      >
        {canSystem && (
          <Button variant="primary" onClick={() => setAdding(true)}>
            {t('grouping.add')}
          </Button>
        )}
      </ListToolbar>
      <DataTable
        tableId="settings.alertGroupingFolders"
        rows={sorting.rows}
        columns={sorting.columns}
        sort={sorting.sort}
        onSortChange={sorting.setSort}
        rowKey={(r) => r.groupId}
        filters={filtering.filters}
        onFiltersChange={filtering.setFilters}
        filterCounts={filtering.counts}
        loading={grouping.loading}
        empty={
          filtering.anyFiltered ? (
            t('common:filter.noMatch')
          ) : (
            <EmptyState
              text={t('grouping.empty')}
              action={
                canSystem && (
                  <Button variant="primary" onClick={() => setAdding(true)}>
                    {t('grouping.add')}
                  </Button>
                )
              }
            />
          )
        }
      />
      {adding && (
        <FolderLevelModal
          groups={groups}
          taken={rows.map((r) => r.groupId)}
          onClose={() => setAdding(false)}
          onDone={() => {
            setAdding(false);
            grouping.reload();
          }}
        />
      )}
    </section>
  );
}

function FolderLevelModal({
  groups,
  taken,
  onClose,
  onDone,
}: {
  groups: NodeGroup[];
  taken: string[];
  onClose: () => void;
  onDone: () => void;
}) {
  const { t } = useTranslation('alertsConfig');
  const [groupId, setGroupId] = useState('');
  const [level, setLevel] = useState<GroupingLevel>('certain');
  const form = useSubmit({ errorFallback: t('grouping.err.save'), onDone });
  const options = useMemo(
    () => groupOptions(groups).filter((o) => !taken.includes(o.id)),
    [groups, taken],
  );

  const submit = () => {
    if (!groupId) {
      form.refuse(t('grouping.err.noFolder'));
      return;
    }
    form.submit(() => api.setFolderGroupingLevel(groupId, level).then(() => done()));
  };

  return (
    <Modal
      title={t('grouping.modal.title')}
      onClose={onClose}
      footer={
        <FormFooter
          form={form}
          onClose={onClose}
          onSubmit={submit}
          submitLabel={t('grouping.modal.add')}
        />
      }
    >
      <div className="modal-field">
        <span className="modal-field-label">{t('grouping.cols.folder')}</span>
        <GroupPicker
          options={options}
          value={groupId}
          placeholder={t('grouping.modal.pickFolder')}
          onChange={setGroupId}
        />
      </div>
      <div className="modal-field">
        <label className="modal-field-label" htmlFor="grouping-level">
          {t('grouping.cols.level')}
        </label>
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
      </div>
      <FormError form={form} />
    </Modal>
  );
}
