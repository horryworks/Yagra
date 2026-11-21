// SPDX-License-Identifier: AGPL-3.0-only
// The parts of Notification delivery that sit with the routing rules but are not rules (ADR-262):
// the catch-all line under the rule list, the zone a rule's hours are read in when no folder names
// one, and the schedules an earlier version kept, which hold nothing any more.

import { useMemo, useState } from 'react';
import { Trans, useTranslation } from 'react-i18next';
import { api, errMsg } from '../services/api';
import type { DeliverySchedule, NodeGroup, NotificationChannel } from '../types/api';
import { useLoad } from '../lib/useLoad';
import { Button } from '../components/ui/Button';
import { Modal } from '../components/ui/Modal';
import { ConfirmDeleteModal } from '../components/ui/ConfirmDeleteModal';
import { Select } from '../components/ui/Field';
import { Fields } from '../components/ui/FormLayout';
import { CheckList } from '../components/ui/CheckList';
import { Callout } from '../components/ui/Section';
import { Inline, Stack } from '../components/ui/Page';
import { Badge } from '../components/ui/Badge';
import { knownTimeZones } from '../lib/timeZone';
import { groupOptions } from '../lib/nodeTree';
import { useSubmit } from '../lib/useSubmit';
import { done } from '../lib/submitState';
import { FormError, FormFooter } from '../components/ui/FormFooter';
import { daysText, hoursText } from './deliveryHours';

/** The catch-all, as the last line of the rule list: where an alert goes when no rule that matches
 *  it is open. A line rather than a table row, for the reason the default route is one: the rules
 *  table is filtered and counted in the browser, and a row that is not a rule would be too. */
export function CatchAllLine({
  channels,
  canSystem,
}: {
  channels: NotificationChannel[];
  canSystem: boolean;
}) {
  const { t } = useTranslation('alertsConfig');
  const load = useLoad(() => api.getRoutingCatchAll(), [], { initial: { channel_ids: [] } });
  const [editing, setEditing] = useState(false);
  const names = load.data.channel_ids
    .map((id) => channels.find((c) => c.id === id)?.name)
    .filter((n): n is string => n !== undefined);
  return (
    <Inline as="p" gap={8} className="routing-default-route">
      <span className="yt-name-txt">{t('routing.catchAll.title')}</span>
      <Badge tone="neutral">{t('routing.catchAll.when')}</Badge>
      <span className="muted">
        {names.length > 0 ? names.join(', ') : t('routing.catchAll.held')}
      </span>
      {canSystem && (
        <Button variant="outline" onClick={() => setEditing(true)}>
          {t('routing.catchAll.edit')}
        </Button>
      )}
      {editing && (
        <CatchAllModal
          channels={channels}
          initial={load.data.channel_ids}
          onClose={() => setEditing(false)}
          onDone={() => {
            setEditing(false);
            load.reload();
          }}
        />
      )}
    </Inline>
  );
}

function CatchAllModal({
  channels,
  initial,
  onClose,
  onDone,
}: {
  channels: NotificationChannel[];
  initial: string[];
  onClose: () => void;
  onDone: () => void;
}) {
  const { t } = useTranslation('alertsConfig');
  const [picked, setPicked] = useState<Set<string>>(() => new Set(initial));
  const form = useSubmit({ errorFallback: t('routing.catchAll.err'), onDone });
  const items = channels.map((c) => ({ key: c.id, label: c.name }));
  const submit = () =>
    form.submit(() =>
      api
        .setRoutingCatchAll(channels.map((c) => c.id).filter((id) => picked.has(id)))
        .then(() => done()),
    );
  return (
    <Modal
      title={t('routing.catchAll.title')}
      onClose={onClose}
      footer={
        <FormFooter
          form={form}
          onClose={onClose}
          onSubmit={submit}
          submitLabel={t('routing.ruleModal.save')}
        />
      }
    >
      <Fields>
        <CheckList items={items} selected={picked} onChange={setPicked} />
        <FormError form={form} />
      </Fields>
    </Modal>
  );
}

/** The zone a rule's hours are read in for a node no zoned folder sits above. */
export function DefaultZoneSelect({
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
  const zones = useMemo(() => knownTimeZones(), []);
  const setZone = (zone: string) =>
    api
      .setDefaultTimeZone(zone === '' ? null : zone)
      .then(hours.reload)
      .catch((e: unknown) => onError(errMsg(e, t('deliveryHours.err.zone'))));
  return (
    <label className="routing-zone">
      {t('deliveryHours.defaultZone')}
      <Select
        value={hours.data.default_time_zone ?? ''}
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
  );
}

/** The schedules an earlier version kept (ADR-262 decision 6). They hold nothing now, so the
 *  screen says so where they used to be, lists each one so it can be rebuilt as a rule, and lets
 *  it be deleted. Nothing is drawn once none is left. */
export function OldSchedules({
  groups,
  canSystem,
}: {
  groups: NodeGroup[];
  canSystem: boolean;
}) {
  const { t } = useTranslation('alertsConfig');
  const hours = useLoad(() => api.getDeliveryHours(), [], {
    initial: { default_time_zone: null, schedules: [] },
  });
  const [deleting, setDeleting] = useState<DeliverySchedule | null>(null);
  const schedules = hours.data.schedules;
  if (schedules.length === 0) return null;
  const paths = new Map(groupOptions(groups).map((o) => [o.id, o.path]));
  return (
    <Callout tone="warning" status>
      <Stack gap={8}>
        <span>{t('routing.oldSchedules.title', { count: schedules.length })}</span>
        {schedules.map((s) => (
          <Inline key={s.id} gap={8}>
            <strong>{s.name}</strong>
            <span>
              {daysText(t, s.days)} {hoursText(s)}
            </span>
            <span className="muted">
              {s.group_ids.map((g) => paths.get(g) ?? g).join(', ')}
            </span>
            {canSystem && (
              <Button variant="outline" onClick={() => setDeleting(s)}>
                {t('routing.oldSchedules.delete')}
              </Button>
            )}
          </Inline>
        ))}
      </Stack>
      {deleting && (
        <ConfirmDeleteModal
          title={t('routing.oldSchedules.delete')}
          onClose={() => setDeleting(null)}
          onConfirm={() => api.deleteDeliveryHours(deleting.id)}
          errorFallback={t('deliveryHours.err.delete')}
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
    </Callout>
  );
}
