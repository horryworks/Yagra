// SPDX-License-Identifier: AGPL-3.0-only
// Settings ▸ Support bundle (ADR-045, split out of Settings ▸ Yagra health by ADR-055 Inc.6).
// One archive of this deployment's build provenance, health, configuration, database state and
// logs, for diagnosing a site nobody can open a shell on.
//
// It used to be the last card on the health page. That page is read-only self-monitoring gated on
// View, and its own note names only the four things it shows; this is a write-out action needing
// ManageSystem + ManageCredentials + ViewAudit. Different shelf (ADR-055 R8), and a screen nobody
// can find is a screen that does not exist in a closed network where documentation is unreachable
// (R5).
//
// Admin-only in the UI because it is Admin-only in the API — the endpoint demands all three of
// those permissions, so showing the button to an Operator would be offering a 403.
// ⚠️ Since ADR-057 an Operator holds one of the three, which is what makes the union guard on the
// API side load-bearing rather than decorative.
//
// The failure text is shown verbatim rather than replaced by a generic message: the one refusal
// that matters here is the redaction stop, whose message names the file and the rule and is an
// instruction to the operator, not a fault to retry past.
//
// No `.ts` sidecar: there is no judgement to extract. The only decisions are a constant array of
// window lengths and one filename fallback, and a test in this file would never run (testing.md).

import { useState } from 'react';
import { useTranslation } from 'react-i18next';
import { PageHeader } from '../components/ui/PageHeader';
import { Page } from '../components/ui/Page';
import { Card } from '../components/ui/Card';
import { Fields, FieldRow, SaveNote } from '../components/ui/FormLayout';
import { Field, Select } from '../components/ui/Field';
import { Button } from '../components/ui/Button';
import { PermissionHint } from '../components/ui/PermissionHint';
import { InfoTip } from '../components/ui/InfoTip';
import { NodePicker } from '../components/NodePicker/NodePicker';
import { useCan } from '../store';
import { api, errMsg } from '../services/api';
import { saveBlob } from '../lib/download';
import { formatBytes } from '../lib/format';
import './SupportBundlePage.css';

/** Log windows offered for a support bundle. Bounded by the appender's own retention in practice,
 *  so the longest option is a request rather than a promise — a deployment keeping 48 hourly files
 *  cannot serve a week however this is set. */
const BUNDLE_WINDOWS = [1, 6, 24, 72] as const;

export function SupportBundlePage() {
  const { t } = useTranslation('system');
  // The bundle endpoint needs ManageSystem (plus two more); ask for a permission, not a role.
  const canSystem = useCan('manage_system');

  return (
    <Page layout="sections">
      <PageHeader
        title={t('nav:settings.supportBundle')}
        trail={[{ label: t('nav:sections.settings') }, { label: t('nav:settings.supportBundle') }]}
      />
      {canSystem ? (
        <TakeBundleCard />
      ) : (
        <Card title={t('supportBundle.title')}>
          <PermissionHint permission="manage_system" />
        </Card>
      )}
    </Page>
  );
}

/** The window/node controls and the download itself — mounted only for a caller who may use it. */
function TakeBundleCard() {
  const { t } = useTranslation('system');
  const [hours, setHours] = useState<number>(6);
  // Optional: the node this bundle is *about* (ADR-045 Inc.1). Local state, not the URL — a
  // support bundle is an action taken once, not a view worth sharing a link to.
  const [node, setNode] = useState<{ id: string; name: string } | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [done, setDone] = useState<{ file: string; size: string } | null>(null);

  const download = () => {
    setBusy(true);
    setError(null);
    setDone(null);
    api
      .downloadSupportBundle(hours, node?.id)
      .then(({ blob, filename }) => {
        // The server names the file; the fallback exists only for a proxy that strips the header.
        const file = filename ?? 'yagra-support.tar.gz';
        saveBlob(blob, file);
        setDone({ file, size: formatBytes(blob.size) });
      })
      .catch((e: unknown) => setError(errMsg(e, t('supportBundle.err'))))
      .finally(() => setBusy(false));
  };

  return (
    <Card
      title={t('supportBundle.title')}
      footer={
        <>
          {error && <SaveNote tone="error">{error}</SaveNote>}
          {done && <SaveNote tone="ok">{t('supportBundle.done', done)}</SaveNote>}
          <Button variant="primary" onClick={download} disabled={busy}>
            {busy ? t('supportBundle.busy') : t('supportBundle.action')}
          </Button>
          {/* What leaves the deployment is listed inside the archive; that is the one thing worth
              knowing before pressing the button (ADR-200). */}
          <InfoTip infoKey="system:supportBundle.info" label={t('supportBundle.action')} />
        </>
      }
    >
      <Fields className="sb-fields">
        <FieldRow>
          <Field label={t('supportBundle.window')}>
            <Select
              value={hours}
              onChange={(e) => setHours(Number(e.target.value))}
              disabled={busy}
              aria-label={t('supportBundle.window')}
            >
              {BUNDLE_WINDOWS.map((h) => (
                <option key={h} value={h}>
                  {t('supportBundle.windowHours', { count: h })}
                </option>
              ))}
            </Select>
          </Field>
          <Field label={t('supportBundle.node')}>
            <NodePicker
              value={node?.id ?? null}
              valueLabel={node?.name}
              onChange={setNode}
              placeholder={t('supportBundle.nodeAny')}
            />
          </Field>
        </FieldRow>
      </Fields>
    </Card>
  );
}
