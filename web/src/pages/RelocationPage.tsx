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
import { Trans, useTranslation } from 'react-i18next';
import { PageHeader } from '../components/ui/PageHeader';
import { Inline, Page, Stack } from '../components/ui/Page';
import { Facts } from '../components/ui/Facts';
import { Callout, Disclosure, SectionHead } from '../components/ui/Section';
import { CheckField, FieldRow, Fields, SaveNote } from '../components/ui/FormLayout';
import { Field, TextArea, TextInput } from '../components/ui/Field';
import { LoadBlockNotice } from '../components/ui/LoadBlockNotice';
import { classifyLoadError, type LoadBlock } from '../lib/loadState';
import { Card } from '../components/ui/Card';
import { Button } from '../components/ui/Button';
import { Segmented } from '../components/ui/Segmented';
import { ProgressBar } from '../components/ui/ProgressBar';
import { ConfirmDeleteModal } from '../components/ui/ConfirmDeleteModal';
import { InfoTip } from '../components/ui/InfoTip';
import { ScreenLink } from '../components/ui/ScreenLink';
import { StepFrame } from '../components/ui/StepFrame';
import { CopyCommand } from '../components/ui/CopyCommand';
import { api, errMsg } from '../services/api';
import { saveBlob } from '../lib/download';
import { formatBytes, formatTimestamp } from '../lib/format';
import type { RelocationStatus } from '../types/api';
import type { RelocationAuthKind, RunPhase, TargetForm } from './relocationStatus';
import {
  RELOCATION_AUTH_KINDS,
  STOP_OLD_SERVER_COMMAND,
  canStart,
  readiness,
  relocationStage,
  roomForArchive,
  runPhase,
  shouldPoll,
  stageProgress,
  targetCommands,
  targetScreenUrl,
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
      <Page layout="sections">
        <Header />
        <Card>
          <p className="form-error">{t('loadFailed')}</p>
        </Card>
      </Page>
    );
  }
  if (!status) {
    return (
      <Page layout="sections">
        <Header />
      </Page>
    );
  }

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
    <Page layout="sections">
      <Header />
      <Warning />
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
    </Page>
  );
}

function Header() {
  const { t } = useTranslation('settings-relocation');
  return (
    <PageHeader
      title={t('title')}
      trail={[{ label: t('nav:sections.settings') }, { label: t('title') }]}
    />
  );
}

/** The two hazards, stated before anything else on the page. The third, Docker installed as
 *  root, is said by the option that does it, where it can be refused. */
