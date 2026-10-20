// SPDX-License-Identifier: AGPL-3.0-only
// Rename the selected nodes to their device's name, after showing what would change (ADR-208
// Inc.2). The judgement is in `nameFromDevice.ts`; this is the wiring.
import { useEffect, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { api, errMsg } from '../../services/api';
import type { NameFromDevicePreview, NameFromDeviceResult } from '../../types/api';
import { done } from '../../lib/submitState';
import { useSubmit } from '../../lib/useSubmit';
import { Modal } from '../ui/Modal';
import { FormError, FormFooter } from '../ui/FormFooter';
import { applyBody, shownRows, unchangedCounts } from './nameFromDevice';
import './NameFromDeviceModal.css';

export function NameFromDeviceModal({
  nodeIds,
  onClose,
  onApplied,
}: {
  nodeIds: string[];
  onClose: () => void;
  /** Something was written: re-read the tree. */
  onApplied: (result: NameFromDeviceResult) => void;
}) {
  const { t } = useTranslation('nodes');
  const [preview, setPreview] = useState<NameFromDevicePreview | null>(null);
  const [loadError, setLoadError] = useState<string | null>(null);

  useEffect(() => {
    let alive = true;
    api.previewNameFromDevice(nodeIds).then(
      (p) => alive && setPreview(p),
      (e: unknown) => alive && setLoadError(errMsg(e, t('nameFromDevice.err.preview'))),
    );
    return () => {
      alive = false;
    };
    // The selection is fixed for the dialog's life.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const body = preview ? applyBody(preview) : null;
  const form = useSubmit<NameFromDeviceResult>({
    errorFallback: t('nameFromDevice.err.apply'),
    onDone: (r) => onApplied(r),
  });
  const apply = () => {
    if (!body) return;
    form.submit(() => api.applyNameFromDevice(body).then((r) => done(r)));
  };

  const count = preview?.changes.length ?? 0;
  const shown = preview ? shownRows(preview) : null;
  const unchanged = preview ? unchangedCounts(preview) : null;

  return (
    <Modal
      title={
        preview
          ? t('nameFromDevice.title', { count, requested: preview.requested })
          : t('nameFromDevice.titleLoading')
      }
      onClose={onClose}
      footer={
        <FormFooter
          form={form}
          onClose={onClose}
          onSubmit={apply}
          submitLabel={t('nameFromDevice.apply', { count })}
          canSubmit={body !== null}
        />
      }
    >
      <div className="form-stack">
        {loadError && (
          <p className="form-error" role="alert">
            {loadError}
          </p>
        )}
        {shown && shown.rows.length > 0 && (
          <div className="name-from-device-grid" role="table">
            {shown.rows.map((r) => (
              <div key={r.node_id} className="name-from-device-row" role="row">
                <span role="cell" className="name-from-device-old" title={r.name}>
                  {r.name}
                </span>
                <span role="cell" aria-hidden="true">
                  →
                </span>
                <span role="cell" className="mono" title={r.sys_name}>
                  {r.sys_name}
                </span>
              </div>
            ))}
            {shown.more > 0 && (
              <div className="name-from-device-row muted" role="row">
                <span role="cell">{t('nameFromDevice.more', { count: shown.more })}</span>
              </div>
            )}
          </div>
        )}
        {unchanged && unchanged.total > 0 && (
          <p className="form-status">
            {t('nameFromDevice.unchanged', {
              count: unchanged.total,
              noDeviceName: unchanged.noDeviceName,
              alreadySame: unchanged.alreadySame,
              other: unchanged.other,
            })}
          </p>
        )}
        <FormError form={form} />
      </div>
    </Modal>
  );
}
