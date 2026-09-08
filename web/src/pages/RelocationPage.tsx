// SPDX-License-Identifier: AGPL-3.0-only
// Settings ▸ Move to another server (ADR-121). Copy this whole deployment — keys included — onto a
// fresh Linux host, over SSH, in one press.
//
// 🚨 **The artefact this page makes is every secret this deployment holds.** The KEK and the
// database travel together, so anything that can read the archive can read every SNMP community,
// every device login and every API token. `security.md` forbids returning secrets from the API and
// names this as its one deliberate exception; the page's job is to make the operator aware of that
// rather than to hide it behind a friendly button.
//
// Its own screen rather than a card on Settings ▸ Upgrade, for the reason the support bundle got
// its own (ADR-055 R8): the upgrade page is about *this* deployment's version, and this is a
// write-out action with a different permission set and a different hazard.
//
// Admin-only in the UI because it is Admin-only in the API — three permissions plus `Admin` and
// `Leader`. No `useCan` on the buttons: a caller who cannot read the status gets `LoadBlockNotice`
// instead of the whole screen, so no control is ever mounted for someone who could not press it
// (ADR-056 decision 2).
//
// The judgement lives in `relocationStatus.ts` so it can be unit-tested — Vitest never runs a .tsx
// (testing.md). What is left here is layout.

