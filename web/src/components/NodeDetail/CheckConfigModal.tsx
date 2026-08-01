// SPDX-License-Identifier: AGPL-3.0-only
// Edit an existing URL / DNS monitor's configuration (ui-conventions §Modals case 3: focused
// editing). The add-node dialog creates these; before this dialog existed there was no way to
// change one afterwards short of deleting the node and making it again.
//
// The form's judgement — defaults, parsing, what blank means, the `expected_status` union — lives
// in `checkConfigForm.ts` where a test can reach it. This file is layout plus the call.

import { useState } from 'react';
import { useTranslation } from 'react-i18next';
import { api, errMsg } from '../../services/api';
import { Button } from '../ui/Button';
import { Modal } from '../ui/Modal';
import { FieldHint, RequiredMark, Select, TextInput } from '../ui/Field';
import type { DnsCheckConfig, UrlCheckConfig } from '../../types/api';
import {
  dnsBodyFrom,
  dnsDraftFrom,
  urlBodyFrom,
  urlDraftFrom,
  EXPECTED_STATUS_MODES,
  type DnsCheckDraft,
  type UrlCheckDraft,
} from './checkConfigForm';

const HTTP_METHODS = ['GET', 'HEAD', 'POST'] as const;
const DNS_RECORD_TYPES = ['A', 'AAAA', 'CNAME'] as const;

/** One labelled row. Local to this dialog because the shared `Field` exports the controls, not the
 *  label+hint wrapper, and every other modal spells this out the same way. */
function Row({
  label,
  required,
  hint,
  children,
}: {
  label: string;
  required?: boolean;
  hint?: string;
  children: React.ReactNode;
}) {
  return (
    <label className="modal-field">
      <span className="modal-field-label">
        {label} {required && <RequiredMark />}
      </span>
      {children}
      {hint && <FieldHint>{hint}</FieldHint>}
    </label>
  );
}

export function UrlCheckModal({
  nodeId,
  config,
  onClose,
  onSaved,
}: {
  nodeId: string;
  config: UrlCheckConfig;
  onClose: () => void;
  onSaved: () => void;
}) {
  const { t } = useTranslation('nodes');
  const [d, setD] = useState<UrlCheckDraft>(() => urlDraftFrom(config));
  const [error, setError] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);
  const set = <K extends keyof UrlCheckDraft>(k: K, v: UrlCheckDraft[K]) =>
    setD((prev) => ({ ...prev, [k]: v }));

  const save = () => {
    const built = urlBodyFrom(d);
    if ('error' in built) {
      setError(t(`checkEdit.err.${built.error}`));
      return;
    }
    setSaving(true);
    setError(null);
    api
      .setUrlCheck(nodeId, built.body)
      .then(onSaved)
      .catch((e: unknown) => {
        setError(errMsg(e, t('checkEdit.err.save')));
        setSaving(false);
      });
  };

  return (
    <Modal
      title={t('checkEdit.urlTitle')}
      onClose={onClose}
      footer={
        <>
          <Button onClick={onClose} disabled={saving}>
            {t('common:actions.cancel')}
          </Button>
          <Button variant="primary" onClick={save} disabled={saving}>
            {saving ? t('checkEdit.saving') : t('common:actions.save')}
          </Button>
        </>
      }
    >
      <Row label={t('checkEdit.url')} required>
        <TextInput value={d.url} onChange={(e) => set('url', e.target.value)} />
      </Row>
      <Row label={t('checkEdit.method')}>
        <Select
          value={d.method}
          onChange={(e) => set('method', e.target.value as UrlCheckDraft['method'])}
        >
          {HTTP_METHODS.map((m) => (
            <option key={m} value={m}>
              {m}
            </option>
          ))}
        </Select>
      </Row>
      <Row label={t('checkEdit.expectedStatus')}>
        <Select
          value={d.statusMode}
          onChange={(e) => set('statusMode', e.target.value as UrlCheckDraft['statusMode'])}
        >
          {EXPECTED_STATUS_MODES.map((m) => (
            <option key={m} value={m}>
              {t(`checkEdit.statusMode.${m}`)}
            </option>
          ))}
        </Select>
      </Row>
      {d.statusMode === 'exact' && (
        <Row label={t('checkEdit.statusCodes')} hint={t('checkEdit.statusCodesHint')}>
          <TextInput
            value={d.statusCodes}
            placeholder="200, 204"
            onChange={(e) => set('statusCodes', e.target.value)}
          />
        </Row>
      )}
      {d.statusMode === 'range' && (
        <div className="modal-field-row">
          <Row label={t('checkEdit.statusLo')}>
            <TextInput
              value={d.statusLo}
              inputMode="numeric"
              placeholder="200"
              onChange={(e) => set('statusLo', e.target.value)}
            />
          </Row>
          <Row label={t('checkEdit.statusHi')}>
            <TextInput
              value={d.statusHi}
              inputMode="numeric"
              placeholder="299"
              onChange={(e) => set('statusHi', e.target.value)}
            />
          </Row>
        </div>
      )}
      <Row label={t('checkEdit.timeoutMs')}>
        <TextInput
          value={d.timeoutMs}
          inputMode="numeric"
          onChange={(e) => set('timeoutMs', e.target.value)}
        />
      </Row>
      <label className="nd-check-toggle">
        <input
          type="checkbox"
          checked={d.verifyTls}
          onChange={(e) => set('verifyTls', e.target.checked)}
        />
        <span>{t('checkEdit.verifyTls')}</span>
      </label>
      {!d.verifyTls && <FieldHint error>{t('checkEdit.verifyTlsWarning')}</FieldHint>}
      <label className="nd-check-toggle">
        <input
          type="checkbox"
          checked={d.followRedirects}
          onChange={(e) => set('followRedirects', e.target.checked)}
        />
        <span>{t('checkEdit.followRedirects')}</span>
      </label>
      {error && <p className="form-error">{error}</p>}
    </Modal>
  );
}

