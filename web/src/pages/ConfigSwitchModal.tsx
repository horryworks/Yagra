// SPDX-License-Identifier: AGPL-3.0-only
// One deployment-wide on/off switch of the config backup (Config backups page): automatic save
// (ADR-222 decisions 8 and 13) and configuration text for AI clients (ADR-210 stage 10 decision
// 84). The two dialogs were the same form differing in their words and their request, so they are
// one component; the danger of switching on is said in the dialog, at the moment it is chosen.

import { useState } from 'react';
import { useTranslation } from 'react-i18next';
import { Modal } from '../components/ui/Modal';
import { FormError, FormFooter } from '../components/ui/FormFooter';
import { FieldGroup, Fields } from '../components/ui/FormLayout';
import { Callout } from '../components/ui/Section';
import { Segmented } from '../components/ui/Segmented';
import { done } from '../lib/submitState';
import { useSubmit } from '../lib/useSubmit';

/** The `nodes` key group holding the switch's words: `title`, `on`, `off`, `warning`, `err.save`. */
export type ConfigSwitchKeys = 'config.autoSave' | 'config.mcpText';

interface Props {
  keys: ConfigSwitchKeys;
  enabled: boolean;
  save: (enabled: boolean) => Promise<unknown>;
  onClose: () => void;
  onSaved: () => void;
}

export function ConfigSwitchModal({ keys, enabled: initial, save, onClose, onSaved }: Props) {
  const { t } = useTranslation('nodes');
  const [enabled, setEnabled] = useState(initial);
  const form = useSubmit({ errorFallback: t(`${keys}.err.save`), onDone: onSaved });

  function submit() {
    form.submit(async () => {
      await save(enabled);
      return done();
    });
  }

  return (
    <Modal
      title={t(`${keys}.title`)}
      onClose={onClose}
      footer={
        <FormFooter
          form={form}
          onClose={onClose}
          onSubmit={submit}
          submitLabel={t('common:actions.save')}
        />
      }
    >
      <Fields>
        <FieldGroup label={t(`${keys}.title`)}>
          <Segmented
            ariaLabel={t(`${keys}.title`)}
            options={[
              { value: 'on', label: t(`${keys}.on`) },
              { value: 'off', label: t(`${keys}.off`) },
            ]}
            value={enabled ? 'on' : 'off'}
            onChange={(next) => setEnabled(next === 'on')}
          />
        </FieldGroup>
        {enabled && <Callout tone="warning">{t(`${keys}.warning`)}</Callout>}
        <FormError form={form} />
      </Fields>
    </Modal>
  );
}
