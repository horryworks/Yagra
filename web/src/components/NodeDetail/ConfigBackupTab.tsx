// SPDX-License-Identifier: AGPL-3.0-only
// NodeDetail ▸ Config (ADR-210 stages 2 and 3, ADR-240): the node's configuration as the bundled
// oxidized fetches it. A status card at the top says whether the backup works, and when it does not,
// offers the one thing that fixes it; the settings are folded into one line; the stored versions,
// the saves to startup and the failed fetches are one history, newest first.
//
// The text and the comparison need `manage_config` (the server's `remove_secret` is model by model
// and can miss one), so a Viewer sees the card, the settings line and the history and nothing of
// the text. Switching the backup on hands the logins to the device, so the switch and the settings'
// Save also need `manage_credentials` (ADR-210 stage 8); logins are named only for a caller who may
// list credentials. The judgement - the card, the history, the form - is `configBackupForm.ts`.
//
// "Save to startup" (ADR-222) is the one control here that writes to the device: it takes
// `save_device_config` (Admin), confirms, and sits on the line that says the device is unsaved.

import { useEffect, useMemo, useState, type ReactNode } from 'react';
import { useTranslation } from 'react-i18next';
import { Link } from 'react-router-dom';
import { api, ApiError, errMsg } from '../../services/api';
import { useCan } from '../../store';
import { useLoad } from '../../lib/useLoad';
import { LoadGate } from '../ui/LoadGate';
import { Button } from '../ui/Button';
import { Segmented } from '../ui/Segmented';
import { Field, Select } from '../ui/Field';
import { CheckField, FieldGroup } from '../ui/FormLayout';
import { EmptyState } from '../ui/EmptyState';
import { Inline, Stack } from '../ui/Page';
import { Callout, Disclosure, SectionHead } from '../ui/Section';
import { ScreenLink } from '../ui/ScreenLink';
import { CredentialPicker } from '../ui/CredentialPicker';
import { ConfirmDeleteModal } from '../ui/ConfirmDeleteModal';
import { formatExactTime, relativeTime } from '../../lib/format';
import { isCliCredentialKind } from '../../lib/credentialKinds';
import {
  SCAN_MODES,
  type ConfigBackupView,
  type ConfigDiff,
  type ConfigVersion,
  type NodeDetail,
  type ScanMode,
} from '../../types/api';
import {
  HISTORY_FILTERS,
  attemptLine,
  bodyOf,
  canOfferSave,
  canStartWith,
  cardOf,
  formOf,
  historyRows,
  loginMark,
  openingFormOf,
  sshOnly,
  withSshOnly,
  isDirty,
  isQuietFetchRefusal,
  lastSuccessAt,
  loginLabel,
  pickedLogins,
  refreshMs,
  saveLine,
  shortOid,
  type CardFix,
  type ConfigBackupForm,
  type HistoryFilter,
  type HistoryRow,
} from './configBackupForm';
import { compareColumns, compareRows } from './compareOutput';
import './ConfigBackupTab.css';

/** What the viewer under a history row shows: one version's text, a version beside the one before
 *  it, or the startup configuration a save replaced. */
type Shown =
  | { kind: 'text'; oid: string }
  | { kind: 'diff'; from: string; to: string }
  | { kind: 'startup'; save: string };

const CARD_MARK = { off: '–', busy: '…', ok: '✓', fix: '!' } as const;

