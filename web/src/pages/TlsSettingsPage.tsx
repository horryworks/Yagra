// SPDX-License-Identifier: AGPL-3.0-only
// TLS certificate (Settings ▸ TLS, ADR-044): what the WebUI is serving, and how to replace it.
// ManageSystem-gated (ADR-057). The private key is write-only — the API never returns it — while the
// certificate is downloadable, because that is what an operator needs in order to trust a
// self-signed deployment from Prometheus, curl, or an OS trust store.
//
// Every decision this page makes lives in `tlsSettingsForm.ts`, where Vitest can reach it. What is
// left here is layout.

import { useCallback, useEffect, useState } from 'react';
import { useTranslation } from 'react-i18next';

import { api, errMsg } from '../services/api';
import { saveBlob } from '../lib/download';
import { formatTimestamp } from '../lib/format';
import { classifyLoadError, type LoadBlock } from '../lib/loadState';
import { LoadBlockNotice } from '../components/ui/LoadBlockNotice';
import type { WebTlsStatus } from '../types/api';
import {
  certificateFilename,
  expiryLevel,
  importBlock,
  parseNames,
  TLS_SOURCE_INFO,
  type ImportBlock,
} from './tlsSettingsForm';
import { PageHeader } from '../components/ui/PageHeader';
import { Page, Inline } from '../components/ui/Page';
import { Card } from '../components/ui/Card';
import { Facts } from '../components/ui/Facts';
import { Fields, SaveNote } from '../components/ui/FormLayout';
import { Callout } from '../components/ui/Section';
import { Button } from '../components/ui/Button';
import { ConfirmDeleteModal } from '../components/ui/ConfirmDeleteModal';
import { Badge, type Tone } from '../components/ui/Badge';
import { Field, TextArea, FieldError } from '../components/ui/Field';
import { InfoPress } from '../components/ui/InfoTip';
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
  // The certificate is the deployment's own, so an operator is refused it. Say which privilege
  // that needs instead of painting the 403 as a load failure (ADR-056).
  const [loadBlock, setLoadBlock] = useState<LoadBlock | null>(null);
  const [busy, setBusy] = useState(false);
  const [notice, setNotice] = useState<string | null>(null);
  const [saveError, setSaveError] = useState<string | null>(null);

  const [certificate, setCertificate] = useState('');
  const [privateKey, setPrivateKey] = useState('');
  const [names, setNames] = useState('');
  const [attempted, setAttempted] = useState(false);
  const [confirmingRegenerate, setConfirmingRegenerate] = useState(false);

  const load = useCallback(async () => {
    try {
      setStatus(await api.getWebTls());
      setLoadError(null);
      setLoadBlock(null);
    } catch (e) {
      const b = classifyLoadError(e);
      if (b) setLoadBlock(b);
      else setLoadError(errMsg(e, t('error.load')));
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

  // Destructive consent goes through the shared dialog, not `window.confirm`: a native dialog is
  // unstyled, untranslatable beyond its body text, and its OK button says "OK" where this one says
  // "Regenerate". A rejection is rethrown so the dialog keeps itself open and shows the reason.
  async function onRegenerate() {
    setBusy(true);
    setNotice(null);
    setSaveError(null);
    try {
      setStatus(await api.regenerateWebTls(parseNames(names)));
    } finally {
      setBusy(false);
    }
  }

  function onDownload() {
    if (!view) return;
    const blob = new Blob([view.certificate], { type: 'application/x-pem-file' });
    saveBlob(blob, certificateFilename(view));
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

  if (loadBlock) {
    return (
      <Page layout="sections">
        <PageHeader title={t('title')} />
        <LoadBlockNotice
          block={loadBlock}
          unavailable={t('error.load')}
          permission="manage_system"
        />
      </Page>
    );
  }

  return (
    <Page layout="sections">
      <PageHeader title={t('title')} />

      {loadError && (
        <Callout tone="critical" live>
          {loadError}
        </Callout>
      )}

      {status?.api_port_is_public && (
        <Callout tone="warning">
          <strong>{t('warning.apiPortPublicTitle')}</strong>
          <p className="tls-callout-body">{t('warning.apiPortPublic')}</p>
        </Callout>
      )}

      <Card
        title={t('current.heading')}
        footer={view && <Button onClick={onDownload}>{t('current.download')}</Button>}
      >
        {!status && !loadError && <p className="tls-line">{t('loading')}</p>}
        {status && !view && <p className="tls-line">{t('none')}</p>}
        {view && (
          <>
            <Inline gap={8}>
              {/* The source says whether Yagra may replace the certificate on its own — the
                  difference between "this renews itself" and "nothing touches this but you". */}
              <InfoPress
                infoKey={TLS_SOURCE_INFO[view.source]}
                className={`badge badge-${view.source === 'imported' ? 'info' : 'neutral'}`}
              >
                {t(`source.${view.source}`)}
              </InfoPress>
              <Badge tone={EXPIRY_TONE[level]}>
                {t(`expiry.${level}`, { days: Math.abs(view.expires_in_days) })}
              </Badge>
              {/* Whether the web server is serving what is stored: a change goes live within
                  seconds with no restart, and this is where that shows (ADR-200). An unreadable key
                  has its own alert below and says nothing here. */}
              {!view.key_unreadable &&
                (view.materialized ? (
                  <Badge tone="up">{t('live')}</Badge>
                ) : (
                  <InfoPress
                    infoKey="settings-tls:notMaterialized.info"
                    className="badge badge-warning"
                  >
                    {t('notMaterialized.label')}
                  </InfoPress>
                ))}
            </Inline>
            {view.key_unreadable && (
              <Callout tone="critical">{t('warning.keyUnreadable')}</Callout>
            )}

            <Facts
              items={[
                {
                  label: t('current.subject'),
                  value: <span className="mono">{view.subject}</span>,
                },
                {
                  label: t('current.issuer'),
                  value: <span className="mono">{view.issuer}</span>,
                },
                {
                  label: t('current.sans'),
                  value: <span className="mono">{view.sans.join(', ') || '—'}</span>,
                },
                {
                  label: t('current.validity'),
                  value: t('current.validityRange', {
                    from: formatTimestamp(Date.parse(view.not_before)),
                    to: formatTimestamp(Date.parse(view.not_after)),
                  }),
                },
                { label: t('current.keyAlgorithm'), value: view.key_algorithm },
                {
                  label: t('current.fingerprint'),
                  value: <span className="mono tls-fingerprint">{view.fingerprint_sha256}</span>,
                },
                {
                  label: t('current.importedAt'),
                  value: (
                    <>
                      {formatTimestamp(Date.parse(view.imported_at))}
                      {view.imported_by
                        ? ` (${t('current.importedBy', { user: view.imported_by })})`
                        : ''}
                    </>
                  ),
                },
              ]}
            />
          </>
        )}
      </Card>

      <Card
        title={t('import.heading')}
        footer={
          <>
            {saveError && <SaveNote tone="error">{saveError}</SaveNote>}
            {notice && <SaveNote tone="ok">{notice}</SaveNote>}
            <Button variant="primary" onClick={() => void onImport()} disabled={busy}>
              {busy ? t('import.submitting') : t('import.submit')}
            </Button>
          </>
        }
      >
        <Fields>
          <Field label={t('import.certificate')} htmlFor="tls-cert">
            <TextArea
              id="tls-cert"
              className="mono"
              rows={8}
              value={certificate}
              placeholder={t('import.certificatePlaceholder')}
              onChange={(e) => setCertificate(e.target.value)}
            />
            <Inline>
              <input
                type="file"
                accept=".crt,.pem,.cer,.cert,application/x-pem-file"
                onChange={(e) => void onPickFile(e, setCertificate)}
              />
            </Inline>
          </Field>
          <Field label={t('import.privateKey')} htmlFor="tls-key">
            <TextArea
              id="tls-key"
              className="mono"
              rows={6}
              value={privateKey}
              placeholder={t('import.privateKeyPlaceholder')}
              onChange={(e) => setPrivateKey(e.target.value)}
            />
            <Inline>
              <input
                type="file"
                accept=".key,.pem"
                onChange={(e) => void onPickFile(e, setPrivateKey)}
              />
            </Inline>
          </Field>
          {attempted && block && <FieldError>{t(`import.block.${block}`)}</FieldError>}
        </Fields>
      </Card>

      <Card
        title={t('regenerate.heading')}
        footer={
          <Button onClick={() => setConfirmingRegenerate(true)} disabled={busy}>
            {busy ? t('regenerate.submitting') : t('regenerate.submit')}
          </Button>
        }
      >
        {/* What an empty list means changes what to type, so it is behind the label's ⓘ. */}
        <Field
          label={t('regenerate.names')}
          htmlFor="tls-names"
          infoKey="settings-tls:regenerate.info"
        >
          <TextArea
            id="tls-names"
            className="mono"
            rows={3}
            value={names}
            placeholder={t('regenerate.namesPlaceholder')}
            onChange={(e) => setNames(e.target.value)}
          />
        </Field>
      </Card>

      {confirmingRegenerate && (
        <ConfirmDeleteModal
          title={t('regenerate.heading')}
          confirmLabel={t('regenerate.submit')}
          onConfirm={onRegenerate}
          errorFallback={t('error.save')}
          onClose={() => setConfirmingRegenerate(false)}
          onDone={() => {
            setConfirmingRegenerate(false);
            setNotice(t('regenerate.success'));
          }}
        >
          {/* An imported certificate is the operator's, and replacing it with a self-signed one puts
              the browser warning back. That deserves a different sentence from replacing a
              self-signed one. */}
          {view?.source === 'self_signed'
            ? t('regenerate.confirm')
            : t('regenerate.confirmImported')}
        </ConfirmDeleteModal>
      )}
    </Page>
  );
}