function Warning() {
  const { t } = useTranslation('settings-relocation');
  return (
    <Callout tone="critical">
      <ul className="reloc-list">
        <li>{t('warning.secrets')}</li>
        <li>{t('warning.ssh')}</li>
      </ul>
    </Callout>
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
        <Stack gap={6} as="section">
          <SectionHead title={t('carried.yes')} />
          <ul className="reloc-list">
            {list('carried.yesItem', 6).map((s) => (
              <li key={s}>{s}</li>
            ))}
          </ul>
        </Stack>
        <Stack gap={6} as="section">
          <SectionHead title={t('carried.no')} />
          <ul className="reloc-list">
            {list('carried.noItem', 3).map((s) => (
              <li key={s}>{s}</li>
            ))}
          </ul>
        </Stack>
        <Stack gap={6} as="section">
          <SectionHead title={t('carried.again')} />
          <ul className="reloc-list">
            {list('carried.againItem', 5).map((s) => (
              <li key={s}>{s}</li>
            ))}
          </ul>
        </Stack>
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
      <p className={state === 'ready' ? 'muted' : 'form-error'}>
        <Trans
          t={t}
          i18nKey={`readiness.${state}`}
          components={{ lnk: <ScreenLink to="/settings/upgrade" /> }}
        />
      </p>
      <Facts
        items={[
          { label: t('space.free'), value: size(status.free_bytes) },
          {
            key: 'estimate',
            label: (
              <>
                {t('space.estimate')}{' '}
                <InfoTip
                  infoKey="settings-relocation:space.partial.info"
                  label={t('space.estimate')}
                />
              </>
            ),
            value: size(status.estimate_bytes),
          },
          { label: t('space.needed'), value: size(status.needed_bytes) },
        ]}
      />
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
  // Each option's cost sits under it as a short sub-label (ADR-200 kind 5). Part of the control,
  // not a tooltip: a tooltip is a thing a touch device never shows (ADR-055 R4), and each of these
  // four options has a cost the operator has to weigh before ticking it.
  const row = (key: keyof Options, name: string) => (
    <CheckField
      key={name}
      checked={value[key]}
      disabled={disabled}
      onChange={(next) => onChange({ ...value, [key]: next })}
      label={<strong>{t(`options.${name}`)}</strong>}
      detail={t(`options.${name}Hint`)}
    />
  );
  return (
    <Stack gap={8}>
      {row('metrics', 'metrics')}
      {row('tier2', 'tier2')}
      {row('images', 'images')}
      {row('installDocker', 'installDocker')}
    </Stack>
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
  // A sudo password typed and then made moot by unticking Docker is not sent.
  const sendable = options.installDocker ? form : { ...form, sudo: '' };

  // The credentials live in this component's state and nowhere else — not in the store, not in the
  // URL, not in `localStorage`. When a push finishes they go, because the next press is a new
  // decision and a password sitting in a mounted form is one an unattended browser still holds.
  useEffect(() => {
    if (phase.kind === 'done') setForm((f) => ({ ...f, secret: '', sudo: '' }));
  }, [phase.kind]);

  const field = (key: 'host' | 'port' | 'user' | 'dir', type = 'text') => (
    <Field label={t(`push.${key}`)}>
      <TextInput
        type={type}
        value={form[key]}
        disabled={locked}
        autoComplete="off"
        onChange={(e) => setForm({ ...form, [key]: e.target.value })}
      />
    </Field>
  );

  return (
    <Card
      title={t('push.heading')}
      footer={
        <>
          {error && <SaveNote tone="error">{error}</SaveNote>}
          <Button
            onClick={() => onStart('preflight', options, sendable)}
            disabled={locked || problems.length > 0}
          >
            {t('push.check')}
          </Button>
          <Button
            variant="primary"
            onClick={() => onStart('push', options, sendable)}
            disabled={locked || problems.length > 0}
          >
            {t('push.go')}
          </Button>
        </>
      }
    >
      <OptionRows value={options} onChange={setOptions} disabled={locked} />
      <Fields>
        <FieldRow>
          {field('host')}
          {field('port')}
          {field('user')}
          {field('dir')}
        </FieldRow>
        <Segmented
          options={RELOCATION_AUTH_KINDS.map((k) => ({ value: k, label: t(`authKind.${k}`) }))}
          value={form.kind}
          onChange={(v) => setForm({ ...form, kind: v as RelocationAuthKind, secret: '' })}
          ariaLabel={t('push.authKind')}
        />
        {form.kind === 'password' ? (
          <Field label={t('push.password')}>
            <TextInput
              type="password"
              value={form.secret}
              disabled={locked}
              autoComplete="new-password"
              onChange={(e) => setForm({ ...form, secret: e.target.value })}
            />
          </Field>
        ) : (
          // A private key is a dozen lines of base64; a one-line input turns pasting one into a
          // guess.
          <Field label={t('push.key')}>
            <TextArea
              rows={6}
              value={form.secret}
              disabled={locked}
              spellCheck={false}
              onChange={(e) => setForm({ ...form, secret: e.target.value })}
            />
          </Field>
        )}
        {/* sudo is used only to install Docker, so the field exists only while that is ticked. */}
        {options.installDocker && (
          <Field label={t('push.sudo')}>
            <TextInput
              type="password"
              value={form.sudo}
              disabled={locked}
              autoComplete="new-password"
              onChange={(e) => setForm({ ...form, sudo: e.target.value })}
            />
          </Field>
        )}
      </Fields>
      {problems.length > 0 && form.host !== '' && (
        <ul className="reloc-list">
          {problems.map((p) => (
            <li key={p} className="form-error">
              {t(`invalid.${p}`)}
            </li>
          ))}
        </ul>
      )}
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
          {phase.state === 'done' && done.target_url && <AfterwardsList url={done.target_url} />}
        </div>
      )}
      {log.length > 0 && <pre className="reloc-log">{log.join('\n')}</pre>}
    </Card>
  );
}

