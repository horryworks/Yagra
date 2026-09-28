// SPDX-License-Identifier: AGPL-3.0-only
// Scheduled analyses — create/edit a recurring Troubleshoot analysis.
//
// The same two halves as the report schedule editor: what to run, and when. The "what" half is the
// launch drawer's fields (tool / scope / window / sensitivity / notify) and the "when" half is the
// shared preset cadence — times are UTC, because that is what the backend computes `next_run_at` in.
//
// Every decision lives in `scheduleForm.ts` so it can be tested; this file is layout.

import { useState } from 'react';
import { useTranslation } from 'react-i18next';
import { Modal } from '../components/ui/Modal';
import { Select, FieldHint } from '../components/ui/Field';
import { api } from '../services/api';
import { done } from '../lib/submitState';
import { useSubmit } from '../lib/useSubmit';
import { FormError, FormFooter } from '../components/ui/FormFooter';
import type { AnalysisSchedule, AnalysisToolKey, Cadence } from '../types/api';
import { SELECTABLE_CADENCES, WEEKDAY_OPTIONS } from '../lib/cadence';
import { ScopePicker } from '../components/ScopePicker/ScopePicker';
import { allScope, type ScopeValue } from '../components/ScopePicker/scope';
import { ANALYSIS_WINDOWS, analysisWindowLabelKey } from './analysisDefaults';
import {
  blankSchedule,
  formFromSchedule,
  scheduleBody,
  scheduleFormError,
  schedulableTools,
  timeLabel,
  type ScheduleForm,
} from './scheduleForm';

interface Props {
  /** The schedule to edit, or null to create one. */
  schedule: AnalysisSchedule | null;
  /** Whether this deployment has a flow store — decides which analyses may be scheduled. */
  flowEnabled: boolean;
  onClose: () => void;
  onSaved: () => void;
}

export function ScheduleModal({ schedule, flowEnabled, onClose, onSaved }: Props) {
  const { t } = useTranslation('troubleshoot');
  // The scope's label is localized, so it is built here rather than read from the stored row —
  // a saved `scope_label` is a snapshot in whatever language the operator used that day.
  const [form, setForm] = useState<ScheduleForm>(() =>
    schedule ? formFromSchedule(schedule, allScope(t)) : blankSchedule(allScope(t)),
  );
  const save = useSubmit({ errorFallback: t('schedule.err.saveFailed'), onDone: onSaved });

  const set = <K extends keyof ScheduleForm>(k: K, v: ScheduleForm[K]) =>
    setForm((f) => ({ ...f, [k]: v }));

  const tools = schedulableTools(flowEnabled);
  const SENS_LABELS = [
    t('launch.sens.veryLoose'),
    t('launch.sens.loose'),
    t('launch.sens.balanced'),
    t('launch.sens.strict'),
    t('launch.sens.veryStrict'),
  ];

  function submit() {
    const problem = scheduleFormError(form);
    if (problem) {
      save.refuse(t(`schedule.err.${problem}`));
      return;
    }
    const windowLabel = t(analysisWindowLabelKey(form.windowSecs));
    const body = scheduleBody(form, windowLabel);
    save.submit(async () => {
      if (schedule) await api.updateAnalysisSchedule(schedule.id, body);
      else await api.createAnalysisSchedule(body);
      return done();
    });
  }

  return (
    <Modal
      title={schedule ? t('schedule.editTitle') : t('schedule.newTitle')}
      onClose={onClose}
      footer={
        <FormFooter
          form={save}
          onClose={onClose}
          onSubmit={submit}
          submitLabel={t('schedule.save')}
          busyLabel={t('schedule.saving')}
        />
      }
    >
      <div className="rb">
        <label className="rb-field">
          <span>{t('schedule.analysis')}</span>
          <Select
            value={form.tool}
            onChange={(e) => set('tool', e.target.value as AnalysisToolKey)}
          >
            {tools.map((tool) => (
              <option key={tool.id} value={tool.id}>
                {t(tool.name)}
              </option>
            ))}
          </Select>
          {!flowEnabled && <FieldHint>{t('schedule.flowHidden')}</FieldHint>}
        </label>

        <div className="rb-field">
          <span>{t('fields.scope')}</span>
          <ScopePicker
            value={form.scope}
            onChange={(v: ScopeValue) => set('scope', v)}
            className="ts-field-full"
          />
        </div>

        <label className="rb-field">
          <span>{t('fields.timeWindow')}</span>
          <Select
            value={String(form.windowSecs)}
            onChange={(e) => set('windowSecs', Number(e.target.value))}
          >
            {ANALYSIS_WINDOWS.map((w) => (
              <option key={w.secs} value={w.secs}>
                {t(w.labelKey)}
              </option>
            ))}
          </Select>
        </label>

        <label className="rb-field">
          <span>{t('fields.sensitivity')}</span>
          <div className="ts-slider-row">
            <input
              className="ts-slider"
              type="range"
              min={1}
              max={5}
              value={form.sensitivity}
              onChange={(e) => set('sensitivity', Number(e.target.value))}
            />
            <span className="ts-slider-val">{SENS_LABELS[form.sensitivity - 1]}</span>
          </div>
        </label>

        <label className="rb-field">
          <span>{t('schedule.frequency')}</span>
          <Select
            value={form.frequency}
            onChange={(e) => set('frequency', e.target.value as Cadence)}
          >
            {/* Iterated from the deliberate subset, so a cadence added to the backend either
                appears here or is consciously excluded — never silently missing. */}
            {SELECTABLE_CADENCES.map((f) => (
              <option key={f} value={f}>
                {t(`reports:schedule.freq.${f}`)}
              </option>
            ))}
          </Select>
        </label>

        {form.frequency === 'weekly' && (
          <label className="rb-field">
            <span>{t('reports:schedule.dayOfWeek')}</span>
            <Select
              value={String(form.dayOfWeek)}
              onChange={(e) => set('dayOfWeek', Number(e.target.value))}
            >
              {WEEKDAY_OPTIONS.map((o) => (
                <option key={o.value} value={o.value}>
                  {t(o.labelKey)}
                </option>
              ))}
            </Select>
          </label>
        )}

        {form.frequency === 'monthly' && (
          <label className="rb-field">
            <span>{t('reports:schedule.dayOfMonth')}</span>
            <Select
              value={String(form.dayOfMonth)}
              onChange={(e) => set('dayOfMonth', Number(e.target.value))}
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
          <span>{t('reports:schedule.timeUtc')}</span>
          <input
            className="field"
            type="time"
            value={timeLabel(form.atHour, form.atMinute)}
            onChange={(e) => {
              const [h, m] = e.target.value.split(':').map(Number);
              if (Number.isFinite(h) && Number.isFinite(m)) {
                setForm((f) => ({ ...f, atHour: h, atMinute: m }));
              }
            }}
          />
        </label>

        <label className="rb-check">
          <input
            type="checkbox"
            checked={form.notify}
            onChange={(e) => set('notify', e.target.checked)}
          />
          <span>{t('schedule.notify')}</span>
        </label>

        <label className="rb-check">
          <input
            type="checkbox"
            checked={form.enabled}
            onChange={(e) => set('enabled', e.target.checked)}
          />
          <span>{t('reports:schedule.enabled')}</span>
        </label>

        <FormError form={save} />
      </div>
    </Modal>
  );
}
