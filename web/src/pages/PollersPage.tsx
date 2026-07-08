// Pollers (Settings ▸ Pollers). The distributed-poller fleet (ADR-009/020): a per-pool summary
// strip over the data-table standard list of registered pollers. Pollers appear when they connect
// (heartbeat) and are removed here only once offline. Read-only for viewers; deleting a poller and
// the register-poller helper follow the shared Modal patterns. The list auto-refreshes every 10s
// (no SSE — poller liveness is coarse-grained), on top of a manual reload.
//
// Modeled on MutesPage/EventSourcesPage (the data-table-standard exemplars): PageHeader with the
// Settings ▸ Pollers trail, a TableToolbar (count + primary action) over the shared `.ytable`, and
// destructive-consent via the shared Modal.

import { useCallback, useEffect, useState } from 'react';
import { Trans, useTranslation } from 'react-i18next';
import { api, ApiError } from '../services/api';
import { useAuthStore } from '../store';
import type { PollerInfo, PoolSummary } from '../types/api';
import { PageHeader } from '../components/ui/PageHeader';
import { Card } from '../components/ui/Card';
import { Button } from '../components/ui/Button';
import { Modal } from '../components/ui/Modal';
import { Badge } from '../components/ui/Badge';
import { IconButton } from '../components/ui/IconButton';
import { TextInput, FieldHint } from '../components/ui/Field';
import { TableToolbar, TableSpacer, ResultCount } from '../components/ui/TableToolbar';
import { TrashIcon, WarningIcon } from '../components/ui/icons';
import { formatCount, formatUtil } from '../lib/format';
import {
  buildPollerEnv,
  isValidPollerToken,
  lastSeenLabel,
  poolHasWarning,
  poolModeLabel,
  workingSetLabel,
  POLLER_UP_COMMAND,
} from '../lib/pollers';
import './PollersPage.css';

const COLS = '1.2fr 120px 108px 92px 150px 82px 64px 64px 64px 112px 58px';
const REFRESH_MS = 10_000;

const errMsg = (e: unknown, fallback: string) => (e instanceof ApiError ? e.message : fallback);

/** One pool card in the summary strip: name + node/poller counts + mode, with a warning chip when
 *  the pool has nodes but no live poller (icon + text, never color alone — a11y). */
function PoolCard({ pool }: { pool: PoolSummary }) {
  const { t } = useTranslation('system');
  const warn = poolHasWarning(pool);
  return (
    <div className={`pool-card${warn ? ' has-warn' : ''}`}>
      <div className="pool-card-head">
        <span className="pool-card-name mono">{pool.pool}</span>
        <Badge tone={pool.mode === 'working_set' ? 'up' : 'neutral'}>{poolModeLabel(pool.mode, t)}</Badge>
      </div>
      <div className="pool-card-stats">
        <span>
          <strong>{formatCount(pool.nodes)}</strong> {t('common:noun.node', { count: pool.nodes })}
        </span>
        <span className="pool-card-sep">·</span>
        <span>
          <strong>{formatCount(pool.live_pollers)}</strong>{' '}
          {t('pollers.pool.livePoller', { count: pool.live_pollers })}
        </span>
      </div>
      {warn && (
        <span className="pool-warn">
          <WarningIcon />
          {t('pollers.noLivePoller')}
        </span>
      )}
    </div>
  );
}

/** Confirm + remove an offline poller from the durable inventory (destructive-consent modal). On a
 *  409 (`poller_online`) — the poller came back between render and click — surface the reason. */
function DeletePollerModal({
  poller,
  onClose,
  onDone,
}: {
  poller: PollerInfo;
  onClose: () => void;
  onDone: () => void;
}) {
  const { t } = useTranslation('system');
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const submit = () => {
    setBusy(true);
    setError(null);
    api
      .deletePoller(poller.id)
      .then(onDone)
      .catch((e: unknown) => {
        setError(errMsg(e, t('pollers.err.remove')));
        setBusy(false);
      });
  };

  return (
    <Modal
      title={t('pollers.remove.title')}
      onClose={onClose}
      footer={
        <>
          <Button variant="outline" onClick={onClose} disabled={busy}>
            {t('common:actions.cancel')}
          </Button>
          <Button variant="danger" onClick={submit} disabled={busy}>
            {t('pollers.remove.title')}
          </Button>
        </>
      }
    >
      <p className="modal-confirm-text">
        <Trans
          t={t}
          i18nKey="pollers.remove.confirmText"
          values={{ id: poller.id }}
          components={{ code: <strong className="mono" /> }}
        />
      </p>
      {error && <p className="form-error">{error}</p>}
    </Modal>
  );
}

/** Client-side helper: collect id/pool/bus URL and render the ready-to-paste config for a remote
 *  poller container. No API call — it only produces the `docker-compose.poller.yml` `.env`. */
