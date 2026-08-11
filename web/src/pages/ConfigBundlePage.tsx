// SPDX-License-Identifier: AGPL-3.0-only
// Settings ▸ Configuration bundle (ADR-040 decision 3): move a monitoring configuration from one
// deployment to another. Admin-only on both sides.
//
// This is deliberately not the backup screen. A bundle carries no secrets and no history; disaster
// recovery is a database dump (DEPLOYMENT.md "Backup & restore"), and the page says so rather than
// letting an operator infer that downloading this is enough.
//
// The import flow is pick → check → apply, and the check is not cosmetic: it runs the real import
// inside a transaction that is rolled back, so the report it shows is what applying would actually
// do. Judgement (parsing a picked file, summarizing sections, totalling a report) lives in
// `configBundle.ts` where tests can reach it — a test in this file would never run (testing.md).

import { useCallback, useRef, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { api, errMsg } from '../services/api';
import { useAuthStore } from '../store';
import { PageHeader } from '../components/ui/PageHeader';
import { Card } from '../components/ui/Card';
import { Button } from '../components/ui/Button';
import type { ConfigBundle, ImportReport } from '../types/api';
import {
  bundleFilename,
  bundleImportErrorKey,
  bundleRowCount,
  bundleSections,
  parseBundleFile,
  reportIsEmpty,
  reportTotals,
} from './configBundle';
import './ConfigBundlePage.css';

export function ConfigBundlePage() {
  const { t } = useTranslation('system');
  const authed = useAuthStore((s) => s.authed);

  return (
    <div>
      <PageHeader
        title={t('nav:settings.configBundle')}
        trail={[{ label: t('nav:sections.settings') }, { label: t('nav:settings.configBundle') }]}
        note={t('bundle.pageNote')}
      />
      <Card title={t('bundle.export.title')}>
        <p className="cb-help muted">{t('bundle.export.help')}</p>
        <p className="cb-help muted">{t('bundle.notBackup')}</p>
        {authed ? <ExportPanel /> : <p className="muted">{t('settings.signInHint')}</p>}
      </Card>
      <Card title={t('bundle.import.title')}>
        <p className="cb-help muted">{t('bundle.import.help')}</p>
        {authed ? <ImportPanel /> : <p className="muted">{t('settings.signInHint')}</p>}
      </Card>
    </div>
  );
}

/** Download the current configuration as a JSON file. */
function ExportPanel() {
  const { t } = useTranslation('system');
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [done, setDone] = useState<ConfigBundle | null>(null);

  const download = () => {
    setBusy(true);
    setError(null);
    setDone(null);
    api
      .exportConfigBundle()
      .then((bundle) => {
        const blob = new Blob([JSON.stringify(bundle, null, 2)], {
          type: 'application/json;charset=utf-8',
        });
        const url = URL.createObjectURL(blob);
        const a = document.createElement('a');
        a.href = url;
        a.download = bundleFilename(bundle.exported_at);
        a.click();
        URL.revokeObjectURL(url);
        setDone(bundle);
      })
      .catch((e: unknown) => setError(errMsg(e, t('bundle.export.err'))))
      .finally(() => setBusy(false));
  };

  return (
    <div className="cb-panel">
      <div className="cb-actions">
        <Button variant="primary" onClick={download} disabled={busy}>
          {t('bundle.export.action')}
        </Button>
      </div>
      {error && <p className="form-error">{error}</p>}
      {done && (
        <>
          <p className="cb-ok">
            {t('bundle.export.done', { count: bundleRowCount(done), file: bundleFilename(done.exported_at) })}
          </p>
          <SectionList bundle={done} />
          <NoteList notes={done.notes ?? []} />
        </>
      )}
    </div>
  );
}

/** Pick a bundle, check it against this deployment, then apply it. */
function ImportPanel() {
  const { t } = useTranslation('system');
  const fileRef = useRef<HTMLInputElement>(null);
  const [bundle, setBundle] = useState<ConfigBundle | null>(null);
  const [filename, setFilename] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [report, setReport] = useState<ImportReport | null>(null);

  const pick = useCallback(
    (file: File | undefined) => {
      setBundle(null);
      setReport(null);
      setError(null);
      setFilename(file?.name ?? null);
      if (!file) return;
      file
        .text()
        .then((text) => {
          const parsed = parseBundleFile(text);
          if (parsed.ok) {
            setBundle(parsed.bundle);
            return;
          }
          setError(
            t(`bundle.import.err.${bundleImportErrorKey(parsed.reason)}`, {
              version: 'version' in parsed ? parsed.version : undefined,
            }),
          );
        })
        .catch(() => setError(t('bundle.import.err.read')));
    },
    [t],
  );

  const send = (dryRun: boolean) => {
    if (!bundle) return;
    setBusy(true);
    setError(null);
    setReport(null);
    api
      .importConfigBundle(bundle, { dryRun })
      .then(setReport)
      .catch((e: unknown) => setError(errMsg(e, t('bundle.import.err.failed'))))
      .finally(() => setBusy(false));
  };

  return (
    <div className="cb-panel">
      <input
        ref={fileRef}
        type="file"
        accept="application/json,.json"
        className="cb-file"
        onChange={(e) => pick(e.target.files?.[0])}
        aria-label={t('bundle.import.pickAria')}
      />
      {filename && !bundle && !error && <p className="muted">{t('bundle.import.reading')}</p>}
      {bundle && (
        <>
          <p className="cb-ok">
            {t('bundle.import.picked', { file: filename, count: bundleRowCount(bundle) })}
          </p>
          <SectionList bundle={bundle} />
          <p className="cb-warn">{t('bundle.import.warning')}</p>
          <div className="cb-actions">
            <Button onClick={() => send(true)} disabled={busy}>
              {t('bundle.import.check')}
            </Button>
            <Button variant="primary" onClick={() => send(false)} disabled={busy}>
              {t('bundle.import.apply')}
            </Button>
          </div>
        </>
      )}
      {error && <p className="form-error">{error}</p>}
      {report && <ReportView report={report} />}
    </div>
  );
}

/** What a bundle carries, section by section. */
function SectionList({ bundle }: { bundle: ConfigBundle }) {
  const { t } = useTranslation('system');
  const sections = bundleSections(bundle);
  if (sections.length === 0) return <p className="muted">{t('bundle.empty')}</p>;
  return (
    <ul className="cb-sections">
      {sections.map((s) => (
        <li key={s.table}>
          <span className="cb-section-name">
            {t(`bundle.table.${s.table}`, { defaultValue: s.table })}
          </span>
          <span className="cb-section-count">{s.count}</span>
        </li>
      ))}
    </ul>
  );
}

/** The per-table result of an import, plus every note it produced. */
function ReportView({ report }: { report: ImportReport }) {
  const { t } = useTranslation('system');
  const totals = reportTotals(report);
  const changed = report.tables.filter((r) => r.created + r.updated + r.skipped > 0);
  return (
    <div className="cb-report">
      <p className={report.dry_run ? 'cb-warn' : 'cb-ok'}>
        {report.dry_run
          ? t('bundle.report.dryRun', totals)
          : reportIsEmpty(report)
            ? t('bundle.report.noop')
            : t('bundle.report.applied', totals)}
      </p>
      {changed.length > 0 && (
        <ul className="cb-sections">
          {changed.map((r) => (
            <li key={r.table}>
              <span className="cb-section-name">
                {t(`bundle.table.${r.table}`, { defaultValue: r.table })}
              </span>
              <span className="cb-section-count">
                {t('bundle.report.row', {
                  created: r.created,
                  updated: r.updated,
                  skipped: r.skipped,
                })}
              </span>
            </li>
          ))}
        </ul>
      )}
      <NoteList notes={report.notes} />
    </div>
  );
}

/** The notes an export or an import produced. Rendered even when empty is *not* the choice here —
 *  an absent list means nothing was dropped, which is worth showing as silence rather than a row. */
function NoteList({ notes }: { notes: ImportReport['notes'] }) {
  const { t } = useTranslation('system');
  if (notes.length === 0) return null;
  return (
    <ul className="cb-notes">
      {notes.map((n) => (
        <li key={`${n.table}-${n.code}-${n.field ?? ''}`}>
          {t(`bundle.notes.${n.code}`, { defaultValue: n.code })}
          {' — '}
          <span className="cb-section-name">
            {t(`bundle.table.${n.table}`, { defaultValue: n.table })}
          </span>
          {n.field ? ` (${n.field})` : ''} · {n.count}
        </li>
      ))}
    </ul>
  );
}
