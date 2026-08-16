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
import { Card } from '../components/ui/Card';
import { Button } from '../components/ui/Button';
import { PermissionHint } from '../components/ui/PermissionHint';
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
    <div>
      <PageHeader
        title={t('nav:settings.supportBundle')}
        trail={[{ label: t('nav:sections.settings') }, { label: t('nav:settings.supportBundle') }]}
        note={t('supportBundle.help')}
      />
      <Card title={t('supportBundle.title')} className="support-bundle-card">
        <p className="muted">{t('supportBundle.contents')}</p>
        {canSystem ? (
          <TakeBundlePanel />
        ) : (
          <PermissionHint permission="manage_system" signInHint={t('supportBundle.signInHint')} />
        )}
      </Card>
    </div>
  );
}

/** The window/node controls and the download itself — mounted only for a caller who may use it. */
function TakeBundlePanel() {
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
    <>
      <div className="sb-actions">
        <label className="sb-window">
          <span className="muted">{t('supportBundle.window')}</span>
          <select
            className="field"
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
          </select>
        </label>
        <label className="sb-node">
          <span className="muted">{t('supportBundle.node')}</span>
          <NodePicker
            value={node?.id ?? null}
            valueLabel={node?.name}
            onChange={setNode}
            placeholder={t('supportBundle.nodeAny')}
          />
        </label>
        <Button variant="primary" onClick={download} disabled={busy}>
          {busy ? t('supportBundle.busy') : t('supportBundle.action')}
        </Button>
      </div>
      <p className="muted sb-node-help">{t('supportBundle.nodeHelp')}</p>
      {error && <p className="form-error">{error}</p>}
      {done && <p className="sb-done">{t('supportBundle.done', done)}</p>}
    </>
  );
}
