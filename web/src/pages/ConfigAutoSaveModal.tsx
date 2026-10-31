// SPDX-License-Identifier: AGPL-3.0-only
// Automatic save (ADR-222 decision 8): on or off, and how long a device stays unsaved before Yagra
// saves it after its next fetch. Admin only (`save_device_config`). What it may send is
// `configBackupSchedule.ts`.

import { useState } from 'react';
import { useTranslation } from 'react-i18next';
import { Modal } from '../components/ui/Modal';
import { Field, TextInput } from '../components/ui/Field';
import { FormError, FormFooter } from '../components/ui/FormFooter';
import { Segmented } from '../components/ui/Segmented';
import { api } from '../services/api';
import { done } from '../lib/submitState';
import { useSubmit } from '../lib/useSubmit';
import type { ConfigBackupScheduleView } from '../types/api';
import { autoSaveBody } from './configBackupSchedule';

interface Props {
  schedule: ConfigBackupScheduleView;
  onClose: () => void;
  onSaved: () => void;
}

export function ConfigAutoSaveModal({ schedule, onClose, onSaved }: Props) {
  const { t } = useTranslation('nodes');
  const [enabled, setEnabled] = useState(schedule.auto_save.enabled);
  const [hours, setHours] = useState(String(schedule.auto_save.after_hours));
  const form = useSubmit({ errorFallback: t('config.autoSave.err.save'), onDone: onSaved });

  function save() {
    const parsed = autoSaveBody(enabled, hours);
    if (!parsed.ok) {
      form.refuse(t(parsed.key));
      return;
    }
    form.submit(async () => {
      await api.putConfigAutoSave(parsed.body);
      return done();
    });
  }

  return (
    <Modal
      title={t('config.autoSave.title')}
      onClose={onClose}
      footer={
        <FormFooter
          form={form}
          onClose={onClose}
          onSubmit={save}
          submitLabel={t('common:actions.save')}
        />
      }
    >
      <div className="modal-field">
        <span className="modal-field-label">{t('config.autoSave.title')}</span>
        <Segmented
          ariaLabel={t('config.autoSave.title')}
          options={[
            { value: 'on', label: t('config.autoSave.on') },
            { value: 'off', label: t('config.autoSave.off') },
          ]}
          value={enabled ? 'on' : 'off'}
          onChange={(next) => setEnabled(next === 'on')}
        />
      </div>
      <Field label={t('config.autoSave.after')} htmlFor="cfgbl-autosave-hours">
        <TextInput
          id="cfgbl-autosave-hours"
          inputMode="numeric"
          value={hours}
          suffix={t('config.autoSave.hours')}
          onChange={(e) => setHours(e.target.value)}
        />
      </Field>
      {enabled && <p className="form-warning">{t('config.autoSave.warning')}</p>}
      <FormError form={form} />
    </Modal>
  );
}
