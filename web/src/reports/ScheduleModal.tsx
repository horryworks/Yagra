// SPDX-License-Identifier: AGPL-3.0-only
// Schedule editor — create/edit a preset cadence for a report definition. Times are UTC (the
// backend computes next_run_at in UTC). Weekly adds a weekday picker; monthly a day-of-month.

import { useState } from 'react';
import { useTranslation } from 'react-i18next';
import { Modal } from '../components/ui/Modal';
import { Button } from '../components/ui/Button';
import { Select, RequiredMark, FieldHint } from '../components/ui/Field';
import { api, ApiError } from '../services/api';
import type { ReportDefinition, Cadence, ReportSchedule } from '../types/api';
import { WEEKDAY_OPTIONS } from '../lib/cadence';
import { SELECTABLE_CADENCES } from '../lib/cadence';
import { timeValue } from '../lib/format';

interface Props {
  definitions: ReportDefinition[];
  /** The schedule to edit, or null to create a new one. */
  schedule: ReportSchedule | null;
  onClose: () => void;
  onSaved: () => void;
}

export function ScheduleModal({ definitions, schedule, onClose, onSaved }: Props) {
  const { t } = useTranslation('reports');
  const [definitionId, setDefinitionId] = useState(
    schedule?.definition_id ?? definitions[0]?.id ?? '',
  );
  // 🚨 A stored `unknown` cadence (written by a newer core) stays `unknown` until the operator
  // picks one. This used to open on `daily`, and Save then wrote daily — so opening a monthly
  // report's schedule on an older bundle and pressing Save turned it into a nightly one, with
  // nothing on screen saying the cadence had changed. `troubleshoot/scheduleForm.ts` refuses the
  // same case for the same reason; this is its twin catching up.
  const [frequency, setFrequency] = useState<Cadence>(schedule?.frequency ?? 'daily');
  const [dayOfWeek, setDayOfWeek] = useState<number>(schedule?.day_of_week ?? 1);
  const [dayOfMonth, setDayOfMonth] = useState<number>(schedule?.day_of_month ?? 1);
  const [time, setTime] = useState<string>(
    timeValue(schedule?.at_hour ?? 9, schedule?.at_minute ?? 0),
  );
  const [enabled, setEnabled] = useState(schedule?.enabled ?? true);
  const [error, setError] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);

  async function save() {
    if (!definitionId) {
      setError(t('schedule.err.chooseReport'));
      return;
    }
    if (frequency === 'unknown') {
      setError(t('schedule.err.chooseFrequency'));
      return;
    }
    const [h, m] = time.split(':');
    const at_hour = Number(h);
    const at_minute = Number(m);
    if (!Number.isFinite(at_hour) || !Number.isFinite(at_minute)) {
      setError(t('schedule.err.invalidTime'));
      return;
    }
    setSaving(true);
    setError(null);
    const body = {
      definition_id: definitionId,
      frequency,
      day_of_week: frequency === 'weekly' ? dayOfWeek : null,
      day_of_month: frequency === 'monthly' ? dayOfMonth : null,
      at_hour,
      at_minute,
      enabled,
    };
    try {
      if (schedule) await api.updateReportSchedule(schedule.id, body);
      else await api.createReportSchedule(body);
      onSaved();
    } catch (e) {
      setError(e instanceof ApiError ? e.message : t('schedule.err.saveFailed'));
      setSaving(false);
    }
  }

  return (
    <Modal
      title={schedule ? t('schedule.editTitle') : t('schedule.newTitle')}
      onClose={onClose}
      footer={
        <>
          <Button onClick={onClose} disabled={saving}>
            {t('common:actions.cancel')}
          </Button>
          <Button variant="primary" disabled={saving} onClick={save}>
            {saving ? t('schedule.saving') : t('schedule.save')}
          </Button>
        </>
      }
    >
      <div className="rb">
        <label className="rb-field">
          <span>
            {t('schedule.report')} <RequiredMark />
          </span>
          <Select value={definitionId} onChange={(e) => setDefinitionId(e.target.value)}>
            {definitions.map((d) => (
              <option key={d.id} value={d.id}>
                {d.name}
              </option>
            ))}
          </Select>
        </label>

        <label className="rb-field">
          <span>{t('schedule.frequency')}</span>
          <Select
            value={frequency}
            onChange={(e) => setFrequency(e.target.value as Cadence)}
          >
            {/* Iterated from the deliberate subset, so a cadence added to the backend either
                appears here or is consciously excluded — never silently missing. */}
            {frequency === 'unknown' && (
              <option value="unknown" disabled>
                {t('schedule.freqChoose')}
              </option>
            )}
            {SELECTABLE_CADENCES.map((f) => (
              <option key={f} value={f}>
                {t(`schedule.freq.${f}`)}
              </option>
            ))}
          </Select>
        </label>

        {frequency === 'weekly' && (
          <label className="rb-field">
            <span>{t('schedule.dayOfWeek')}</span>
            <Select value={String(dayOfWeek)} onChange={(e) => setDayOfWeek(Number(e.target.value))}>
              {WEEKDAY_OPTIONS.map((o) => (
                <option key={o.value} value={o.value}>
                  {t(o.labelKey)}
                </option>
              ))}
            </Select>
          </label>
        )}

        {frequency === 'monthly' && (
          <label className="rb-field">
            <span>{t('schedule.dayOfMonth')}</span>
            <Select
              value={String(dayOfMonth)}
              onChange={(e) => setDayOfMonth(Number(e.target.value))}
            >
              {Array.from({ length: 28 }, (_, i) => i + 1).map((d) => (
                <option key={d} value={d}>
                  {d}
                </option>
              ))}
            </Select>
          </label>
        )}

        <label className="rb-field">
          <span>{t('schedule.timeUtc')}</span>
          <input
            className="field"
            type="time"
            value={time}
            onChange={(e) => setTime(e.target.value)}
          />
        </label>

        <label className="rb-check">
          <input type="checkbox" checked={enabled} onChange={(e) => setEnabled(e.target.checked)} />
          <span>{t('schedule.enabled')}</span>
        </label>

        {error && <FieldHint error>{error}</FieldHint>}
      </div>
    </Modal>
  );
}
