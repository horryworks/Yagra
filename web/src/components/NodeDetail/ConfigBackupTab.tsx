// SPDX-License-Identifier: AGPL-3.0-only
// NodeDetail ▸ Config (ADR-210 stages 2 and 3): the node's configuration as the bundled oxidized
// fetches it. One line says where the backup stands; Start (save and fetch once) or Stop and Fetch
// now act on it; the settings are the connection, the model and the order the CLI logins are tried
// in; below them the latest login attempts, the stored versions, a version's text and two versions
// side by side.
//
// The text and the comparison need `manage_config` (the server's `remove_secret` is model by model
// and can miss one), so a Viewer sees the state, the attempts and the versions and nothing of the
// text. Switching the backup on hands the logins to the device, so Start and Save also need
// `reveal_credentials`; logins are named only for a caller who may list credentials. The judgement
// — the form, the state line, how often to read again — is `configBackupForm.ts`.

import { useEffect, useMemo, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { api, ApiError, errMsg } from '../../services/api';
import { useCan } from '../../store';
import { useLoad } from '../../lib/useLoad';
import { LoadGate } from '../ui/LoadGate';
import { Button } from '../ui/Button';
import { Select } from '../ui/Field';
import { DataTable, type Column } from '../ui/DataTable';
import { EmptyState } from '../ui/EmptyState';
import { ScreenLink } from '../ui/ScreenLink';
import { CredentialPicker } from '../ui/CredentialPicker';
import { TimeCell } from '../ui/tableCells';
import { formatExactTime } from '../../lib/format';
import { isCliCredentialKind } from '../../lib/credentialKinds';
import {
  BACKUP_INPUTS,
  type AttemptView,
  type BackupInput,
  type ConfigBackupView,
  type ConfigDiff,
  type ConfigVersion,
  type NodeDetail,
} from '../../types/api';
import {
  attemptLine,
  bodyOf,
  formOf,
  isDirty,
  isQuietFetchRefusal,
  loginLabel,
  pickedLogins,
  previousOf,
  refreshMs,
  shortOid,
  stateLine,
  type ConfigBackupForm,
} from './configBackupForm';
import './ConfigBackupTab.css';

/** What the viewer pane shows: one version's text, or a version beside the one before it. */
type Shown = { kind: 'text'; oid: string } | { kind: 'diff'; from: string; to: string };

export function ConfigBackupTab({ node }: { node: NodeDetail }) {
  const { t } = useTranslation('nodes');
  const canConfig = useCan('manage_config');
  const canCreds = useCan('manage_credentials');
  const canReveal = useCan('reveal_credentials');
  const [poll, setPoll] = useState<number | undefined>(undefined);
  const view = useLoad(() => api.getConfigBackup(node.id), [node.id], {
    initial: null as ConfigBackupView | null,
    intervalMs: poll,
    errorFallback: t('config.err.load'),
  });
  useEffect(() => setPoll(refreshMs(view.data)), [view.data]);
  const available = view.data?.available ?? false;
  const confirmed = view.data?.login_confirmed ?? false;
  const versions = useLoad(
    () => api.listConfigVersions(node.id),
    [node.id, available, confirmed, view.data?.attempts[0]?.at],
    {
      initial: [] as ConfigVersion[],
      enabled: available,
      errorFallback: t('config.err.versions'),
    },
  );
  const creds = useLoad(() => api.listCredentials(), [], {
    initial: [],
    enabled: canCreds,
  });
  const logins = useMemo(() => creds.data.filter((c) => isCliCredentialKind(c.kind)), [creds.data]);
  const names = useMemo(() => new Map(logins.map((c) => [c.id, c.name])), [logins]);

  const [form, setForm] = useState<ConfigBackupForm | null>(null);
  useEffect(() => {
    if (view.data) setForm(formOf(view.data));
  }, [view.data]);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [shown, setShown] = useState<Shown | null>(null);

  const candidates = view.data?.candidates;
  const loginText = useMemo(
    () => (id: string | null | undefined) => {
      const l = loginLabel(id, names, candidates ?? []);
      if (l.kind === 'name') return l.name;
      if (l.kind === 'index') return t('config.loginIndex', { n: l.index });
      return t('config.loginGone');
    },
    [names, candidates, t],
  );

  /** Run one change, then read the view again whatever happened. */
  const act = (work: () => Promise<unknown>, fallback: string) => {
    setBusy(true);
    setError(null);
    work()
      .catch((e: unknown) => setError(errMsg(e, fallback)))
      .finally(() => {
        setBusy(false);
        view.reload();
      });
  };
  /** Fetch now. A fetch the limits hold back, or one already out, is not an error: the state line
   *  says until when. */
  const fetchQuietly = () =>
    api.fetchConfigNow(node.id).catch((e: unknown) => {
      if (!(e instanceof ApiError && isQuietFetchRefusal(e.status, e.code))) throw e;
    });
  const start = () => {
    if (!form) return;
    act(async () => {
      await api.putConfigBackup(node.id, bodyOf(form, true));
      await fetchQuietly();
    }, t('config.err.save'));
  };
  const stop = () => {
    if (!form) return;
    act(() => api.putConfigBackup(node.id, bodyOf(form, false)), t('config.err.save'));
  };
  const save = () => {
    if (!form || !view.data) return;
    act(() => api.putConfigBackup(node.id, bodyOf(form, view.data?.enabled ?? false)), t('config.err.save'));
  };
  const fetchNow = () => act(fetchQuietly, t('config.err.fetch'));

  const attemptColumns = useMemo<Column<AttemptView>[]>(
    () => [
      {
        key: 'at',
        header: t('config.attempts.cols.at'),
        width: '200px',
        render: (a) => <TimeCell iso={a.at} />,
      },
      {
        key: 'login',
        header: t('config.attempts.cols.login'),
        width: '180px',
        render: (a) => loginText(a.credential_id),
      },
      {
        key: 'result',
        header: t('config.attempts.cols.result'),
        width: '1fr',
        render: (a) => {
          const line = attemptLine(a);
          return (
            <span className={`cfgb-tone-${line.tone}`}>
              {t(line.key, { login: loginText(line.login) })}
            </span>
          );
        },
      },
    ],
    [t, loginText],
  );

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
  const line = v ? stateLine(v) : null;
  const canStart = canConfig && canReveal;
  return (
    <LoadGate load={view}>
      {view.error && (
        <p className="form-error" role="alert">
          {view.error}
        </p>
      )}
      {v && form && line && (
        <div className="cfgb">
          {!v.available && <p className="form-status">{t('config.unavailable')}</p>}
          <div className="cfgb-head">
            <p className={`cfgb-state cfgb-tone-${line.tone}`} role="status">
              {t(line.key, { ...line.params, login: loginText(line.login) })}
              {v.try_after && <> · {t('config.state.nextTry', { time: formatExactTime(v.try_after) })}</>}
              {v.readiness === 'no_credential' && canCreds && (
                <>
                  {' '}
                  <ScreenLink to="/nodes/credentials" />
                </>
              )}
            </p>
            {canConfig && v.available && (
              <div className="cfgb-buttons">
                {!v.enabled && canStart && (
                  <Button variant="primary" onClick={start} disabled={busy}>
                    {t('config.start')}
                  </Button>
                )}
                {v.enabled && v.readiness === 'ready' && !v.pending && (
                  <Button onClick={fetchNow} disabled={busy}>
                    {t('config.fetchNow')}
                  </Button>
                )}
                {v.enabled && (
                  <Button variant="outline" onClick={stop} disabled={busy}>
                    {t('config.stop')}
                  </Button>
                )}
              </div>
            )}
          </div>
          {error && (
            <p className="form-error" role="alert">
              {error}
            </p>
          )}

          <div className="cfgb-settings">
            <div className="modal-field">
              <label className="modal-field-label" htmlFor="cfgb-input">
                {t('config.connection')}
              </label>
              <Select
                id="cfgb-input"
                value={form.input}
                disabled={!canStart}
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
                disabled={!canStart}
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
          </div>
          {canStart && canCreds && (
            <div className="cfgb-logins">
              <div className="cfgb-logins-head">
                <span className="modal-field-label">{t('config.logins')}</span>
                {form.order !== null && (
                  <Button variant="outline" onClick={() => setForm({ ...form, order: null })}>
                    {t('config.defaultOrder')}
                  </Button>
                )}
              </div>
              <CredentialPicker
                options={logins}
                selected={pickedLogins(form, v)}
                onChange={(ids) => setForm({ ...form, order: ids })}
                disabled={busy}
              />
            </div>
          )}
          {canStart && v.enabled && (
            <div className="cfgb-buttons">
              <Button variant="primary" onClick={save} disabled={busy || !isDirty(form, v)}>
                {t('common:actions.save')}
              </Button>
            </div>
          )}

          {v.attempts.length > 0 && (
            <DataTable
              tableId="node.configAttempts"
              rows={v.attempts}
              columns={attemptColumns}
              rowKey={(a) => `${a.at}:${a.outcome}`}
            />
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
