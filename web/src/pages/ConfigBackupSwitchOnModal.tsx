// SPDX-License-Identifier: AGPL-3.0-only
// Switching on nodes that were never backed up (ADR-224 decision 6): the one thing to choose is
// whether they stay off telnet - otherwise Yagra tries SSH and moves to telnet when SSH does not
// connect (ADR-242). Nodes with settings keep their own; nodes with no model are left out and named.

import { useState } from 'react';
import { useTranslation } from 'react-i18next';
import { Modal } from '../components/ui/Modal';
import { FormError, FormFooter } from '../components/ui/FormFooter';
import { CheckField, Fields } from '../components/ui/FormLayout';
import { Callout } from '../components/ui/Section';
import { api } from '../services/api';
import { done } from '../lib/submitState';
import { useSubmit } from '../lib/useSubmit';

interface Props {
  ids: string[];
  /** The names of the selected nodes that are left out: no model is known for them. */
  skipped: string[];
  onClose: () => void;
  onSaved: () => void;
}

export function ConfigBackupSwitchOnModal({ ids, skipped, onClose, onSaved }: Props) {
  const { t } = useTranslation('nodes');
  const [sshOnly, setSshOnly] = useState(false);
  const form = useSubmit({ errorFallback: t('config.err.save'), onDone: onSaved });

  function save() {
    form.submit(async () => {
      await api.switchConfigBackups({
        node_ids: ids,
        enabled: true,
        input: sshOnly ? 'ssh' : 'ssh_then_telnet',
      });
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
      <Fields>
        <CheckField checked={sshOnly} onChange={setSshOnly} label={t('config.input.ssh')} />
        {skipped.length > 0 && (
          <Callout tone="warning">
            {t('config.list.switchOn.skipped', { count: skipped.length, names: skipped.join(', ') })}
          </Callout>
        )}
        <FormError form={form} />
      </Fields>
    </Modal>
  );
}
