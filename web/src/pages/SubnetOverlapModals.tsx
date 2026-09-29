// SPDX-License-Identifier: AGPL-3.0-only
// The three dialogs Nodes ▸ Subnet overlaps opens (ADR-187): the exclusion rules, adding one, and
// marking one overlap as intentional. Each owns its own fields; closing it is the reset.

import { useState } from 'react';
import { useTranslation } from 'react-i18next';
import { api, errMsg } from '../services/api';
import {
  EXCLUSION_REASONS,
  type ExclusionReason,
  type OverlapRule,
  type OverlapRuleBody,
  type SubnetOverlap,
} from '../types/api';
import { done } from '../lib/submitState';
import { useSubmit } from '../lib/useSubmit';
import { Modal } from '../components/ui/Modal';
import { Button } from '../components/ui/Button';
import { FieldHint, Select, TextInput } from '../components/ui/Field';
import { FormError, FormFooter } from '../components/ui/FormFooter';
import { ConfirmDeleteModal } from '../components/ui/ConfirmDeleteModal';
import { draftFrom, ruleBody, toggled, type RuleDraft } from './subnetOverlaps';

/** Every exclusion rule, with what each one excludes now. Read-only without `manage_config`. */
export function OverlapRulesModal({
  rules,
  canConfig,
  describe,
  onAdd,
  onChanged,
  onClose,
}: {
  rules: OverlapRule[];
  canConfig: boolean;
  describe: (r: OverlapRule) => string;
  onAdd: () => void;
  onChanged: () => void;
  onClose: () => void;
}) {
  const { t } = useTranslation('monitoring');
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState<string | null>(null);
  const [deleting, setDeleting] = useState<OverlapRule | null>(null);

  const flip = (r: OverlapRule) => {
    setError(null);
    setBusy(r.id);
    api
      .updateOverlapRule(r.id, toggled(r))
      .then(() => onChanged())
      .catch((e: unknown) => setError(errMsg(e, t('subnetOverlaps.rules.toggleErr'))))
      .finally(() => setBusy(null));
  };

  return (
    <Modal
      title={t('subnetOverlaps.rules.title')}
      onClose={onClose}
      size="wide"
      footer={
        <>
          {canConfig && (
            <Button variant="outline" onClick={onAdd}>
              {t('subnetOverlaps.rules.add')}
            </Button>
          )}
          <Button variant="primary" onClick={onClose}>
            {t('common:actions.close')}
          </Button>
        </>
      }
    >
      <div className="form-stack">
        <p className="muted">{t('subnetOverlaps.rules.text')}</p>
        <ul className="so-rules">
          {rules.map((r) => (
            <li className="so-rule" key={r.id}>
              <div className="so-rule-what">
                <span>{describe(r)}</span>
                <span className="so-rule-meta">
                  {t(`subnetOverlaps.rules.reason.${r.reason}`)}
                  {r.note && ` · ${r.note}`}
                  {r.builtin && ` · ${t('subnetOverlaps.rules.builtin')}`}
                </span>
              </div>
              <span className="so-rule-hits">
                {r.enabled
                  ? t('subnetOverlaps.rules.hits', { count: r.excluded_count })
                  : t('subnetOverlaps.rules.off')}
              </span>
              {canConfig && (
                <span className="so-rule-actions">
                  <Button variant="outline" onClick={() => flip(r)} disabled={busy === r.id}>
                    {r.enabled ? t('subnetOverlaps.rules.disable') : t('subnetOverlaps.rules.enable')}
                  </Button>
                  {!r.builtin && (
                    <Button variant="ghost" onClick={() => setDeleting(r)}>
                      {t('subnetOverlaps.rules.delete')}
                    </Button>
                  )}
                </span>
              )}
            </li>
          ))}
        </ul>
        {error && (
          <p className="form-error" role="alert">
            {error}
          </p>
        )}
      </div>
      {deleting && (
        <ConfirmDeleteModal
          title={t('subnetOverlaps.rules.deleteTitle')}
          onConfirm={() => api.deleteOverlapRule(deleting.id)}
          errorFallback={t('subnetOverlaps.rules.deleteErr')}
          onClose={() => setDeleting(null)}
          onDone={() => {
            setDeleting(null);
            onChanged();
          }}
        >
          <p>{t('subnetOverlaps.rules.deleteText', { rule: describe(deleting) })}</p>
        </ConfirmDeleteModal>
      )}
    </Modal>
  );
}