export function ConfigBackupTab({ node }: { node: NodeDetail }) {
  const { t } = useTranslation('nodes');
  const canConfig = useCan('manage_config');
  const canCreds = useCan('manage_credentials');
  const canSave = useCan('save_device_config');
  const [confirmSave, setConfirmSave] = useState(false);
  const [poll, setPoll] = useState<number | undefined>(undefined);
  const view = useLoad(() => api.getConfigBackup(node.id), [node.id], {
    initial: null as ConfigBackupView | null,
    intervalMs: poll,
    errorFallback: t('config.err.load'),
  });
  useEffect(() => setPoll(refreshMs(view.data)), [view.data]);
  const available = view.data?.available ?? false;
  const enabled = view.data?.enabled ?? false;
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
  // When the next daily run starts. Its failure is not worth a line: the card simply leaves it out.
  const schedule = useLoad(() => api.getConfigBackupSchedule(), [enabled], {
    initial: null,
    enabled: available && enabled,
  });
  const creds = useLoad(() => api.listCredentials(), [], {
    initial: [],
    enabled: canCreds,
  });
  const logins = useMemo(() => creds.data.filter((c) => isCliCredentialKind(c.kind)), [creds.data]);
  const names = useMemo(() => new Map(logins.map((c) => [c.id, c.name])), [logins]);

  const [form, setForm] = useState<ConfigBackupForm | null>(null);
  useEffect(() => {
    if (view.data) setForm(openingFormOf(view.data));
  }, [view.data]);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [shown, setShown] = useState<Shown | null>(null);
  const [filter, setFilter] = useState<HistoryFilter>('changes');
  // Null until a person folds or opens it: then the settings are open while the backup is off or
  // still looking for a login, and folded once one works (ADR-240 Inc.2 decision 10).
  const [settingsOpen, setSettingsOpen] = useState<boolean | null>(null);
  const [pickedModel, setPickedModel] = useState('');
  // Which unsaved spell the comparison was opened for: a node saved and then changed again starts
  // a new one, so the panel does not reopen on its own.
  const [diffOpenFor, setDiffOpenFor] = useState<string | null>(null);

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
  /** Fetch now. A fetch the limits hold back, or one already out, is not an error: the card says
   *  until when. */
  const fetchQuietly = () =>
    api.fetchConfigNow(node.id).catch((e: unknown) => {
      if (!(e instanceof ApiError && isQuietFetchRefusal(e.status, e.code))) throw e;
    });
  /** Switch on with `f` and ask for the first fetch. */
  const start = (f: ConfigBackupForm) =>
    act(async () => {
      await api.putConfigBackup(node.id, bodyOf(f, true));
      await fetchQuietly();
    }, t('config.err.save'));
  /** Switch off with the settings as stored: an unsaved edit in the form is not saved by stopping. */
  const stop = () => {
    const stored = view.data;
    if (stored) act(() => api.putConfigBackup(node.id, bodyOf(formOf(stored), false)), t('config.err.save'));
  };
  const save = () => {
    if (form) act(() => api.putConfigBackup(node.id, bodyOf(form, enabled)), t('config.err.save'));
  };
  const fetchNow = () => act(fetchQuietly, t('config.err.fetch'));
  const cancelFetch = () => act(() => api.cancelConfigFetch(node.id), t('config.err.cancel'));
  const rescan = () => act(() => api.rescanConfigLogins(node.id), t('config.err.rescan'));

  const v = view.data;
  const vers = versions.data;
  const lastOk = useMemo(() => (v ? lastSuccessAt(v, vers) : null), [v, vers]);
  const card = useMemo(() => (v ? cardOf(v) : null), [v]);
  const rows = useMemo(() => (v ? historyRows(vers, v.saves, v.attempts, filter) : []), [v, vers, filter]);
  const canStart = canConfig && canCreds;

  /** The control behind one of the card's fixes, or nothing for a caller who may not use it. */
  const fixControl = (fix: CardFix): ReactNode => {
    if (!v || !form) return null;
    switch (fix) {
      case 'pickModel':
        return canStart ? (
          <span key={fix} className="cfgb-fix-model">
            <Select
              id="cfgb-fix-model"
              aria-label={t('config.model')}
              value={pickedModel}
              onChange={(e) => setPickedModel(e.target.value)}
            >
              <option value="">{t('config.modelPick')}</option>
              {v.models.map((m) => (
                <option key={m} value={m}>
                  {m}
                </option>
              ))}
            </Select>
            <Button
              variant="primary"
              disabled={busy || pickedModel === ''}
              onClick={() => start({ ...form, model: pickedModel })}
            >
              {t('config.card.saveAndFetch')}
            </Button>
          </span>
        ) : null;
      case 'addLogin':
        return canCreds ? (
          <Link key={fix} className="btn btn-primary" to="/nodes/credentials?tab=cli">
            {t('config.addLogin')}
          </Link>
        ) : null;
      // Rescanning hands the logins out again, so it takes what switching on takes.
      case 'rescan':
        return canStart ? (
          <Button key={fix} onClick={rescan} disabled={busy}>
            {t('config.card.tryAgain')}
          </Button>
        ) : null;
      case 'fetch':
        return canConfig ? (
          <Button key={fix} variant="primary" onClick={fetchNow} disabled={busy}>
            {t('config.fetchNow')}
          </Button>
        ) : null;
      case 'settings':
        return (
          <Button key={fix} onClick={() => setSettingsOpen(true)}>
            {t('config.settings.title')}
          </Button>
        );
      case 'resave':
        return canStart ? (
          <Button key={fix} variant="primary" onClick={save} disabled={busy}>
            {t('config.card.saveAgain')}
          </Button>
        ) : null;
      case 'pollers':
        return <ScreenLink key={fix} to="/settings/pollers" />;
    }
  };

  /** Whether the viewer is open under this row. */
  const opensUnder = (r: HistoryRow): boolean => {
    if (!shown || !canConfig) return false;
    if (r.kind === 'version') {
      return (shown.kind === 'text' && shown.oid === r.oid) || (shown.kind === 'diff' && shown.to === r.oid);
    }
    return r.kind === 'save' && shown.kind === 'startup' && shown.save === r.save.id;
  };

  const historyRow = (r: HistoryRow, i: number): ReactNode => {
    const at = <span className="cfgb-h-at">{r.at ? formatExactTime(r.at) : ''}</span>;
    switch (r.kind) {
      case 'version':
        return (
          <li key={`v:${r.oid}`} className="cfgb-h-row">
            {at}
            <span className="cfgb-h-mark cfgb-h-version" aria-hidden="true" />
            <span className="cfgb-h-what">
              {t(r.prev ? 'config.history.version' : 'config.history.first')}{' '}
              <code className="mono cfgb-h-oid">{shortOid(r.oid)}</code>
            </span>
            <span className="cfgb-h-acts">
              {canConfig && (
                <>
                  <Button onClick={() => setShown({ kind: 'text', oid: r.oid })}>{t('config.view')}</Button>
                  {r.prev && (
                    <Button onClick={() => setShown({ kind: 'diff', from: r.prev ?? '', to: r.oid })}>
                      {t('config.compare')}
                    </Button>
                  )}
                </>
              )}
            </span>
          </li>
        );
      case 'save': {
        const line = saveLine(r.save);
        return (
          <li key={`s:${r.save.id}`} className="cfgb-h-row">
            {at}
            <span className="cfgb-h-mark cfgb-h-save" aria-hidden="true" />
            <span className="cfgb-h-what">
              {t('config.history.save', { who: t(`config.saves.origin.${r.save.origin}`) })}{' '}
              <span className={`cfgb-tone-${line.tone}`}>{t(line.key)}</span>
            </span>
            <span className="cfgb-h-acts">
              {canConfig && r.save.kept_startup && (
                <Button onClick={() => setShown({ kind: 'startup', save: r.save.id })}>
                  {t('config.saves.viewStartup')}
                </Button>
              )}
            </span>
          </li>
        );
      }
      case 'attempt': {
        const line = attemptLine(r.attempt);
        return (
          <li key={`a:${r.at}:${i}`} className="cfgb-h-row">
            {at}
            <span className={`cfgb-h-mark cfgb-h-${line.tone === 'bad' ? 'fail' : 'quiet'}`} aria-hidden="true" />
            <span className={`cfgb-h-what cfgb-tone-${line.tone}`}>
              {t(line.key, { login: loginText(line.login) })}
              {r.attempt.input && <span className="cfgb-h-proto">{t(`config.protocol.${r.attempt.input}`)}</span>}
            </span>
            <span className="cfgb-h-acts" />
          </li>
        );
      }
      case 'same':
        return (
          <li key={`n:${r.at}:${i}`} className="cfgb-h-row cfgb-h-same">
            {at}
            <span className="cfgb-h-mark cfgb-h-quiet" aria-hidden="true" />
            <span className="cfgb-h-what">
              {r.count > 1 ? t('config.history.sameN', { count: r.count }) : t('config.result.noChange')}
            </span>
            <span className="cfgb-h-acts" />
          </li>
        );
    }
  };

  return (
    <LoadGate load={view}>
      {view.error && (
        <p className="form-error" role="alert">
          {view.error}
        </p>
      )}
      {v && form && card && (
        <Stack gap={12} className="nd-tabpad">
          {!v.available && <p className="form-status">{t('config.unavailable')}</p>}
          {v.available && (
            <section className={`cfgb-card cfgb-card-${card.kind}`} aria-label={t('config.switch.label')}>
              <span className="cfgb-card-mark" aria-hidden="true">
                {CARD_MARK[card.kind]}
              </span>
              <div className="cfgb-card-text">
                <p className="cfgb-state" role="status">
                  {t(card.line.key, { ...card.line.params, login: loginText(card.line.login) })}
                </p>
                {v.enabled && (
                  <p className="cfgb-facts">
                    {lastOk && <span>{t('config.card.last', { ago: relativeTime(lastOk) })}</span>}
                    {v.try_after ? (
                      <span>{t('config.state.nextTry', { time: formatExactTime(v.try_after) })}</span>
                    ) : (
                      // A problem the run cannot fix (no model, every login refused) is not
                      // waiting for the next run, so the card does not promise one.
                      (card.kind !== 'fix' || card.fixes.includes('fetch')) &&
                      schedule.data?.upcoming_start_at && (
                        <span>{t('config.card.next', { time: formatExactTime(schedule.data.upcoming_start_at) })}</span>
                      )
                    )}
                    {vers.length > 0 && <span>{t('config.card.versions', { count: vers.length })}</span>}
                    {v.protocol && <span>{t(`config.protocol.${v.protocol}`)}</span>}
                  </p>
                )}
              </div>
              <div className="cfgb-card-right">
                {canConfig && v.enabled && v.pending && (
                  <Button onClick={cancelFetch} disabled={busy}>
                    {t('config.cancelFetch')}
                  </Button>
                )}
                {canConfig && card.kind === 'ok' && (
                  <Button variant="ghost" onClick={fetchNow} disabled={busy}>
                    {t('config.fetchNow')}
                  </Button>
                )}
                {/* Starting is the settings' own button, below them (ADR-240 Inc.2 decision 8);
                    stopping hands nothing out, so it takes what editing the node takes. */}
                {canConfig && v.enabled && (
                  <Button variant="ghost" onClick={stop} disabled={busy}>
                    {t('config.stop')}
                  </Button>
                )}
              </div>
              {card.kind === 'fix' && card.fixes.length > 0 && (
                <div className="cfgb-card-fixes">{card.fixes.map(fixControl)}</div>
              )}
            </section>
          )}
          {v.enabled && v.unsaved && (
            <Callout tone="warning">
              <Inline gap={12} className="cfgb-unsaved">
                <span className="cfgb-unsaved-t">
                  {t('config.unsaved.since', { time: formatExactTime(v.unsaved.since) })}
                </span>
                {canConfig && v.unsaved.has_output && (
                  <Button
                    aria-expanded={diffOpenFor === v.unsaved.since}
                    onClick={() => setDiffOpenFor((o) => (o === v.unsaved?.since ? null : (v.unsaved?.since ?? null)))}
                  >
                    {diffOpenFor === v.unsaved.since ? t('config.unsaved.hide') : t('config.unsaved.view')}
                  </Button>
                )}
                {canSave && canOfferSave(v) && (
                  <Button variant="primary" onClick={() => setConfirmSave(true)} disabled={busy}>
                    {t('config.saves.button')}
                  </Button>
                )}
              </Inline>
            </Callout>
          )}
          {v.enabled && v.unsaved && canConfig && v.unsaved.has_output && diffOpenFor === v.unsaved.since && (
            <UnsavedDiff nodeId={node.id} checkedAt={v.unsaved.checked_at} />
          )}
          {error && (
            <p className="form-error" role="alert">
              {error}
            </p>
          )}

          {v.available && (
            <Disclosure
              className="cfgb-settings"
              open={settingsOpen ?? (!v.enabled || !v.login_confirmed)}
              onToggle={setSettingsOpen}
              summary={
                <>
                  <span className="cfgb-settings-t">{t('config.settings.title')}</span>
                  <span className="cfgb-settings-sum">
                    {[
                      t(`config.input.${form.input}`),
                      v.model_override ?? v.profile_model ?? t('config.settings.noModel'),
                      v.login_confirmed && v.credential_id ? loginText(v.credential_id) : null,
                    ]
                      .filter(Boolean)
                      .join(' · ')}
                  </span>
                </>
              }
            >
              <div className="cfgb-settings-grid">
                <Field label={t('config.model')}>
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
                </Field>
                <Field label={t('config.scanMode')}>
                  <Select
                    id="cfgb-speed"
                    value={form.scanMode}
                    disabled={!canStart}
                    onChange={(e) => setForm({ ...form, scanMode: e.target.value as ScanMode })}
                  >
                    {SCAN_MODES.map((m) => (
                      <option key={m} value={m}>
                        {t(`config.scanModes.${m}`)}
                      </option>
                    ))}
                  </Select>
                </Field>
              </div>
              <CheckField
                checked={sshOnly(form)}
                disabled={!canStart}
                onChange={(on) => setForm(withSshOnly(form, on))}
                label={t('config.input.ssh')}
              />
              {canStart && (
                <div className="cfgb-logins">
                  <FieldGroup
                    label={t('config.logins')}
                    action={
                      // `null` is every CLI login in the try order, which also takes in one added later.
                      form.order !== null && (
                        <Button onClick={() => setForm({ ...form, order: null })}>{t('config.selectAll')}</Button>
                      )
                    }
                  >
                    <CredentialPicker
                      options={logins}
                      selected={pickedLogins(form, v)}
                      onChange={(ids) => setForm({ ...form, order: ids })}
                      disabled={busy}
                      emptyText={t('config.noLogins')}
                      rowStatus={(id) => {
                        const m = loginMark(v, id);
                        return m && <span className={`cfgb-tone-${m.tone}`}>{t(m.key)}</span>;
                      }}
                    />
                    <Link className="cfgb-add-login" to="/nodes/credentials?tab=cli">
                      {t('config.addLogin')}
                    </Link>
                  </FieldGroup>
                </div>
              )}
              {/* Under the settings, at the left, the main button first: read top to bottom and
                  press at the end (ADR-240 decision 14), as Discovery's Scan (ADR-244 Inc.2). */}
              {canStart && !v.enabled && (
                <Inline gap={8}>
                  <Button variant="primary" onClick={() => start(form)} disabled={busy || !canStartWith(form, v)}>
                    {t('config.start')}
                  </Button>
                  <p className="form-status cfgb-picked">
                    {t('config.picked', { count: pickedLogins(form, v).length })}
                  </p>
                </Inline>
              )}
              {canStart && v.enabled && (
                <Inline gap={8}>
                  <Button
                    variant="primary"
                    onClick={save}
                    disabled={busy || !isDirty(form, v) || !canStartWith(form, v)}
                  >
                    {t('common:actions.save')}
                  </Button>
                  <Button onClick={rescan} disabled={busy}>
                    {t('config.rescan')}
                  </Button>
                </Inline>
              )}
            </Disclosure>
          )}

          {v.available && (v.enabled || vers.length > 0) && (
            <Stack as="section" gap={8}>
              <SectionHead
                title={t('config.history.title')}
                actions={
                  <Segmented
                    ariaLabel={t('config.history.title')}
                    options={HISTORY_FILTERS.map((f) => ({ value: f, label: t(`config.history.filter.${f}`) }))}
                    value={filter}
                    onChange={(f) => setFilter(f as HistoryFilter)}
                  />
                }
              />
              {versions.error && (
                <p className="form-error" role="alert">
                  {versions.error}
                </p>
              )}
              {rows.length === 0 ? (
                <EmptyState text={t('config.noVersions')} />
              ) : (
                <ol className="cfgb-h">
                  {rows.map((r, i) => [
                    historyRow(r, i),
                    shown && opensUnder(r) ? (
                      <li key={`viewer:${i}`} className="cfgb-h-viewer">
                        <ConfigViewer nodeId={node.id} shown={shown} onClose={() => setShown(null)} />
                      </li>
                    ) : null,
                  ])}
                </ol>
              )}
            </Stack>
          )}
          {confirmSave && (
            <ConfirmDeleteModal
              title={t('config.saves.confirmTitle')}
              confirmLabel={t('config.saves.confirmLabel')}
              errorFallback={t('config.saves.err')}
              onConfirm={() => api.saveConfigNow(node.id)}
              onClose={() => setConfirmSave(false)}
              onDone={() => {
                setConfirmSave(false);
                view.reload();
              }}
            >
              {t('config.saves.confirm', { node: node.name })}
            </ConfirmDeleteModal>
          )}
        </Stack>
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
  const key =
    shown.kind === 'text'
      ? shown.oid
      : shown.kind === 'startup'
        ? `startup:${shown.save}`
        : `${shown.from}:${shown.to}`;
  const loaded = useLoad<string | ConfigDiff | null>(
    () =>
      shown.kind === 'text'
        ? api.getConfigVersion(nodeId, shown.oid).then((r) => r.text)
        : shown.kind === 'startup'
          ? api.getConfigSaveStartup(nodeId, shown.save).then((r) => r.text)
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
            : shown.kind === 'startup'
              ? t('config.saves.viewStartup')
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

/** How running differed from startup the last time the device compared them, opened right under
 *  the line that says so (ADR-222 Inc.9): the two sides side by side, a changed row marked. Output
 *  in any other shape is shown as it came. Read again when the script compares again. */
function UnsavedDiff({ nodeId, checkedAt }: { nodeId: string; checkedAt: string }) {
  const { t } = useTranslation('nodes');
  const loaded = useLoad(() => api.getConfigUnsaved(nodeId).then((r) => r.text), [nodeId, checkedAt], {
    initial: null as string | null,
    errorFallback: t('config.err.text'),
  });
  const columns = useMemo(() => (loaded.data === null ? null : compareColumns(loaded.data)), [loaded.data]);
  const rows = useMemo(() => (columns ? compareRows(columns) : []), [columns]);
  return (
    <div className="cfgb-udiff">
      {loaded.error && (
        <p className="form-error" role="alert">
          {loaded.error}
        </p>
      )}
      {loaded.data !== null && !columns && <pre className="cfgb-text mono">{loaded.data}</pre>}
      {columns && (
        <div className="cfgb-udiff-grid mono" role="table">
          <div className="cfgb-udiff-head" role="row">
            <span role="columnheader">{t('config.unsaved.running', { n: columns.running.line })}</span>
            <span role="columnheader">{t('config.unsaved.saved', { n: columns.saved.line })}</span>
          </div>
          {rows.map((r, i) => (
            <div key={i} role="row" className={`cfgb-udiff-row${r.differs ? ' cfgb-udiff-differs' : ''}`}>
              <span role="cell" className={r.differs && r.running !== undefined ? 'cfgb-line-added' : ''}>
                {r.running ?? ''}
              </span>
              <span role="cell" className={r.differs && r.saved !== undefined ? 'cfgb-line-removed' : ''}>
                {r.saved ?? ''}
              </span>
            </div>
          ))}
        </div>
      )}
    </div>
  );
}