function RegisterPollerModal({ onClose }: { onClose: () => void }) {
  const { t } = useTranslation('system');
  const [id, setId] = useState('');
  const [pool, setPool] = useState('');
  const [busUrl, setBusUrl] = useState('');
  const [caFile, setCaFile] = useState('');
  const [copied, setCopied] = useState<'env' | 'cmd' | null>(null);

  const idBad = id !== '' && !isValidPollerToken(id);
  const poolBad = pool !== '' && !isValidPollerToken(pool);
  const ready = isValidPollerToken(id) && isValidPollerToken(pool) && busUrl.trim() !== '';

  const env = ready
    ? buildPollerEnv({ id, pool, busUrl: busUrl.trim(), caFile })
    : '';

  const copy = (text: string, mark: 'env' | 'cmd') => {
    void navigator.clipboard?.writeText(text);
    setCopied(mark);
    setTimeout(() => setCopied(null), 1200);
  };

  return (
    <Modal
      title={t('pollers.register.title')}
      onClose={onClose}
      footer={
        <Button variant="primary" onClick={onClose}>
          {t('pollers.register.done')}
        </Button>
      }
    >
      <p className="modal-confirm-text">
        <Trans
          t={t}
          i18nKey="pollers.register.intro"
          components={{ env: <span className="mono" />, file: <span className="mono" /> }}
        />
      </p>

      <div className="modal-field">
        <label className="modal-field-label">{t('pollers.register.fields.id.label')}</label>
        <TextInput
          value={id}
          onChange={(e) => setId(e.target.value)}
          placeholder="tokyo-edge-1"
          autoFocus
        />
        <FieldHint error={idBad}>
          {idBad ? t('pollers.register.invalidToken') : t('pollers.register.fields.id.hint')}
        </FieldHint>
      </div>

      <div className="modal-field">
        <label className="modal-field-label">{t('pollers.register.fields.pool.label')}</label>
        <TextInput value={pool} onChange={(e) => setPool(e.target.value)} placeholder="tokyo" />
        <FieldHint error={poolBad}>
          {poolBad ? t('pollers.register.invalidToken') : t('pollers.register.fields.pool.hint')}
        </FieldHint>
      </div>

      <div className="modal-field">
        <label className="modal-field-label">{t('pollers.register.fields.busUrl.label')}</label>
        <TextInput
          value={busUrl}
          onChange={(e) => setBusUrl(e.target.value)}
          placeholder="tls://poller:<password>@yagra.example.com:4222"
        />
        <FieldHint>{t('pollers.register.fields.busUrl.hint')}</FieldHint>
      </div>

      <div className="modal-field">
        <label className="modal-field-label">{t('pollers.register.fields.caFile.label')}</label>
        <TextInput
          value={caFile}
          onChange={(e) => setCaFile(e.target.value)}
          placeholder="/certs/ca.pem"
        />
        <FieldHint>{t('pollers.register.fields.caFile.hint')}</FieldHint>
      </div>

      <div className="modal-field">
        <label className="modal-field-label">{t('pollers.register.fields.env.label')}</label>
        {ready ? (
          <div className="poller-copyrow">
            <pre className="poller-snippet mono">{env}</pre>
            <Button variant="outline" onClick={() => copy(env, 'env')}>
              {copied === 'env' ? t('common:copy.copied') : t('pollers.register.copy')}
            </Button>
          </div>
        ) : (
          <p className="muted poller-snippet-empty">{t('pollers.register.snippetEmpty')}</p>
        )}
      </div>

      <div className="modal-field">
        <label className="modal-field-label">{t('pollers.register.fields.bringUp.label')}</label>
        <div className="poller-copyrow">
          <code className="poller-snippet mono">{POLLER_UP_COMMAND}</code>
          <Button variant="outline" onClick={() => copy(POLLER_UP_COMMAND, 'cmd')}>
            {copied === 'cmd' ? t('common:copy.copied') : t('pollers.register.copy')}
          </Button>
        </div>
      </div>
    </Modal>
  );
}