/** Add a rule — blank, or pre-filled from a hint. */
export function OverlapRuleModal({
  initial,
  onClose,
  onDone,
}: {
  initial: OverlapRuleBody | null;
  onClose: () => void;
  onDone: () => void;
}) {
  const { t } = useTranslation('monitoring');
  const [draft, setDraft] = useState<RuleDraft>(() => draftFrom(initial));
  const form = useSubmit({ errorFallback: t('subnetOverlaps.rules.form.err'), onDone });
  const set = (p: Partial<RuleDraft>) => setDraft((d) => ({ ...d, ...p }));

  const submit = () => {
    const r = ruleBody(draft);
    if ('refuse' in r) {
      form.refuse(t(`subnetOverlaps.${r.refuse}`));
      return;
    }
    form.submit(() => api.createOverlapRule(r.body).then(() => done()));
  };

  return (
    <Modal
      title={t('subnetOverlaps.rules.form.title')}
      onClose={onClose}
      footer={
        <FormFooter
          form={form}
          onClose={onClose}
          onSubmit={submit}
          submitLabel={t('subnetOverlaps.rules.form.save')}
        />
      }
    >
      <div className="form-stack">
        <label className="modal-field">
          <span className="modal-field-label">{t('subnetOverlaps.rules.form.portText')}</span>
          <TextInput value={draft.portText} onChange={(e) => set({ portText: e.target.value })} maxLength={200} />
          <FieldHint>{t('subnetOverlaps.rules.form.portHint')}</FieldHint>
        </label>
        <label className="modal-field">
          <span className="modal-field-label">{t('subnetOverlaps.rules.form.range')}</span>
          <TextInput
            className="mono"
            value={draft.range}
            placeholder="192.168.1.0/24"
            onChange={(e) => set({ range: e.target.value })}
          />
          <FieldHint>{t('subnetOverlaps.rules.form.rangeHint')}</FieldHint>
        </label>
        <label className="modal-field">
          <span className="modal-field-label">{t('subnetOverlaps.rules.form.reason')}</span>
          <Select
            value={draft.reason}
            onChange={(e) => set({ reason: e.target.value as ExclusionReason })}
          >
            {EXCLUSION_REASONS.map((r) => (
              <option key={r} value={r}>
                {t(`subnetOverlaps.rules.reason.${r}`)}
              </option>
            ))}
          </Select>
        </label>
        <label className="modal-field">
          <span className="modal-field-label">{t('subnetOverlaps.rules.form.note')}</span>
          <TextInput value={draft.note} onChange={(e) => set({ note: e.target.value })} maxLength={200} />
        </label>
        <FormError form={form} />
      </div>
    </Modal>
  );
}

/** Mark one overlap as intentional, for the sites it spans now. */
export function AckOverlapModal({
  overlap,
  onClose,
  onDone,
}: {
  overlap: SubnetOverlap;
  onClose: () => void;
  onDone: () => void;
}) {
  const { t } = useTranslation('monitoring');
  const [note, setNote] = useState('');
  const form = useSubmit({ errorFallback: t('subnetOverlaps.ack.err'), onDone });

  return (
    <Modal
      title={t('subnetOverlaps.ack.title', { subnet: overlap.subnet })}
      onClose={onClose}
      footer={
        <FormFooter
          form={form}
          onClose={onClose}
          onSubmit={() =>
            form.submit(() => api.ackOverlap(overlap.key, note.trim()).then(() => done()))
          }
          submitLabel={t('subnetOverlaps.ack.save')}
        />
      }
    >
      <div className="form-stack">
        <p className="muted">{t('subnetOverlaps.ack.text', { count: overlap.site_count })}</p>
        <label className="modal-field">
          <span className="modal-field-label">{t('subnetOverlaps.ack.note')}</span>
          <TextInput
            value={note}
            placeholder={t('subnetOverlaps.ack.notePlaceholder')}
            onChange={(e) => setNote(e.target.value)}
            maxLength={200}
          />
        </label>
        <FormError form={form} />
      </div>
    </Modal>
  );
}
