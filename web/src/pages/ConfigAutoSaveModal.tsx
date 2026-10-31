// SPDX-License-Identifier: AGPL-3.0-only
// Automatic save (ADR-222 decisions 8 and 13): on or off. When on, Yagra saves a device right
// after a fetch that finds it unsaved, unless its last save failed — so the dialog holds no time of
// its own. Admin only (`save_device_config`).

import { useState } from 'react';
import { useTranslation } from 'react-i18next';
import { Modal } from '../components/ui/Modal';
import { FormError, FormFooter } from '../components/ui/FormFooter';
import { Segmented } from '../components/ui/Segmented';
import { api } from '../services/api';
import { done } from '../lib/submitState';
import { useSubmit } from '../lib/useSubmit';
import type { ConfigBackupScheduleView } from '../types/api';

interface Props {
  schedule: ConfigBackupScheduleView;
  onClose: () => void;
  onSaved: () => void;
}

export function ConfigAutoSaveModal({ schedule, onClose, onSaved }: Props) {
  const { t } = useTranslation('nodes');
  const [enabled, setEnabled] = useState(schedule.auto_save.enabled);
  const form = useSubmit({ errorFallback: t('config.autoSave.err.save'), onDone: onSaved });

  function save() {
    form.submit(async () => {
      await api.putConfigAutoSave({ enabled });
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
      {enabled && <p className="form-warning">{t('config.autoSave.warning')}</p>}
      <FormError form={form} />
    </Modal>
  );
}
