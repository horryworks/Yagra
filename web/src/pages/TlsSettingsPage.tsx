// SPDX-License-Identifier: AGPL-3.0-only
// TLS certificate (Settings ▸ TLS, ADR-044): what the WebUI is serving, and how to replace it.
// ManageConfig-gated. The private key is write-only — the API never returns it — while the
// certificate is downloadable, because that is what an operator needs in order to trust a
// self-signed deployment from Prometheus, curl, or an OS trust store.
//
// Every decision this page makes lives in `tlsSettingsForm.ts`, where Vitest can reach it. What is
// left here is layout.

import { useCallback, useEffect, useState } from 'react';
import { useTranslation } from 'react-i18next';

import { api, errMsg } from '../services/api';
import type { WebTlsStatus } from '../types/api';
import {
  certificateFilename,
  expiryLevel,
  importBlock,
  parseNames,
  type ImportBlock,
} from './tlsSettingsForm';
import { PageHeader } from '../components/ui/PageHeader';
import { Card } from '../components/ui/Card';
import { Button } from '../components/ui/Button';
import { Badge, type Tone } from '../components/ui/Badge';
import { TextArea, FieldHint } from '../components/ui/Field';
import './TlsSettingsPage.css';

const EXPIRY_TONE: Record<ReturnType<typeof expiryLevel>, Tone> = {
  ok: 'up',
  soon: 'warning',
  critical: 'critical',
  expired: 'critical',
};

