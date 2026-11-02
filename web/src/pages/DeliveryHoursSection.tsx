// SPDX-License-Identifier: AGPL-3.0-only
// Delivery hours (ADR-226), the fourth section of Alerts ▸ Notification delivery: named schedules
// outside which notifications about the nodes they cover are held, and the time zone a node with no
// zoned folder above it keeps. The judgement is in `deliveryHours.ts`.

import { useMemo, useState } from 'react';
import { Trans, useTranslation } from 'react-i18next';
import { api, errMsg } from '../services/api';
import type { DeliverySchedule, NodeGroup } from '../types/api';
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
import { ConfirmDeleteModal } from '../components/ui/ConfirmDeleteModal';
import { OverflowMenu } from '../components/ui/OverflowMenu';
import { TextInput, Select } from '../components/ui/Field';
import { ChipInput } from '../components/ui/ChipInput';
import { GroupPicker } from '../components/ui/GroupPicker';
import { EditIcon, PowerIcon, TrashIcon } from '../components/ui/icons';
import { rowActionsWidth } from '../lib/rowActions';
import { groupOptions } from '../lib/nodeTree';
import { knownTimeZones } from '../lib/timeZone';
import { useSubmit } from '../lib/useSubmit';
import { done } from '../lib/submitState';
import { FormError, FormFooter } from '../components/ui/FormFooter';
import {
  daysText,
  hoursText,
  scheduleBodyFrom,
  scheduleDraftFrom,
  WEEKDAYS,
  weekdayName,
  type ScheduleDraft,
} from './deliveryHours';

