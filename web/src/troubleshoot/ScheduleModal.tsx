// SPDX-License-Identifier: AGPL-3.0-only
// Scheduled analyses — create/edit a recurring Troubleshoot analysis.
//
// The same two halves as the report schedule editor: what to run, and when. The "what" half is the
// launch drawer's fields (tool / scope / window / sensitivity / notify) and the "when" half is the
// shared preset cadence — the time is a wall time in the schedule's zone (ADR-221).
//
// Every decision lives in `scheduleForm.ts` so it can be tested; this file is layout.

import { useState } from 'react';
import { useTranslation } from 'react-i18next';
import { Modal } from '../components/ui/Modal';
import { Field, Select } from '../components/ui/Field';
import { CheckField, Fields } from '../components/ui/FormLayout';
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
import { zoneAbbreviation } from '../lib/timeZone';

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
      <Fields>
        <Field
          label={t('schedule.analysis')}
          status={!flowEnabled && t('schedule.flowHidden')}
        >
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
        </Field>

        <Field label={t('fields.scope')}>
          <ScopePicker
            value={form.scope}
            onChange={(v: ScopeValue) => set('scope', v)}
            className="ts-field-full"
          />
        </Field>

        <Field label={t('fields.timeWindow')}>
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
        </Field>

        <Field label={t('fields.sensitivity')} htmlFor="ts-schedule-sensitivity">
          <div className="ts-slider-row">
            <input
              id="ts-schedule-sensitivity"
              className="ts-slider"
              type="range"
              min={1}
              max={5}
              value={form.sensitivity}
              onChange={(e) => set('sensitivity', Number(e.target.value))}
            />
            <span className="ts-slider-val">{SENS_LABELS[form.sensitivity - 1]}</span>
          </div>
        </Field>

        <Field label={t('schedule.frequency')}>
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
        </Field>

        {form.frequency === 'weekly' && (
          <Field label={t('reports:schedule.dayOfWeek')}>
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
          </Field>
        )}

        {form.frequency === 'monthly' && (
          <Field label={t('reports:schedule.dayOfMonth')}>
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
          </Field>
        )}

        <Field label={t('reports:schedule.time', { zone: zoneAbbreviation(form.timeZone) })}>
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
        </Field>

        <CheckField
          checked={form.notify}
          onChange={(next) => set('notify', next)}
          label={t('schedule.notify')}
        />

        <CheckField
          checked={form.enabled}
          onChange={(next) => set('enabled', next)}
          label={t('reports:schedule.enabled')}
        />

        <FormError form={save} />
      </Fields>
    </Modal>
  );
}
