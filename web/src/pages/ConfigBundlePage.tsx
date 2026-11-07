// SPDX-License-Identifier: AGPL-3.0-only
// Settings ▸ Configuration bundle (ADR-040 decision 3): move a monitoring configuration from one
// deployment to another. Admin-only on both sides.
//
// This is deliberately not the backup screen. A bundle carries no secrets and no history; disaster
// recovery is a database dump (DEPLOYMENT.md "Backup & restore"), and the page says so — one sentence
// and a link to the relocation screen, which is what moves a whole deployment (ADR-200) — rather than
// letting an operator infer that downloading this is enough.
//
// The import flow is pick → check → apply, and the check is not cosmetic: it runs the real import
// inside a transaction that is rolled back, so the report it shows is what applying would actually
// do. Judgement (parsing a picked file, summarizing sections, totalling a report) lives in
// `configBundle.ts` where tests can reach it — a test in this file would never run (testing.md).

import { useCallback, useRef, useState } from 'react';
import { Trans, useTranslation } from 'react-i18next';
import { api, errMsg } from '../services/api';
import { saveBlob } from '../lib/download';
import { useCan } from '../store';
import { PageHeader } from '../components/ui/PageHeader';
import { Page, Stack } from '../components/ui/Page';
import { Card } from '../components/ui/Card';
import { Facts } from '../components/ui/Facts';
import { SaveNote } from '../components/ui/FormLayout';
import { Callout } from '../components/ui/Section';
import { Button } from '../components/ui/Button';
import { PermissionHint } from '../components/ui/PermissionHint';
import { ScreenLink } from '../components/ui/ScreenLink';
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
  // An import rewrites the whole configuration in one call (ADR-057).
  const canSystem = useCan('manage_system');

  return (
    <Page layout="sections">
      <PageHeader
        title={t('nav:settings.configBundle')}
        trail={[{ label: t('nav:sections.settings') }, { label: t('nav:settings.configBundle') }]}
      />
      {canSystem ? (
        <ExportCard />
      ) : (
        <Card title={t('bundle.export.title')}>
          <NotBackup />
          <PermissionHint permission="manage_system" />
        </Card>
      )}
      {canSystem ? (
        <ImportCard />
      ) : (
        <Card title={t('bundle.import.title')}>
          <PermissionHint permission="manage_system" />
        </Card>
      )}
    </Page>
  );
}

/** The one sentence that says this is not a backup, with the way to the screen that is. Shown
 *  whether or not the caller may export. */
function NotBackup() {
  const { t } = useTranslation('system');
  return (
    <p className="cb-line muted">
      <Trans
        t={t}
        i18nKey="bundle.notBackup"
        components={{ lnk: <ScreenLink to="/settings/relocation" /> }}
      />
    </p>
  );
}

/** Download the current configuration as a JSON file. */
function ExportCard() {
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
        saveBlob(blob, bundleFilename(bundle.exported_at));
        setDone(bundle);
      })
      .catch((e: unknown) => setError(errMsg(e, t('bundle.export.err'))))
      .finally(() => setBusy(false));
  };

  return (
    <Card
      title={t('bundle.export.title')}
      footer={
        <>
          {error && <SaveNote tone="error">{error}</SaveNote>}
          {done && (
            <SaveNote tone="ok">
              {t('bundle.export.done', {
                count: bundleRowCount(done),
                file: bundleFilename(done.exported_at),
              })}
            </SaveNote>
          )}
          <Button variant="primary" onClick={download} disabled={busy}>
            {t('bundle.export.action')}
          </Button>
        </>
      }
    >
      <NotBackup />
      {done && (
        <>
          <SectionList bundle={done} />
          <NoteList notes={done.notes ?? []} />
        </>
      )}
    </Card>
  );
}

/** Pick a bundle, check it against this deployment, then apply it. */
function ImportCard() {
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
    <Card
      title={t('bundle.import.title')}
      footer={
        (error || bundle) && (
          <>
            {error && <SaveNote tone="error">{error}</SaveNote>}
            {bundle && (
              <>
                <Button onClick={() => send(true)} disabled={busy}>
                  {t('bundle.import.check')}
                </Button>
                <Button variant="primary" onClick={() => send(false)} disabled={busy}>
                  {t('bundle.import.apply')}
                </Button>
              </>
            )}
          </>
        )
      }
    >
      <input
        ref={fileRef}
        type="file"
        accept="application/json,.json"
        className="cb-file"
        onChange={(e) => pick(e.target.files?.[0])}
        aria-label={t('bundle.import.pickAria')}
      />
      {filename && !bundle && !error && (
        <p className="cb-line muted">{t('bundle.import.reading')}</p>
      )}
      {bundle && (
        <>
          <p className="cb-line ok">
            {t('bundle.import.picked', { file: filename, count: bundleRowCount(bundle) })}
          </p>
          <SectionList bundle={bundle} />
          <Callout tone="warning">{t('bundle.import.warning')}</Callout>
        </>
      )}
      {report && <ReportView report={report} />}
    </Card>
  );
}

/** What a bundle carries, section by section. */
function SectionList({ bundle }: { bundle: ConfigBundle }) {
  const { t } = useTranslation('system');
  const sections = bundleSections(bundle);
  if (sections.length === 0) return <p className="cb-line muted">{t('bundle.empty')}</p>;
  return (
    <Facts
      items={sections.map((s) => ({
        key: s.table,
        label: t(`bundle.table.${s.table}`, { defaultValue: s.table }),
        value: <span className="mono">{s.count}</span>,
      }))}
    />
  );
}

/** The per-table result of an import, plus every note it produced. */
function ReportView({ report }: { report: ImportReport }) {
  const { t } = useTranslation('system');
  const totals = reportTotals(report);
  const changed = report.tables.filter((r) => r.created + r.updated + r.skipped > 0);
  return (
    <Stack gap={12} className="cb-report">
      <p className={report.dry_run ? 'cb-line warn' : 'cb-line ok'}>
        {report.dry_run
          ? t('bundle.report.dryRun', totals)
          : reportIsEmpty(report)
            ? t('bundle.report.noop')
            : t('bundle.report.applied', totals)}
      </p>
      {changed.length > 0 && (
        <Facts
          items={changed.map((r) => ({
            key: r.table,
            label: t(`bundle.table.${r.table}`, { defaultValue: r.table }),
            value: (
              <span className="mono">
                {t('bundle.report.row', {
                  created: r.created,
                  updated: r.updated,
                  skipped: r.skipped,
                })}
              </span>
            ),
          }))}
        />
      )}
      <NoteList notes={report.notes} />
    </Stack>
  );
}

/** The notes an export or an import produced. Rendered even when empty is *not* the choice here —
 *  an absent list means nothing was dropped, which is worth showing as silence rather than a row. */
function NoteList({ notes }: { notes: ImportReport['notes'] }) {
  const { t } = useTranslation('system');
  if (notes.length === 0) return null;
  return (
    <Stack as="ul" gap={8} className="cb-notes">
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
    </Stack>
  );
}