export function DeliveryHoursSection({
  canSystem,
  onError,
}: {
  canSystem: boolean;
  onError: (m: string) => void;
}) {
  const { t } = useTranslation('alertsConfig');
  const hours = useLoad(() => api.getDeliveryHours(), [], {
    initial: { default_time_zone: null, schedules: [] },
  });
  const { data: groups } = useLoad<NodeGroup[]>(() => api.listNodeGroups(), [], { initial: [] });
  const { schedules, default_time_zone: defaultZone } = hours.data;
  const [adding, setAdding] = useState(false);
  const [editing, setEditing] = useState<DeliverySchedule | null>(null);
  const [deleting, setDeleting] = useState<DeliverySchedule | null>(null);
  const zones = useMemo(() => knownTimeZones(), []);

  const save = (s: DeliverySchedule, enabled: boolean) =>
    api
      .updateDeliveryHours(s.id, { ...s, enabled })
      .then(hours.reload)
      .catch((e: unknown) => onError(errMsg(e, t('deliveryHours.err.update'))));
  const setZone = (zone: string) =>
    api
      .setDefaultTimeZone(zone === '' ? null : zone)
      .then(hours.reload)
      .catch((e: unknown) => onError(errMsg(e, t('deliveryHours.err.zone'))));

  const groupName = (id: string) => groups.find((g) => g.id === id)?.name ?? id;

  const columns = useMemo<Column<DeliverySchedule>[]>(
    () => [
      {
        key: 'name',
        sortValue: (s) => s.name,
        header: t('deliveryHours.cols.name'),
        width: '1.2fr',
        render: (s) => <ClippedText className="yt-name-txt" text={s.name} />,
      },
      {
        key: 'days',
        sortValue: (s) => (s.days.length > 0 ? Math.min(...s.days) : null),
        header: t('deliveryHours.cols.days'),
        width: '160px',
        render: (s) => daysText(t, s.days),
      },
      {
        key: 'hours',
        sortValue: (s) => s.start_minute,
        header: t('deliveryHours.cols.hours'),
        width: '130px',
        render: (s) => <span className="mono">{hoursText(s)}</span>,
      },
      {
        key: 'appliesTo',
        sortValue: (s) => [...s.group_ids.map(groupName), ...s.labels].join(', ') || null,
        header: t('deliveryHours.cols.appliesTo'),
        width: '1.4fr',
        render: (s) => (
          <ClippedText
            className="muted"
            text={[...s.group_ids.map(groupName), ...s.labels].join(', ')}
          />
        ),
      },
      {
        key: 'status',
        sortValue: (s) => (s.enabled ? 0 : 1),
        header: t('deliveryHours.cols.status'),
        width: '130px',
        render: (s) => (
          <span className={s.enabled ? 'yt-status enabled' : 'yt-status disabled'}>
            <span className="yt-status-dot" />
            {s.enabled ? t('status.enabled') : t('status.disabled')}
          </span>
        ),
      },
      {
        key: 'actions',
        header: t('deliveryHours.cols.actions'),
        width: rowActionsWidth(3),
        align: 'right',
        render: (s) =>
          canSystem ? (
            <span className="ytable-actions">
              <OverflowMenu
                actions={[
                  { label: t('deliveryHours.edit'), icon: <EditIcon />, onClick: () => setEditing(s) },
                  {
                    label: s.enabled ? t('deliveryHours.disable') : t('deliveryHours.enable'),
                    icon: <PowerIcon />,
                    onClick: () => save(s, !s.enabled),
                  },
                  {
                    label: t('deliveryHours.delete'),
                    icon: <TrashIcon />,
                    danger: true,
                    onClick: () => setDeleting(s),
                  },
                ]}
              />
            </span>
          ) : null,
      },
    ],
    // `save` and `groupName` are rebuilt every render; what they read is listed instead.
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [t, canSystem, groups],
  );
  const filtering = useClientFilters(columns, schedules, { prefix: 'hours.' });
  const { filters, setFilters, shown, counts, anyFiltered } = filtering;
  const sorting = useClientSort(columns, filtering.shown, { prefix: 'hours.' });

  return (
    <section className="routing-rules-section">
      <ListToolbar
        list={filtering}
        labels={columnLabels(columns)}
        count={{
          shown: shown.length,
          total: schedules.length,
          noun: (n) => t('noun.schedule', { count: n }),
        }}
        leading={<h2 className="table-section-title">{t('deliveryHours.title')}</h2>}
      >
        <label className="routing-zone">
          {t('deliveryHours.defaultZone')}
          <Select
            value={defaultZone ?? ''}
            disabled={!canSystem}
            onChange={(e) => void setZone(e.target.value)}
          >
            <option value="">{t('deliveryHours.utc')}</option>
            {zones.map((z) => (
              <option key={z} value={z}>
                {z}
              </option>
            ))}
          </Select>
        </label>
        {canSystem && (
          <Button variant="primary" onClick={() => setAdding(true)}>
            {t('deliveryHours.add')}
          </Button>
        )}
      </ListToolbar>
      <DataTable
        tableId="settings.deliveryHours"
        rows={sorting.rows}
        columns={sorting.columns}
        sort={sorting.sort}
        onSortChange={sorting.setSort}
        rowKey={(s) => s.id}
        rowClass={(s) => (s.enabled ? undefined : 'is-muted')}
        filters={filters}
        onFiltersChange={setFilters}
        filterCounts={counts}
        loading={hours.loading}
        empty={
          anyFiltered ? (
            t('common:filter.noMatch')
          ) : (
            <EmptyState
              text={t('deliveryHours.empty')}
              action={
                canSystem && (
                  <Button variant="primary" onClick={() => setAdding(true)}>
                    {t('deliveryHours.add')}
                  </Button>
                )
              }
            />
          )
        }
      />
      {(adding || editing) && (
        <ScheduleModal
          schedule={editing ?? undefined}
          groups={groups}
          onClose={() => {
            setAdding(false);
            setEditing(null);
          }}
          onDone={() => {
            setAdding(false);
            setEditing(null);
            hours.reload();
          }}
        />
      )}
      {deleting && (
        <ConfirmDeleteModal
          title={t('deliveryHours.deleteTitle')}
          onConfirm={() => api.deleteDeliveryHours(deleting.id)}
          errorFallback={t('deliveryHours.err.delete')}
          onClose={() => setDeleting(null)}
          onDone={() => {
            setDeleting(null);
            hours.reload();
          }}
        >
          <Trans
            t={t}
            i18nKey="deliveryHours.deleteBody"
            values={{ name: deleting.name }}
            components={{ strong: <strong /> }}
          />
        </ConfirmDeleteModal>
      )}
    </section>
  );
}