import { useCallback, useEffect, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { PageHeader } from '../components/ui/PageHeader';
import { LoadBlockNotice } from '../components/ui/LoadBlockNotice';
import { classifyLoadError, type LoadBlock } from '../lib/loadState';
import { Card } from '../components/ui/Card';
import { Button } from '../components/ui/Button';
import { Segmented } from '../components/ui/Segmented';
import { ProgressBar } from '../components/ui/ProgressBar';
import { ConfirmDeleteModal } from '../components/ui/ConfirmDeleteModal';
import { api, errMsg } from '../services/api';
import { saveBlob } from '../lib/download';
import { formatBytes, formatTimestamp } from '../lib/format';
import type { RelocationStatus } from '../types/api';
import type { RelocationAuthKind, RunPhase, TargetForm } from './relocationStatus';
import {
  RELOCATION_AUTH_KINDS,
  canStart,
  readiness,
  relocationStage,
  roomForArchive,
  runPhase,
  shouldPoll,
  stageProgress,
  targetCommands,
  validateTarget,
} from './relocationStatus';
import './RelocationPage.css';

/** How often to re-read while a relocation is in flight.
 *
 *  Slower than the Upgrade page's two seconds, because this runs for minutes rather than one: a
 *  full database dump, a metrics snapshot, a compress and a transfer. Nothing here restarts core,
 *  so — unlike an upgrade — these requests are expected to succeed throughout. */
const POLL_MS = 3_000;

/** Give up on a run that never reports. Deliberately long: the archive can be gigabytes and the
 *  transfer is over whatever link the two hosts share. Dropping `pending` returns the page to
 *  whatever the server last said, which is the honest thing to show. */
const POLL_CEILING_MS = 60 * 60_000;

/** Lines of the run's log to show. Enough to hold the restore's own output on the far side. */
const LOG_TAIL = 200;

export function RelocationPage() {
  const { t } = useTranslation('settings-relocation');
  const [status, setStatus] = useState<RelocationStatus | null>(null);
  const [log, setLog] = useState<string[]>([]);
  const [block, setBlock] = useState<LoadBlock | null>(null);
  const [failed, setFailed] = useState(false);
  const [pending, setPending] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const load = useCallback(async () => {
    try {
      const s = await api.getRelocation();
      setStatus(s);
      setFailed(false);
      if (s.run) {
        const l = await api.getRelocationLog(LOG_TAIL).catch(() => null);
        if (l) setLog(l.lines);
      }
    } catch (e: unknown) {
      const b = classifyLoadError(e);
      if (b) setBlock(b);
      else setFailed(true);
    }
  }, []);

  useEffect(() => {
    void load();
  }, [load]);

  // Armed by the act of starting, not by what the server currently says — the inversion that was
  // the bug on the Upgrade page (`relocationStatus.ts::shouldPoll`).
  const polling = status ? shouldPoll(status, pending) : false;
  useEffect(() => {
    if (!polling) return undefined;
    const started = Date.now();
    const h = window.setInterval(() => {
      if (Date.now() - started > POLL_CEILING_MS) setPending(null);
      else void load();
    }, POLL_MS);
    return () => window.clearInterval(h);
  }, [polling, load]);

  if (block)
    return (
      <LoadBlockNotice
        block={block}
        unavailable={t("unavailable")}
        permission="manage_system"
      />
    );
  if (failed && !status) {
    return (
      <div>
        <Header />
        <Card>
          <p className="form-error">{t('loadFailed')}</p>
        </Card>
      </div>
    );
  }
  if (!status) return <Header />;

  const state = readiness(status);
  const phase = runPhase(status, pending);

  const start = async (
    mode: 'archive' | 'preflight' | 'push',
    options: Options,
    target?: TargetForm & { sudo: string },
  ) => {
    setBusy(true);
    setError(null);
    try {
      const { id } = await api.startRelocation({
        mode,
        include_metrics: options.metrics,
        include_tier2: options.tier2,
        include_images: options.images,
        install_docker: options.installDocker,
        ...(target
          ? {
              target: {
                host: target.host.trim(),
                port: Number(target.port),
                user: target.user.trim(),
                dir: target.dir.trim(),
              },
              auth: {
                kind: target.kind,
                secret: target.secret,
                ...(target.sudo ? { sudo_password: target.sudo } : {}),
              },
            }
          : {}),
      });
      setPending(id);
      setLog([]);
      await load();
    } catch (e: unknown) {
      setError(errMsg(e, t('err.start')));
    } finally {
      setBusy(false);
    }
  };

  return (
    <div>
      <Header />
      <WarningCard />
      <CarriedCard />
      <ReadinessCard status={status} state={state} />
      {state === 'ready' && (
        <RelocateForm
          status={status}
          phase={phase}
          busy={busy}
          error={error}
          onStart={start}
        />
      )}
      {phase.kind !== 'idle' && <ProgressCard phase={phase} log={log} />}
      <ManualCard
        status={status}
        busy={busy}
        disabled={!canStart(status, pending)}
        onCreate={(o) => void start('archive', o)}
        onChanged={() => void load()}
      />
    </div>
  );
}

function Header() {
  const { t } = useTranslation('settings-relocation');
  return (
    <PageHeader
      title={t('title')}
      trail={[{ label: t('nav:sections.settings') }, { label: t('title') }]}
      note={t('subtitle')}
    />
  );
}

/** The three hazards, stated before anything else on the page. */
function WarningCard() {
  const { t } = useTranslation('settings-relocation');
  return (
    <Card className="reloc-warning">
      <ul>
        <li>{t('warning.secrets')}</li>
        <li>{t('warning.ssh')}</li>
        <li>{t('warning.root')}</li>
      </ul>
    </Card>
  );
}

/** What travels, what does not, and what has to be decided again on the new host. Three columns,
 *  because "it copies everything" is the belief this page has to correct before it is acted on. */
function CarriedCard() {
  const { t } = useTranslation('settings-relocation');
  const list = (key: string, n: number) =>
    Array.from({ length: n }, (_, i) => t(`${key}.${i + 1}`));
  return (
    <Card title={t('carried.heading')}>
      <div className="reloc-carried">
        <section>
          <h4>{t('carried.yes')}</h4>
          <ul>
            {list('carried.yesItem', 6).map((s) => (
              <li key={s}>{s}</li>
            ))}
          </ul>
        </section>
        <section>
          <h4>{t('carried.no')}</h4>
          <ul>
            {list('carried.noItem', 3).map((s) => (
              <li key={s}>{s}</li>
            ))}
          </ul>
        </section>
        <section>
          <h4>{t('carried.again')}</h4>
          <ul>
            {list('carried.againItem', 5).map((s) => (
              <li key={s}>{s}</li>
            ))}
          </ul>
        </section>
      </div>
    </Card>
  );
}

/** Why the page can or cannot offer to move anything, plus the space numbers. */
function ReadinessCard({
  status,
  state,
}: {
  status: RelocationStatus;
  state: ReturnType<typeof readiness>;
}) {
  const { t } = useTranslation('settings-relocation');
  const room = roomForArchive(status);
  const size = (n: number | null | undefined) =>
    typeof n === 'number' ? formatBytes(n) : t('space.unknown');
  return (
    <Card title={t('mechanism.heading')}>
      <p className={state === 'ready' ? 'muted' : 'form-error'}>{t(`readiness.${state}`)}</p>
      <dl className="reloc-space">
        <dt>{t('space.free')}</dt>
        <dd>{size(status.free_bytes)}</dd>
        <dt>{t('space.estimate')}</dt>
        <dd>{size(status.estimate_bytes)}</dd>
        <dt>{t('space.needed')}</dt>
        <dd>{size(status.needed_bytes)}</dd>
      </dl>
      <p className="muted">{t('space.partial')}</p>
      {room === false && <p className="form-error">{t('space.tight')}</p>}
    </Card>
  );
}

/** The four options every mode shares. */
export interface Options {
  metrics: boolean;
  tier2: boolean;
  images: boolean;
  installDocker: boolean;
}

const DEFAULT_OPTIONS: Options = {
  metrics: true,
  tier2: true,
  images: false,
  installDocker: true,
};

function OptionRows({
  value,
  onChange,
  disabled,
}: {
  value: Options;
  onChange: (o: Options) => void;
  disabled: boolean;
}) {
  const { t } = useTranslation('settings-relocation');
  const row = (key: keyof Options, name: string) => (
    <label className="reloc-opt" key={name}>
      <input
        type="checkbox"
        checked={value[key]}
        disabled={disabled}
        onChange={(e) => onChange({ ...value, [key]: e.target.checked })}
      />
      <span>
        <strong>{t(`options.${name}`)}</strong>
        <em className="muted">{t(`options.${name}Hint`)}</em>
      </span>
    </label>
  );
  return (
    <div className="reloc-opts">
      {row('metrics', 'metrics')}
      {row('tier2', 'tier2')}
      {row('images', 'images')}
      {row('installDocker', 'installDocker')}
    </div>
  );
}

const EMPTY_TARGET: TargetForm & { sudo: string } = {
  host: '',
  port: '22',
  user: '',
  dir: 'yagra',
  kind: 'password',
  secret: '',
  sudo: '',
};

/** The main path: type where it is going, press once. */
function RelocateForm({
  status,
  phase,
  busy,
  error,
  onStart,
}: {
  status: RelocationStatus;
  phase: RunPhase;
  busy: boolean;
  error: string | null;
  onStart: (
    mode: 'archive' | 'preflight' | 'push',
    options: Options,
    target?: TargetForm & { sudo: string },
  ) => void;
}) {
  const { t } = useTranslation('settings-relocation');
  const [options, setOptions] = useState<Options>(DEFAULT_OPTIONS);
  const [form, setForm] = useState(EMPTY_TARGET);
  const problems = validateTarget(form);
  const running = phase.kind === 'starting' || phase.kind === 'running';
  const locked = busy || running || !status.enabled;

  // The credentials live in this component's state and nowhere else — not in the store, not in the
  // URL, not in `localStorage`. When a push finishes they go, because the next press is a new
  // decision and a password sitting in a mounted form is one an unattended browser still holds.
  useEffect(() => {
    if (phase.kind === 'done') setForm((f) => ({ ...f, secret: '', sudo: '' }));
  }, [phase.kind]);

  const field = (key: 'host' | 'port' | 'user' | 'dir', type = 'text') => (
    <label className="reloc-field">
      <span>{t(`push.${key}`)}</span>
      <input
        className="field"
        type={type}
        value={form[key]}
        disabled={locked}
        autoComplete="off"
        onChange={(e) => setForm({ ...form, [key]: e.target.value })}
      />
    </label>
  );

  return (
    <Card title={t('push.heading')}>
      <p className="muted">{t('push.help')}</p>
      <OptionRows value={options} onChange={setOptions} disabled={locked} />
      <div className="reloc-target">
        {field('host')}
        {field('port')}
        {field('user')}
        {field('dir')}
      </div>
      <div className="reloc-auth">
        <Segmented
          options={RELOCATION_AUTH_KINDS.map((k) => ({ value: k, label: t(`authKind.${k}`) }))}
          value={form.kind}
          onChange={(v) => setForm({ ...form, kind: v as RelocationAuthKind, secret: '' })}
          ariaLabel={t('push.authKind')}
        />
        {form.kind === 'password' ? (
          <label className="reloc-field">
            <span>{t('push.password')}</span>
            <input
              className="field"
              type="password"
              value={form.secret}
              disabled={locked}
              autoComplete="new-password"
              onChange={(e) => setForm({ ...form, secret: e.target.value })}
            />
          </label>
        ) : (
          <label className="reloc-field reloc-key">
            <span>{t('push.key')}</span>
            <textarea
              className="field"
              rows={6}
              value={form.secret}
              disabled={locked}
              spellCheck={false}
              onChange={(e) => setForm({ ...form, secret: e.target.value })}
            />
          </label>
        )}
        <label className="reloc-field">
          <span>{t('push.sudo')}</span>
          <input
            className="field"
            type="password"
            value={form.sudo}
            disabled={locked}
            autoComplete="new-password"
            onChange={(e) => setForm({ ...form, sudo: e.target.value })}
          />
          <em className="muted">{t('push.sudoHint')}</em>
        </label>
      </div>
      {problems.length > 0 && form.host !== '' && (
        <ul className="reloc-problems">
          {problems.map((p) => (
            <li key={p} className="form-error">
              {t(`invalid.${p}`)}
            </li>
          ))}
        </ul>
      )}
      <div className="reloc-actions">
        <Button
          onClick={() => onStart('preflight', options, form)}
          disabled={locked || problems.length > 0}
        >
          {t('push.check')}
        </Button>
        <Button
          variant="primary"
          onClick={() => onStart('push', options, form)}
          disabled={locked || problems.length > 0}
        >
          {t('push.go')}
        </Button>
      </div>
      {error && <p className="form-error">{error}</p>}
    </Card>
  );
}

/** Where the run has got to, and what it has printed — including the far side's own output. */
function ProgressCard({ phase, log }: { phase: RunPhase; log: string[] }) {
  const { t } = useTranslation('settings-relocation');
  const stage = phase.kind === 'running' ? phase.stage : null;
  const done = phase.kind === 'done' ? phase.run : null;
  const doneStage = relocationStage(done?.stage);
  return (
    <Card title={t('run.heading')}>
      {(phase.kind === 'starting' || phase.kind === 'running') && (
        <>
          <ProgressBar value={stageProgress(phase)} label={t('run.heading')} />
          <p>
            {phase.kind === 'starting'
              ? t('run.starting')
              : t(`stage.${stage ?? 'start'}`)}
          </p>
        </>
      )}
      {done && phase.kind === 'done' && (
        <div className={phase.state === 'done' ? 'reloc-done' : 'form-error'}>
          <p>
            {phase.state === 'done'
              ? done.target_url
                ? t('done.pushed', { url: done.target_url })
                : t('done.archived')
              : t('done.failed', { stage: t(`stage.${doneStage ?? 'start'}`) })}
          </p>
          {done.message && <p className="muted">{done.message}</p>}
          {done.host_key_fingerprint && (
            <p className="muted">{t('done.fingerprint', { fp: done.host_key_fingerprint })}</p>
          )}
          {done.docker_installed && <p className="muted">{t('done.dockerInstalled')}</p>}
          {phase.state === 'done' && done.target_url && <AfterwardsList />}
        </div>
      )}
      {log.length > 0 && <pre className="reloc-log">{log.join('\n')}</pre>}
    </Card>
  );
}

/** The five things the new server's operator has to decide again. Also printed by the restore
 *  script and by RELOCATION-README.md — three copies of one list, so keep them in step. */
function AfterwardsList() {
  const { t } = useTranslation('settings-relocation');
  return (
    <>
      <h4>{t('afterwards.heading')}</h4>
      <ol className="reloc-afterwards">
        {[1, 2, 3, 4, 5].map((i) => (
          <li key={i}>{t(`afterwards.item${i}`)}</li>
        ))}
      </ol>
    </>
  );
}

/** The fallback path: build an archive, download it, carry it over. */
function ManualCard({
  status,
  busy,
  disabled,
  onCreate,
  onChanged,
}: {
  status: RelocationStatus;
  busy: boolean;
  disabled: boolean;
  onCreate: (o: Options) => void;
  onChanged: () => void;
}) {
  const { t } = useTranslation('settings-relocation');
  const [open, setOpen] = useState(false);
  const [options, setOptions] = useState<Options>({ ...DEFAULT_OPTIONS, installDocker: false });
  const [confirming, setConfirming] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const archive = status.archive ?? null;

  const download = () => {
    setError(null);
    api
      .downloadRelocationArchive()
      .then(({ blob, filename }) => saveBlob(blob, filename ?? 'yagra-relocation.tar.gz'))
      .catch((e: unknown) => setError(errMsg(e, t('err.download'))));
  };

  return (
    <Card title={t('manual.heading')}>
      <button type="button" className="reloc-disclose" onClick={() => setOpen(!open)}>
        {open ? t('manual.hide') : t('manual.show')}
      </button>
      {open && (
        <>
          <p className="muted">{t('manual.help')}</p>
          <OptionRows value={options} onChange={setOptions} disabled={busy || disabled} />
          <div className="reloc-actions">
            <Button onClick={() => onCreate(options)} disabled={busy || disabled}>
              {t('manual.create')}
            </Button>
          </div>
        </>
      )}
      {/* 🚨 Deliberately OUTSIDE the disclosure. An archive on disk holds the encryption key and
          every stored credential, so "there is one here" is not a detail of the fallback route —
          it is the single most important fact this screen can tell an operator, and hiding it
          behind a "show the manual route" link would mean the only way to find out is to already
          suspect it. */}
      {archive && (
        <div className="reloc-archive">
          <p>{t('manual.waiting')}</p>
          <p>
            <strong>{archive.filename}</strong>{' '}
            <span className="muted">
              {formatBytes(archive.size_bytes)} · {formatTimestamp(archive.modified_at * 1000)}
            </span>
          </p>
          <div className="reloc-actions">
            <Button variant="primary" onClick={download}>
              {t('manual.download')}
            </Button>
            <Button variant="danger" onClick={() => setConfirming(true)}>
              {t('manual.delete')}
            </Button>
          </div>
          <p className="muted">{t('manual.thenRun')}</p>
          <pre className="reloc-cmds">{targetCommands(archive.filename).join('\n')}</pre>
        </div>
      )}
      {error && <p className="form-error">{error}</p>}
      {confirming && (
        <ConfirmDeleteModal
          title={t('manual.deleteTitle')}
          onConfirm={() => api.deleteRelocation()}
          errorFallback={t('err.delete')}
          onClose={() => setConfirming(false)}
          onDone={() => {
            setConfirming(false);
            onChanged();
          }}
        >
          {t('manual.deleteBody')}
        </ConfirmDeleteModal>
      )}
    </Card>
  );
}
