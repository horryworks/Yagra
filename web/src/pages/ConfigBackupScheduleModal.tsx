// SPDX-License-Identifier: AGPL-3.0-only
// The config backup schedule's dialog (ADR-210 stage 6): the daily start time, in the schedule's
// own zone (UTC until one is saved, ADR-221), and how many nodes are fetched at once. What it may send is `configBackupSchedule.ts`.

import { useState } from 'react';
import { useTranslation } from 'react-i18next';
import { Modal } from '../components/ui/Modal';
import { Field, Select } from '../components/ui/Field';
import { FormError, FormFooter } from '../components/ui/FormFooter';
import { api } from '../services/api';
import { done } from '../lib/submitState';
import { useSubmit } from '../lib/useSubmit';
import type { ConfigBackupScheduleView } from '../types/api';
import { scheduleBody, scheduleDraftTime } from './configBackupSchedule';
import { scheduleZone, zoneAbbreviation } from '../lib/timeZone';

interface Props {
  schedule: ConfigBackupScheduleView;
  onClose: () => void;
  onSaved: () => void;
}

export function ConfigBackupScheduleModal({ schedule, onClose, onSaved }: Props) {
  const { t } = useTranslation('nodes');
  // There is always one schedule, so it is never "new": a saved zone stays, and one that never
  // saved a zone is offered in the display zone, which saving then records.
  const zone = scheduleZone(schedule.time_zone, schedule.time_zone == null);
  const [time, setTime] = useState(() => scheduleDraftTime(schedule, zone));
  const [count, setCount] = useState(schedule.concurrency);
  const form = useSubmit({ errorFallback: t('config.schedule.err.save'), onDone: onSaved });
  const counts = Array.from({ length: schedule.max_concurrency }, (_, i) => i + 1);

  function save() {
    const parsed = scheduleBody(time, count, schedule.max_concurrency, zone);
    if (!parsed.ok) {
      form.refuse(t(parsed.key));
      return;
    }
    form.submit(async () => {
      await api.putConfigBackupSchedule(parsed.body);
      return done();
    });
  }

  return (
    <Modal
      title={t('config.schedule.title')}
      onClose={onClose}
      footer={
        <FormFooter
          form={form}
          onClose={onClose}
          onSubmit={save}
          submitLabel={t('config.schedule.save')}
          busyLabel={t('config.schedule.saving')}
        />
      }
    >
      <Field
        label={t('config.schedule.start', { zone: zoneAbbreviation(zone) })}
        htmlFor="cfgbl-sched-time"
      >
        <input
          id="cfgbl-sched-time"
          className="field"
          type="time"
          value={time}
          onChange={(e) => setTime(e.target.value)}
        />
      </Field>
      <Field label={t('config.schedule.atOnce')} htmlFor="cfgbl-sched-count">
        <Select
          id="cfgbl-sched-count"
          value={String(count)}
          onChange={(e) => setCount(Number(e.target.value))}
        >
          {counts.map((n) => (
            <option key={n} value={n}>
              {n}
            </option>
          ))}
        </Select>
      </Field>
      <FormError form={form} />
    </Modal>
  );
}