export function TlsSettingsPage() {
  const { t } = useTranslation('settings-tls');
  const [status, setStatus] = useState<WebTlsStatus | null>(null);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [notice, setNotice] = useState<string | null>(null);
  const [saveError, setSaveError] = useState<string | null>(null);

  const [certificate, setCertificate] = useState('');
  const [privateKey, setPrivateKey] = useState('');
  const [names, setNames] = useState('');
  const [attempted, setAttempted] = useState(false);

  const load = useCallback(async () => {
    try {
      setStatus(await api.getWebTls());
      setLoadError(null);
    } catch (e) {
      setLoadError(errMsg(e, t('error.load')));
    }
  }, [t]);

  useEffect(() => {
    void load();
  }, [load]);

  const view = status?.config ?? null;
  const block: ImportBlock | null = importBlock(certificate, privateKey);

  async function onImport() {
    setAttempted(true);
    if (block) return;
    setBusy(true);
    setSaveError(null);
    setNotice(null);
    try {
      const next = await api.importWebTls(certificate, privateKey);
      setStatus(next);
      // Cleared on success only. Keeping it on failure means an operator who pasted a five-kilobyte
      // chain and hit one validation error does not have to find the file again.
      setCertificate('');
      setPrivateKey('');
      setAttempted(false);
      setNotice(t('import.success'));
    } catch (e) {
      setSaveError(errMsg(e, t('error.save')));
    } finally {
      setBusy(false);
    }
  }

  async function onRegenerate() {
    // An imported certificate is the operator's, and replacing it with a self-signed one puts the
    // browser warning back. That deserves a different sentence from replacing a self-signed one.
    const question =
      view?.source === 'self_signed' ? t('regenerate.confirm') : t('regenerate.confirmImported');
    if (!window.confirm(question)) return;
    setBusy(true);
    setSaveError(null);
    setNotice(null);
    try {
      setStatus(await api.regenerateWebTls(parseNames(names)));
      setNotice(t('regenerate.success'));
    } catch (e) {
      setSaveError(errMsg(e, t('error.save')));
    } finally {
      setBusy(false);
    }
  }

  function onDownload() {
    if (!view) return;
    const blob = new Blob([view.certificate], { type: 'application/x-pem-file' });
    const url = URL.createObjectURL(blob);
    const a = document.createElement('a');
    a.href = url;
    a.download = certificateFilename(view);
    a.click();
    URL.revokeObjectURL(url);
  }

  async function onPickFile(e: React.ChangeEvent<HTMLInputElement>, into: (s: string) => void) {
    const file = e.target.files?.[0];
    if (!file) return;
    // Read in the browser and drop the text into the textarea rather than uploading: the operator
    // sees exactly what will be sent, and a wrong file is obvious before a round trip.
    into(await file.text());
    e.target.value = '';
  }

  const level = view ? expiryLevel(view.expires_in_days) : 'ok';

  return (
    <div className="tlspage">
      <PageHeader title={t('title')} note={t('subtitle')} />

      {loadError && <div className="tls-alert tls-alert-error">{loadError}</div>}

      {status?.api_port_is_public && (
        <div className="tls-alert tls-alert-warn">
          <strong>{t('warning.apiPortPublicTitle')}</strong>
          <p>{t('warning.apiPortPublic')}</p>
        </div>
      )}

      <Card title={t('current.heading')}>
        {!status && !loadError && <p className="tls-muted">{t('loading')}</p>}
        {status && !view && (
          <>
            <p className="tls-muted">{t('none')}</p>
            <FieldHint>{t('noneHint')}</FieldHint>
          </>
        )}
        {view && (
          <>
            <div className="tls-headline">
              <Badge tone={view.source === 'imported' ? 'info' : 'neutral'}>
                {t(`source.${view.source}`)}
              </Badge>
              <Badge tone={EXPIRY_TONE[level]}>
                {t(`expiry.${level}`, { days: Math.abs(view.expires_in_days) })}
              </Badge>
            </div>
            <FieldHint>{t(`sourceHint.${view.source}`)}</FieldHint>

            {view.key_unreadable && (
              <div className="tls-alert tls-alert-error">{t('warning.keyUnreadable')}</div>
            )}
            {!view.materialized && !view.key_unreadable && (
              <div className="tls-alert tls-alert-warn">{t('warning.notMaterialized')}</div>
            )}

            <dl className="tls-facts">
              <dt>{t('current.subject')}</dt>
              <dd className="mono">{view.subject}</dd>
              <dt>{t('current.issuer')}</dt>
              <dd className="mono">{view.issuer}</dd>
              <dt>{t('current.sans')}</dt>
              <dd className="mono">{view.sans.join(', ') || '—'}</dd>
              <dt>{t('current.validity')}</dt>
              <dd>
                {t('current.validityRange', {
                  from: new Date(view.not_before).toLocaleString(),
                  to: new Date(view.not_after).toLocaleString(),
                })}
              </dd>
              <dt>{t('current.keyAlgorithm')}</dt>
              <dd>{view.key_algorithm}</dd>
              <dt>{t('current.fingerprint')}</dt>
              <dd className="mono tls-fingerprint">{view.fingerprint_sha256}</dd>
              <dt>{t('current.importedAt')}</dt>
              <dd>
                {new Date(view.imported_at).toLocaleString()}
                {view.imported_by ? ` (${t('current.importedBy', { user: view.imported_by })})` : ''}
              </dd>
            </dl>

            {view.source === 'self_signed' && <FieldHint>{t('expiry.renewNote')}</FieldHint>}

            <div className="tls-actions">
              <Button onClick={onDownload}>{t('current.download')}</Button>
            </div>
            <FieldHint>{t('current.downloadHint')}</FieldHint>
          </>
        )}
      </Card>

      <Card title={t('import.heading')}>
        <p className="tls-muted">{t('import.intro')}</p>

        <label className="tls-label" htmlFor="tls-cert">
          {t('import.certificate')}
        </label>
        <TextArea
          id="tls-cert"
          className="mono"
          rows={8}
          value={certificate}
          placeholder={t('import.certificatePlaceholder')}
          onChange={(e) => setCertificate(e.target.value)}
        />
        <div className="tls-row">
          <input
            type="file"
            accept=".crt,.pem,.cer,.cert,application/x-pem-file"
            onChange={(e) => void onPickFile(e, setCertificate)}
          />
          <FieldHint>{t('import.certificateHint')}</FieldHint>
        </div>

        <label className="tls-label" htmlFor="tls-key">
          {t('import.privateKey')}
        </label>
        <TextArea
          id="tls-key"
          className="mono"
          rows={6}
          value={privateKey}
          placeholder={t('import.privateKeyPlaceholder')}
          onChange={(e) => setPrivateKey(e.target.value)}
        />
        <div className="tls-row">
          <input
            type="file"
            accept=".key,.pem"
            onChange={(e) => void onPickFile(e, setPrivateKey)}
          />
          <FieldHint>{t('import.privateKeyHint')}</FieldHint>
        </div>

        {attempted && block && <FieldHint error>{t(`import.block.${block}`)}</FieldHint>}
        {saveError && <div className="tls-alert tls-alert-error">{saveError}</div>}
        {notice && <div className="tls-alert tls-alert-ok">{notice}</div>}

        <div className="tls-actions">
          <Button variant="primary" onClick={() => void onImport()} disabled={busy}>
            {busy ? t('import.submitting') : t('import.submit')}
          </Button>
        </div>
      </Card>

      <Card title={t('regenerate.heading')}>
        <p className="tls-muted">{t('regenerate.intro')}</p>
        <label className="tls-label" htmlFor="tls-names">
          {t('regenerate.names')}
        </label>
        <TextArea
          id="tls-names"
          className="mono"
          rows={3}
          value={names}
          placeholder={t('regenerate.namesPlaceholder')}
          onChange={(e) => setNames(e.target.value)}
        />
        <FieldHint>{t('regenerate.namesHint')}</FieldHint>
        <div className="tls-actions">
          <Button onClick={() => void onRegenerate()} disabled={busy}>
            {busy ? t('regenerate.submitting') : t('regenerate.submit')}
          </Button>
        </div>
      </Card>
    </div>
  );
}
