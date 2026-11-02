// SPDX-License-Identifier: AGPL-3.0-only
// Switching on nodes that were never backed up (ADR-224 decision 6): the one thing to choose is how
// they are reached. Nodes with settings keep their own; nodes with no model are left out and named.

import { useState } from 'react';
import { useTranslation } from 'react-i18next';
import { Modal } from '../components/ui/Modal';
import { FormError, FormFooter } from '../components/ui/FormFooter';
import { Segmented } from '../components/ui/Segmented';
import { api } from '../services/api';
import { done } from '../lib/submitState';
import { useSubmit } from '../lib/useSubmit';
import { BACKUP_INPUTS, type BackupInput } from '../types/api';

interface Props {
  ids: string[];
  /** The names of the selected nodes that are left out: no model is known for them. */
  skipped: string[];
  onClose: () => void;
  onSaved: () => void;
}

export function ConfigBackupSwitchOnModal({ ids, skipped, onClose, onSaved }: Props) {
  const { t } = useTranslation('nodes');
  const [input, setInput] = useState<BackupInput>('ssh');
  const form = useSubmit({ errorFallback: t('config.err.save'), onDone: onSaved });

  function save() {
    form.submit(async () => {
      await api.switchConfigBackups({ node_ids: ids, enabled: true, input });
      return done();
    });
  }

  return (
    <Modal
      title={t('config.list.switchOn.title', { count: ids.length })}
      onClose={onClose}
      footer={
        <FormFooter
          form={form}
          onClose={onClose}
          onSubmit={save}
          submitLabel={t('config.list.switchOn.submit', { count: ids.length })}
        />
      }
    >
      <div className="modal-field">
        <span className="modal-field-label">{t('config.connection')}</span>
        <Segmented
          ariaLabel={t('config.connection')}
          options={BACKUP_INPUTS.map((i) => ({ value: i, label: t(`config.input.${i}`) }))}
          value={input}
          onChange={(next) => setInput(next as BackupInput)}
        />
      </div>
      {skipped.length > 0 && (
        <p className="form-warning">
          {t('config.list.switchOn.skipped', { count: skipped.length, names: skipped.join(', ') })}
        </p>
      )}
      <FormError form={form} />
    </Modal>
  );
}