function ScheduleModal({
  schedule,
  groups,
  onClose,
  onDone,
}: {
  schedule?: DeliverySchedule;
  groups: NodeGroup[];
  onClose: () => void;
  onDone: () => void;
}) {
  const { t } = useTranslation('alertsConfig');
  const [draft, setDraft] = useState<ScheduleDraft>(() => scheduleDraftFrom(schedule));
  const [picking, setPicking] = useState('');
  const form = useSubmit({ errorFallback: t('deliveryHours.err.save'), onDone });
  const options = useMemo(
    () => groupOptions(groups).filter((o) => !draft.groupIds.includes(o.id)),
    [groups, draft.groupIds],
  );
  const groupName = (id: string) => groups.find((g) => g.id === id)?.name ?? id;

  const toggleDay = (d: number) =>
    setDraft((cur) => ({
      ...cur,
      days: cur.days.includes(d) ? cur.days.filter((x) => x !== d) : [...cur.days, d],
    }));

  const submit = () => {
    const r = scheduleBodyFrom(draft, schedule?.enabled ?? true);
    if ('error' in r) {
      form.refuse(t(`deliveryHours.err.${r.error}`));
      return;
    }
    form.submit(() =>
      (schedule
        ? api.updateDeliveryHours(schedule.id, r.body)
        : api.createDeliveryHours(r.body)
      ).then(() => done()),
    );
  };

  return (
    <Modal
      title={
        schedule
          ? t('deliveryHours.modal.editTitle', { name: schedule.name })
          : t('deliveryHours.modal.title')
      }
      onClose={onClose}
      footer={
        <FormFooter
          form={form}
          onClose={onClose}
          onSubmit={submit}
          submitLabel={schedule ? t('deliveryHours.modal.save') : t('deliveryHours.modal.add')}
        />
      }
    >
      <div className="modal-field">
        <label className="modal-field-label" htmlFor="dh-name">
          {t('deliveryHours.modal.name')}
        </label>
        <TextInput
          id="dh-name"
          value={draft.name}
          onChange={(e) => setDraft({ ...draft, name: e.target.value })}
          autoFocus
        />
      </div>
      <div className="modal-field">
        <span className="modal-field-label">{t('deliveryHours.modal.days')}</span>
        <div className="routing-picks">
          {WEEKDAYS.map((d) => (
            <label key={d} className="routing-pick">
              <input type="checkbox" checked={draft.days.includes(d)} onChange={() => toggleDay(d)} />
              {weekdayName(t, d)}
            </label>
          ))}
        </div>
      </div>
      <div className="form-row">
        <div className="modal-field">
          <label className="modal-field-label" htmlFor="dh-start">
            {t('deliveryHours.modal.start')}
          </label>
          <TextInput
            id="dh-start"
            type="time"
            value={draft.start}
            onChange={(e) => setDraft({ ...draft, start: e.target.value })}
          />
        </div>
        <div className="modal-field">
          <label className="modal-field-label" htmlFor="dh-end">
            {t('deliveryHours.modal.end')}
          </label>
          <TextInput
            id="dh-end"
            type="time"
            value={draft.end}
            onChange={(e) => setDraft({ ...draft, end: e.target.value })}
          />
        </div>
      </div>
      <div className="modal-field">
        <span className="modal-field-label">{t('deliveryHours.modal.folders')}</span>
        {draft.groupIds.length > 0 && (
          <div className="routing-picks">
            {draft.groupIds.map((id) => (
              <Button
                key={id}
                variant="outline"
                aria-label={t('deliveryHours.modal.removeFolder', { name: groupName(id) })}
                onClick={() =>
                  setDraft({ ...draft, groupIds: draft.groupIds.filter((x) => x !== id) })
                }
              >
                {groupName(id)} ✕
              </Button>
            ))}
          </div>
        )}
        <GroupPicker
          options={options}
          value={picking}
          placeholder={t('deliveryHours.modal.addFolder')}
          onChange={(id) => {
            if (id) setDraft({ ...draft, groupIds: [...draft.groupIds, id] });
            setPicking('');
          }}
        />
      </div>
      <div className="modal-field">
        <span className="modal-field-label">{t('deliveryHours.modal.labels')}</span>
        <ChipInput
          value={draft.labels}
          onChange={(next) => setDraft({ ...draft, labels: next })}
          inputLabel={t('deliveryHours.modal.labels')}
        />
      </div>
      <FormError form={form} />
    </Modal>
  );
}