export function PollersPage() {
  const { t } = useTranslation('system');
  const authed = useAuthStore((s) => s.authed);
  const [pollers, setPollers] = useState<PollerInfo[]>([]);
  const [pools, setPools] = useState<PoolSummary[]>([]);
  const [unavailable, setUnavailable] = useState(false);
  const [loading, setLoading] = useState(true);
  const [registering, setRegistering] = useState(false);
  const [deleting, setDeleting] = useState<PollerInfo | null>(null);

  // Refresh without flashing the initial loading state on every poll (loading only gates the very
  // first paint, like the sibling list pages).
  const load = useCallback(() => {
    api
      .listPollers()
      .then((res) => {
        setPollers(res.pollers);
        setPools(res.pools);
        setUnavailable(false);
      })
      .catch((e: unknown) => {
        if (e instanceof ApiError && e.code === 'admin_unavailable') setUnavailable(true);
      })
      .finally(() => setLoading(false));
  }, []);

  useEffect(() => {
    load();
    const id = setInterval(load, REFRESH_MS);
    return () => clearInterval(id);
  }, [load]);

  return (
    <div>
      <PageHeader
        title={t('nav:settings.pollers')}
        trail={[{ label: t('nav:sections.settings') }, { label: t('nav:settings.pollers') }]}
        note={t('pollers.note')}
      />

      {unavailable ? (
        <Card>
          <p className="muted">{t('pollers.unavailable')}</p>
        </Card>
      ) : (
        <>
          {pools.length > 0 && (
            <div className="pool-strip">
              {pools.map((p) => (
                <PoolCard key={p.pool} pool={p} />
              ))}
            </div>
          )}

          <TableToolbar>
            <TableSpacer />
            <ResultCount
              shown={pollers.length}
              noun={t('common:noun.poller', { count: pollers.length })}
            />
            {authed && (
              <Button variant="primary" onClick={() => setRegistering(true)}>
                {t('pollers.registerButton')}
              </Button>
            )}
          </TableToolbar>

          <div className="ytable">
            <div className="ytable-scroll">
              <div className="ytable-head" style={{ gridTemplateColumns: COLS }}>
                <div className="ytable-h">{t('pollers.cols.poller')}</div>
                <div className="ytable-h">{t('pollers.cols.pool')}</div>
                <div className="ytable-h">{t('pollers.cols.status')}</div>
                <div className="ytable-h">{t('pollers.cols.version')}</div>
                <div className="ytable-h">{t('pollers.cols.workingSet')}</div>
                <div className="ytable-h right">{t('pollers.cols.results')}</div>
                <div className="ytable-h right">{t('pollers.cols.cpu')}</div>
                <div className="ytable-h right">{t('pollers.cols.mem')}</div>
                <div className="ytable-h right">{t('pollers.cols.disk')}</div>
                <div className="ytable-h">{t('pollers.cols.lastSeen')}</div>
                <div className="ytable-h right">{t('pollers.cols.actions')}</div>
              </div>

              {pollers.length === 0 ? (
                <div className="yt-empty">
                  <p className="yt-empty-title">
                    {loading ? t('common:loading') : t('pollers.empty.title')}
                  </p>
                  {!loading && (
                    <p className="yt-empty-sub">
                      <Trans
                        t={t}
                        i18nKey="pollers.empty.sub"
                        components={{ b: <strong /> }}
                      />
                    </p>
                  )}
                </div>
              ) : (
                pollers.map((p) => {
                  const online = p.status === 'online';
                  return (
                    <div className="ytable-row" style={{ gridTemplateColumns: COLS }} key={p.id}>
                      <div className="ytable-cell mono">{p.id}</div>
                      <div className="ytable-cell">
                        <Badge tone="neutral">{p.pool}</Badge>
                      </div>
                      <div className="ytable-cell">
                        <span className={`poller-status ${online ? 'online' : 'offline'}`}>
                          <span className="poller-status-dot" />
                          {online ? t('pollers.status.online') : t('pollers.status.offline')}
                        </span>
                      </div>
                      <div className="ytable-cell mono">{p.version ?? '—'}</div>
                      <div className="ytable-cell">
                        {workingSetLabel(p.working_set_nodes, p.working_set_specs, online, t)}
                      </div>
                      <div className="ytable-cell right mono">{formatCount(p.results_total)}</div>
                      <div className="ytable-cell right mono">{formatUtil(p.cpu_pct)}</div>
                      <div className="ytable-cell right mono">{formatUtil(p.mem_used_pct)}</div>
                      <div className="ytable-cell right mono">{formatUtil(p.disk_used_pct)}</div>
                      <div className="ytable-cell">{lastSeenLabel(p.last_seen, online, t)}</div>
                      <div className="ytable-cell right">
                        {authed && !online && (
                          <span className="ytable-actions">
                            <IconButton
                              title={t('pollers.remove.title')}
                              danger
                              onClick={() => setDeleting(p)}
                            >
                              <TrashIcon />
                            </IconButton>
                          </span>
                        )}
                      </div>
                    </div>
                  );
                })
              )}
            </div>
          </div>
        </>
      )}

      {registering && <RegisterPollerModal onClose={() => setRegistering(false)} />}
      {deleting && (
        <DeletePollerModal
          poller={deleting}
          onClose={() => setDeleting(null)}
          onDone={() => {
            setDeleting(null);
            load();
          }}
        />
      )}
    </div>
  );
}
