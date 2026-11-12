// SPDX-License-Identifier: AGPL-3.0-only
// Switching on nodes that were never backed up (ADR-224 decision 6): whether they stay off telnet -
// otherwise Yagra tries SSH and moves to telnet when SSH does not connect (ADR-242) - and which CLI
// logins they try, in order (ADR-240 Inc.2 decision 12). Nothing is ticked at first, so starting
// tries only the logins a person picked. Nodes with settings keep their own; nodes with no model are
// left out and named.

import { useMemo, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { Modal } from '../components/ui/Modal';
import { FormError, FormFooter } from '../components/ui/FormFooter';
import { CheckField, FieldGroup, Fields } from '../components/ui/FormLayout';
import { Callout } from '../components/ui/Section';
import { Button } from '../components/ui/Button';
import { CredentialPicker } from '../components/ui/CredentialPicker';
import { api } from '../services/api';
import { done } from '../lib/submitState';
import { useSubmit } from '../lib/useSubmit';
import { useLoad } from '../lib/useLoad';
import { isCliCredentialKind } from '../lib/credentialKinds';

interface Props {
  ids: string[];
  /** The names of the selected nodes that are left out: no model is known for them. */
  skipped: string[];
  /** How many of `ids` have settings of their own and keep their own logins. */
  keeping: number;
  onClose: () => void;
  onSaved: () => void;
}

export function ConfigBackupSwitchOnModal({ ids, skipped, keeping, onClose, onSaved }: Props) {
  const { t } = useTranslation('nodes');
  const [sshOnly, setSshOnly] = useState(false);
  // `null` is every CLI login in the try order, sent as such so a login added later is tried too.
  const [order, setOrder] = useState<string[] | null>([]);
  const form = useSubmit({ errorFallback: t('config.err.save'), onDone: onSaved });
  const creds = useLoad(() => api.listCredentials(), [], { initial: [] });
  const logins = useMemo(() => creds.data.filter((c) => isCliCredentialKind(c.kind)), [creds.data]);
  const picked = order ?? logins.map((c) => c.id);

  function save() {
    form.submit(async () => {
      await api.switchConfigBackups({
        node_ids: ids,
        enabled: true,
        input: sshOnly ? 'ssh' : 'ssh_then_telnet',
        login_order: order,
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
          canSubmit={picked.length > 0}
          submitLabel={t('config.list.switchOn.submit', { count: ids.length })}
        />
      }
    >
      <Fields>
        <CheckField checked={sshOnly} onChange={setSshOnly} label={t('config.input.ssh')} />
        <FieldGroup
          label={t('config.logins')}
          action={order !== null && <Button onClick={() => setOrder(null)}>{t('config.selectAll')}</Button>}
        >
          <CredentialPicker
            options={logins}
            selected={picked}
            onChange={setOrder}
            disabled={form.busy}
            emptyText={t('config.noLogins')}
          />
        </FieldGroup>
        {keeping > 0 && <p className="form-status">{t('config.list.switchOn.keeping', { count: keeping })}</p>}
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
