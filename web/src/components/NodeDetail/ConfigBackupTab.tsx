// SPDX-License-Identifier: AGPL-3.0-only
// NodeDetail ▸ Config (ADR-210 stage 2): the node's configuration as the bundled oxidized fetches
// it. The settings (on or off, the CLI login, SSH or telnet, the model), what oxidized last
// reported, the stored versions, a version's text and two versions side by side.
//
// The text and the comparison need `manage_config` (the server's `remove_secret` is model by model
// and can miss one), so a Viewer sees the versions and the last result and nothing of the text.
// The judgement — the form, which version a version is compared with, when a status is stale —
// is `configBackupForm.ts`.

import { useEffect, useMemo, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { api, errMsg } from '../../services/api';
import { useCan } from '../../store';
import { useLoad } from '../../lib/useLoad';
import { LoadGate } from '../ui/LoadGate';
import { Button } from '../ui/Button';
import { Select } from '../ui/Field';
import { DataTable, type Column } from '../ui/DataTable';
import { EmptyState } from '../ui/EmptyState';
import { ScreenLink } from '../ui/ScreenLink';
import { relativeTime, formatExactTime } from '../../lib/format';
import { isCliCredentialKind } from '../../lib/credentialKinds';
import { BACKUP_INPUTS, type BackupInput, type ConfigDiff, type ConfigVersion, type NodeDetail } from '../../types/api';
import {
  bodyOf,
  formOf,
  isDirty,
  isStale,
  previousOf,
  resultTone,
  shortOid,
  type ConfigBackupForm,
} from './configBackupForm';
import './ConfigBackupTab.css';

/** What the viewer pane shows: one version's text, or a version beside the one before it. */
type Shown = { kind: 'text'; oid: string } | { kind: 'diff'; from: string; to: string };

export function ConfigBackupTab({ node }: { node: NodeDetail }) {
  const { t } = useTranslation('nodes');
  const canConfig = useCan('manage_config');
  const canCreds = useCan('manage_credentials');
  const view = useLoad(() => api.getConfigBackup(node.id), [node.id], {
    initial: null,
    errorFallback: t('config.err.load'),
  });
  const available = view.data?.available ?? false;
  const versions = useLoad(() => api.listConfigVersions(node.id), [node.id, available], {
    initial: [] as ConfigVersion[],
    enabled: available,
    errorFallback: t('config.err.versions'),
  });
  const creds = useLoad(() => api.listCredentials(), [], {
    initial: [],
    enabled: canConfig && canCreds,
  });
  const logins = useMemo(() => creds.data.filter((c) => isCliCredentialKind(c.kind)), [creds.data]);

  const [form, setForm] = useState<ConfigBackupForm | null>(null);
  useEffect(() => {
    if (view.data) setForm(formOf(view.data));
  }, [view.data]);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [asked, setAsked] = useState(false);
  const [shown, setShown] = useState<Shown | null>(null);

  const save = () => {
    if (!form) return;
    setBusy(true);
    setError(null);
    api
      .putConfigBackup(node.id, bodyOf(form))
      .then(() => view.reload())
      .catch((e: unknown) => setError(errMsg(e, t('config.err.save'))))
      .finally(() => setBusy(false));
  };
  const fetchNow = () => {
    setBusy(true);
    setError(null);
    api
      .fetchConfigNow(node.id)
      .then(() => setAsked(true))
      .catch((e: unknown) => setError(errMsg(e, t('config.err.fetch'))))
      .finally(() => setBusy(false));
  };

  const columns = useMemo<Column<ConfigVersion>[]>(
    () => [
      {
        key: 'stored',
        header: t('config.cols.stored'),
        width: '220px',
        render: (v) => (v.stored_at ? formatExactTime(v.stored_at) : '—'),
      },
      {
        key: 'oid',
        header: t('config.cols.version'),
        width: '1fr',
        render: (v) => <code className="mono">{shortOid(v.oid)}</code>,
      },
      {
        key: 'actions',
        header: t('config.cols.actions'),
        width: '200px',
        align: 'right',
        render: (v) => {
          if (!canConfig) return null;
          const prev = previousOf(versions.data, v.oid);
          return (
            <span className="cfgb-actions">
              <Button variant="outline" onClick={() => setShown({ kind: 'text', oid: v.oid })}>
                {t('config.view')}
              </Button>
              {prev && (
                <Button
                  variant="outline"
                  onClick={() => setShown({ kind: 'diff', from: prev, to: v.oid })}
                >
                  {t('config.compare')}
                </Button>
              )}
            </span>
          );
        },
      },
    ],
    [t, canConfig, versions.data],
  );

  const v = view.data;
  const status = v?.status ?? null;
  return (
    <LoadGate load={view}>
      {view.error && (
        <p className="form-error" role="alert">
          {view.error}
        </p>
      )}
      {v && form && (
        <div className="cfgb">
          {!v.available && <p className="form-status">{t('config.unavailable')}</p>}
          <div className="cfgb-settings">
            <label className="cfgb-check">
              <input
                type="checkbox"
                checked={form.enabled}
                disabled={!canConfig}
                onChange={(e) => setForm({ ...form, enabled: e.target.checked })}
              />
              {t('config.enabled')}
            </label>
            <div className="modal-field">
              <label className="modal-field-label" htmlFor="cfgb-login">
                {t('config.login')}
              </label>
              <Select
                id="cfgb-login"
                value={form.credentialId}
                disabled={!canConfig}
                onChange={(e) => setForm({ ...form, credentialId: e.target.value })}
              >
                <option value="">{t('config.loginNone')}</option>
                {logins.map((c) => (
                  <option key={c.id} value={c.id}>
                    {c.name}
                  </option>
                ))}
              </Select>
            </div>
            <div className="modal-field">
              <label className="modal-field-label" htmlFor="cfgb-input">
                {t('config.connection')}
              </label>
              <Select
                id="cfgb-input"
                value={form.input}
                disabled={!canConfig}
                onChange={(e) => setForm({ ...form, input: e.target.value as BackupInput })}
              >
                {BACKUP_INPUTS.map((i) => (
                  <option key={i} value={i}>
                    {t(`config.input.${i}`)}
                  </option>
                ))}
              </Select>
            </div>
            <div className="modal-field">
              <label className="modal-field-label" htmlFor="cfgb-model">
                {t('config.model')}
              </label>
              <Select
                id="cfgb-model"
                value={form.model}
                disabled={!canConfig}
                onChange={(e) => setForm({ ...form, model: e.target.value })}
              >
                <option value="">
                  {v.profile_model
                    ? t('config.modelFromProfile', { model: v.profile_model })
                    : t('config.modelPick')}
                </option>
                {v.models.map((m) => (
                  <option key={m} value={m}>
                    {m}
                  </option>
                ))}
              </Select>
            </div>
            {canConfig && (
              <div className="cfgb-buttons">
                <Button
                  variant="primary"
                  onClick={save}
                  disabled={busy || !isDirty(form, v)}
                >
                  {t('common:actions.save')}
                </Button>
                {v.available && v.readiness === 'ready' && (
                  <Button onClick={fetchNow} disabled={busy}>
                    {t('config.fetchNow')}
                  </Button>
                )}
              </div>
            )}
            {error && (
              <p className="form-error" role="alert">
                {error}
              </p>
            )}
            {asked && <p className="form-status">{t('config.asked')}</p>}
          </div>

          <p className="form-status">
            <span className={`cfgb-readiness cfgb-readiness-${v.readiness}`}>
              {t(`config.readiness.${v.readiness}`)}
            </span>
            {v.readiness === 'no_credential' && canCreds && (
              <>
                {' '}
                <ScreenLink to="/nodes/credentials" />
              </>
            )}
          </p>
          {status && (
            <p className="form-status">
              <span className={`cfgb-result cfgb-result-${resultTone(status.status)}`}>
                {t('config.lastResult', { status: status.status })}
              </span>
              {status.last_end && <> · {relativeTime(status.last_end)}</>}
              {isStale(status.observed_at, Date.now()) && <> · {t('config.stale')}</>}
            </p>
          )}

          {v.available && (
            <DataTable
              tableId="node.configVersions"
              rows={versions.data}
              columns={columns}
              rowKey={(r) => r.oid}
              loading={versions.loading}
              empty={versions.error ?? <EmptyState text={t('config.noVersions')} />}
            />
          )}
          {shown && canConfig && (
            <ConfigViewer nodeId={node.id} shown={shown} onClose={() => setShown(null)} />
          )}
        </div>
      )}
    </LoadGate>
  );
}

/** One version's text, or two versions side by side, read when it is opened. */
function ConfigViewer({
  nodeId,
  shown,
  onClose,
}: {
  nodeId: string;
  shown: Shown;
  onClose: () => void;
}) {
  const { t } = useTranslation('nodes');
  const key = shown.kind === 'text' ? shown.oid : `${shown.from}:${shown.to}`;
  const loaded = useLoad<string | ConfigDiff | null>(
    () =>
      shown.kind === 'text'
        ? api.getConfigVersion(nodeId, shown.oid).then((r) => r.text)
        : api.diffConfigVersions(nodeId, shown.from, shown.to),
    [nodeId, key],
    { initial: null, errorFallback: t('config.err.text') },
  );
  const data = loaded.data;
  return (
    <div className="cfgb-viewer">
      <div className="cfgb-viewer-head">
        <span className="mono">
          {shown.kind === 'text'
            ? shortOid(shown.oid)
            : `${shortOid(shown.from)} → ${shortOid(shown.to)}`}
        </span>
        <Button variant="outline" onClick={onClose}>
          {t('common:actions.close')}
        </Button>
      </div>
      {loaded.error && (
        <p className="form-error" role="alert">
          {loaded.error}
        </p>
      )}
      {typeof data === 'string' && <pre className="cfgb-text mono">{data}</pre>}
      {data && typeof data !== 'string' && (
        <pre className="cfgb-text mono">
          {data.lines.map((l, i) => (
            <div key={i} className={`cfgb-line cfgb-line-${l.kind}`}>
              {l.kind === 'added' ? '+ ' : l.kind === 'removed' ? '- ' : '  '}
              {l.text}
            </div>
          ))}
        </pre>
      )}
    </div>
  );
}