/** The five things the new server's operator has to decide again. Also printed by the restore
 *  script and by RELOCATION-README.md — three copies of one list, so keep them in step.
 *  The two screen links open the NEW server (`url`): that is where those settings now live. */
function AfterwardsList({ url }: { url: string }) {
  const { t } = useTranslation('settings-relocation');
  return (
    <Stack gap={8}>
      <SectionHead title={t('afterwards.heading')} />
      <ol className="reloc-list">
        <li>
          {t('afterwards.item1')}
          <CopyCommand command={STOP_OLD_SERVER_COMMAND} />
        </li>
        <li>
          <Trans
            t={t}
            i18nKey="afterwards.item2"
            components={{
              lnk: (
                <ScreenLink
                  to="/settings/pollers"
                  href={targetScreenUrl(url, '/settings/pollers')}
                />
              ),
            }}
          />
        </li>
        <li>
          <Trans
            t={t}
            i18nKey="afterwards.item3"
            components={{
              lnk: <ScreenLink to="/settings/auth" href={targetScreenUrl(url, '/settings/auth')} />,
            }}
          />
        </li>
        <li>{t('afterwards.item4')}</li>
        <li>{t('afterwards.item5')}</li>
      </ol>
    </Stack>
  );
}

/** What the operator types on the new server, having carried the archive there. */
function RestoreSteps({ filename }: { filename: string }) {
  const { t } = useTranslation('settings-relocation');
  const [mkdir, untar, run] = targetCommands(filename);
  return (
    <StepFrame
      className="reloc-steps"
      summary={t('manual.steps.title')}
      steps={[
        t('manual.steps.s1'),
        <>
          {t('manual.steps.s2')}
          <CopyCommand command={mkdir} />
        </>,
        <>
          {t('manual.steps.s3')}
          <CopyCommand command={untar} />
        </>,
        <>
          {t('manual.steps.s4')}
          <CopyCommand command={run} />
        </>,
      ]}
    />
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
      {/* Folded rather than a button: the manual path is the fallback, and a button of the same
          weight as "Move this deployment" would read as two equal choices. */}
      <Disclosure
        summary={open ? t('manual.hide') : t('manual.show')}
        open={open}
        onToggle={setOpen}
      >
        <OptionRows value={options} onChange={setOptions} disabled={busy || disabled} />
        <Inline gap={12}>
          <Button onClick={() => onCreate(options)} disabled={busy || disabled}>
            {t('manual.create')}
          </Button>
        </Inline>
      </Disclosure>
      {/* 🚨 Deliberately OUTSIDE the disclosure. An archive on disk holds the encryption key and
          every stored credential, so "there is one here" is not a detail of the fallback route —
          it is the single most important fact this screen can tell an operator, and hiding it
          behind a "show the manual route" link would mean the only way to find out is to already
          suspect it. */}
      {archive && (
        <Stack gap={12} className="reloc-archive">
          <div>{t('manual.waiting')}</div>
          <div>
            <strong>{archive.filename}</strong>{' '}
            <span className="muted">
              {formatBytes(archive.size_bytes)} · {formatTimestamp(archive.modified_at * 1000)}
            </span>
          </div>
          <Inline gap={12}>
            <Button variant="primary" onClick={download}>
              {t('manual.download')}
            </Button>
            <Button variant="danger" onClick={() => setConfirming(true)}>
              {t('manual.delete')}
            </Button>
          </Inline>
          <RestoreSteps filename={archive.filename} />
        </Stack>
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