export function DnsCheckModal({
  nodeId,
  config,
  onClose,
  onSaved,
}: {
  nodeId: string;
  config: DnsCheckConfig;
  onClose: () => void;
  onSaved: () => void;
}) {
  const { t } = useTranslation('nodes');
  const [d, setD] = useState<DnsCheckDraft>(() => dnsDraftFrom(config));
  const [error, setError] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);
  const set = <K extends keyof DnsCheckDraft>(k: K, v: DnsCheckDraft[K]) =>
    setD((prev) => ({ ...prev, [k]: v }));

  const save = () => {
    const built = dnsBodyFrom(d);
    if ('error' in built) {
      setError(t(`checkEdit.err.${built.error}`));
      return;
    }
    setSaving(true);
    setError(null);
    api
      .setDnsCheck(nodeId, built.body)
      .then(onSaved)
      .catch((e: unknown) => {
        setError(errMsg(e, t('checkEdit.err.save')));
        setSaving(false);
      });
  };

  return (
    <Modal
      title={t('checkEdit.dnsTitle')}
      onClose={onClose}
      footer={
        <>
          <Button onClick={onClose} disabled={saving}>
            {t('common:actions.cancel')}
          </Button>
          <Button variant="primary" onClick={save} disabled={saving}>
            {saving ? t('checkEdit.saving') : t('common:actions.save')}
          </Button>
        </>
      }
    >
      <Row label={t('checkEdit.dnsName')} required>
        <TextInput value={d.name} onChange={(e) => set('name', e.target.value)} />
      </Row>
      <Row label={t('checkEdit.recordType')}>
        <Select
          value={d.recordType}
          onChange={(e) => set('recordType', e.target.value as DnsCheckDraft['recordType'])}
        >
          {DNS_RECORD_TYPES.map((r) => (
            <option key={r} value={r}>
              {r}
            </option>
          ))}
        </Select>
      </Row>
      <div className="modal-field-row">
        <Row label={t('checkEdit.resolver')} hint={t('checkEdit.resolverHint')}>
          <TextInput
            value={d.resolver}
            placeholder={t('checkEdit.resolverPlaceholder')}
            onChange={(e) => set('resolver', e.target.value)}
          />
        </Row>
        <Row label={t('checkEdit.resolverPort')}>
          <TextInput
            value={d.resolverPort}
            inputMode="numeric"
            onChange={(e) => set('resolverPort', e.target.value)}
          />
        </Row>
      </div>
      <div className="modal-field-row">
        <Row label={t('checkEdit.maxDepth')} hint={t('checkEdit.maxDepthHint')}>
          <TextInput
            value={d.maxDepth}
            inputMode="numeric"
            onChange={(e) => set('maxDepth', e.target.value)}
          />
        </Row>
        <Row label={t('checkEdit.timeoutMs')}>
          <TextInput
            value={d.timeoutMs}
            inputMode="numeric"
            onChange={(e) => set('timeoutMs', e.target.value)}
          />
        </Row>
      </div>
      {error && <p className="form-error">{error}</p>}
    </Modal>
  );
}
