// SPDX-License-Identifier: AGPL-3.0-only
// "Add as another node?" — what Monitor asks when the device it is about to add looks like a node
// already monitored at another address (ADR-139 Inc.5 decision 7). Drawn by `EndpointSetupCell`,
// so Discovery ▸ Unregistered devices and Node ▸ Neighbors ask it in the same words.
//
// The server still refuses nothing (Inc.3 ⑴): the evidence can be wrong where sites reuse one
// address plan, and this is where the operator says it is. Which sentence and which reasons are
// decided in `pages/discoveryExisting.ts`, where a test reaches them.

import { useTranslation } from 'react-i18next';
import type { SameDeviceMatch } from '../../types/api';
import { sameDeviceConfirmKey, sameDeviceReasonKeys } from '../../pages/discoveryExisting';
import { done } from '../../lib/submitState';
import { useSubmit } from '../../lib/useSubmit';
import { FormError, FormFooter } from '../ui/FormFooter';
import { Modal } from '../ui/Modal';

interface Props {
  match: SameDeviceMatch;
  /** Starts monitoring. The cell it was pressed in shows its progress and its error. */
  onConfirm: () => void;
  onClose: () => void;
}

export function SameDeviceConfirm({ match, onConfirm, onClose }: Props) {
  const { t, i18n } = useTranslation('monitoring');
  const form = useSubmit({ errorFallback: t('discovery.seen.err.import'), onDone: onClose });
  const nodes = new Intl.ListFormat(i18n.language, { type: 'conjunction' }).format(
    match.nodes.map((n) => t('discovery.sameDevice.confirm.node', { name: n.name, address: n.address })),
  );
  const reasons = sameDeviceReasonKeys(match)
    .map((k) => t(k))
    .join(' · ');
  return (
    <Modal
      title={t('discovery.sameDevice.confirm.title')}
      onClose={onClose}
      footer={
        <FormFooter
          form={form}
          onClose={onClose}
          onSubmit={() =>
            form.submit(() => {
              onConfirm();
              return Promise.resolve(done());
            })
          }
          submitLabel={t('discovery.sameDevice.confirm.add')}
        />
      }
    >
      <p className="modal-confirm-text">{t(sameDeviceConfirmKey(match), { nodes })}</p>
      {reasons && <p className="muted">{reasons}</p>}
      <FormError form={form} />
    </Modal>
  );
}
