// SPDX-License-Identifier: AGPL-3.0-only
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
import { Link } from 'react-router-dom';
import { api, ApiError } from '../services/api';
import { useAuthStore } from '../store';
import type {
  MonitoringGap,
  PollerInfo,
  PollerNodesResponse,
  PoolSummary,
} from '../types/api';
import { PageHeader } from '../components/ui/PageHeader';
import { Card } from '../components/ui/Card';
import { Button } from '../components/ui/Button';
import { ConfirmDeleteModal } from '../components/ui/ConfirmDeleteModal';
import { Modal } from '../components/ui/Modal';
import { Badge } from '../components/ui/Badge';
import { IconButton } from '../components/ui/IconButton';
import { TextInput, FieldHint } from '../components/ui/Field';
import { TableToolbar, TableSpacer, ResultCount } from '../components/ui/TableToolbar';
import { TrashIcon, WarningIcon } from '../components/ui/icons';
import { dateOnly, formatCount, formatUtil, formatExactTime, relativeTime } from '../lib/format';
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

const COLS = '1.2fr 120px 108px 92px 150px 82px 64px 64px 64px 96px 112px 58px';
const REFRESH_MS = 10_000;

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
  return (
    <ConfirmDeleteModal
      title={t('pollers.remove.title')}
      confirmLabel={t('pollers.remove.title')}
      onConfirm={() => api.deletePoller(poller.id)}
      errorFallback={t('pollers.err.remove')}
      onClose={onClose}
      onDone={onDone}
    >
      <Trans
        t={t}
        i18nKey="pollers.remove.confirmText"
        values={{ id: poller.id }}
        components={{ code: <strong className="mono" /> }}
      />
    </ConfirmDeleteModal>
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

const GAP_COLS = '1.4fr 120px 1fr 120px';

/** Recent core↔poller visibility outages (store-and-forward, Phase 3). Each row is a window during
 *  which core couldn't hear from the poller; if the poller stayed alive but partitioned, it backfills
 *  the metrics for the window on reconnect (alerts are not backfilled). A compact subsection below the
 *  fleet table, hidden entirely when there are none (gaps are the exception, not the norm). */
function MonitoringGapsSection({ gaps }: { gaps: MonitoringGap[] }) {
  const { t } = useTranslation('system');
  if (gaps.length === 0) return null;

  const duration = (secs: number): string => {
    if (secs < 60) return t('pollers.gaps.durSecs', { count: secs });
    if (secs < 3600) return t('pollers.gaps.durMins', { count: Math.round(secs / 60) });
    const h = Math.floor(secs / 3600);
    const m = Math.round((secs % 3600) / 60);
    return t('pollers.gaps.durHours', { hours: h, mins: m });
  };

  return (
    <div className="poller-gaps">
      <h2 className="poller-gaps-title">{t('pollers.gaps.title')}</h2>
      <p className="muted poller-gaps-note">{t('pollers.gaps.note')}</p>
      <div className="ytable">
        <div className="ytable-scroll">
          <div className="ytable-head" style={{ gridTemplateColumns: GAP_COLS }}>
            <div className="ytable-h">{t('pollers.gaps.cols.poller')}</div>
            <div className="ytable-h">{t('pollers.gaps.cols.pool')}</div>
            <div className="ytable-h">{t('pollers.gaps.cols.window')}</div>
            <div className="ytable-h right">{t('pollers.gaps.cols.duration')}</div>
          </div>
          {gaps.map((g) => (
            <div className="ytable-row" style={{ gridTemplateColumns: GAP_COLS }} key={g.id}>
              <div className="ytable-cell mono">{g.poller_id}</div>
              <div className="ytable-cell">
                <Badge tone="neutral">{g.pool}</Badge>
              </div>
              <div
                className="ytable-cell"
                title={`${formatExactTime(g.started_at)} → ${formatExactTime(g.ended_at)}`}
              >
                {relativeTime(g.ended_at)}
              </div>
              <div className="ytable-cell right mono">{duration(g.duration_secs)}</div>
            </div>
          ))}
        </div>
      </div>
    </div>
  );
}

const NODE_COLS = '1fr 320px';

/** The nodes one poller currently holds — "if this poller goes away, what stops being monitored?".
 *  Inline (not a modal): read-only contextual detail, which ui-conventions keeps out of modals.
 *  Fed by the coordinator's published working set, so it agrees with the node detail's "Polled by"
 *  by construction. */
function PollerNodesSection({
  data,
  onClose,
}: {
  data: PollerNodesResponse;
  onClose: () => void;
}) {
  const { t } = useTranslation('system');
  const note =
    data.state === 'offline'
      ? t('pollers.nodes.offline')
      : data.state === 'unknown'
        ? t('pollers.nodes.unknown')
        : data.nodes.length === 0
          ? t('pollers.nodes.empty')
          : t('pollers.nodes.note');

  return (
    <div className="poller-gaps">
      <div className="poller-nodes-head">
        <h2 className="poller-gaps-title">
          {t('pollers.nodes.title', { id: data.poller_id })}
        </h2>
        <Button variant="outline" onClick={onClose}>
          {t('pollers.nodes.close')}
        </Button>
      </div>
      <p className="muted poller-gaps-note">{note}</p>
      {data.truncated && (
        <p className="muted poller-gaps-note">
          {t('pollers.nodes.truncated', { shown: data.nodes.length, total: data.total })}
        </p>
      )}
      {data.nodes.length > 0 && (
        <div className="ytable">
          <div className="ytable-scroll">
            <div className="ytable-head" style={{ gridTemplateColumns: NODE_COLS }}>
              <div className="ytable-h">{t('pollers.nodes.colName')}</div>
              <div className="ytable-h">{t('pollers.cols.pool')}</div>
            </div>
            {data.nodes.map((n) => (
              <div className="ytable-row" style={{ gridTemplateColumns: NODE_COLS }} key={n.id}>
                {/* Human name is the primary; the uuid stays on hover (no raw UUIDs in tables). */}
                <div className="ytable-cell" title={n.id}>
                  <Link to={`/nodes?sel=${encodeURIComponent(`node:${n.id}`)}`}>{n.name}</Link>
                </div>
                <div className="ytable-cell">
                  <Badge tone="neutral">{data.pool ?? '—'}</Badge>
                </div>
              </div>
            ))}
          </div>
        </div>
      )}
    </div>
  );
}

export function PollersPage() {
  const { t } = useTranslation('system');
  const authed = useAuthStore((s) => s.authed);
  const [pollers, setPollers] = useState<PollerInfo[]>([]);
  const [pools, setPools] = useState<PoolSummary[]>([]);
  const [gaps, setGaps] = useState<MonitoringGap[]>([]);
  const [unavailable, setUnavailable] = useState(false);
  const [loading, setLoading] = useState(true);
  const [registering, setRegistering] = useState(false);
  const [deleting, setDeleting] = useState<PollerInfo | null>(null);
  /** The poller whose node drill-down is open (`null` ⇒ closed), and its last loaded page. */
  const [drillId, setDrillId] = useState<string | null>(null);
  const [drill, setDrill] = useState<PollerNodesResponse | null>(null);

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
    // Monitoring gaps are best-effort context (store-and-forward, Phase 3) — a read error just leaves
    // the section hidden; it must never block the fleet table.
    api
      .listMonitoringGaps()
      .then(setGaps)
      .catch(() => {});
  }, []);

  useEffect(() => {
    load();
    const id = setInterval(load, REFRESH_MS);
    return () => clearInterval(id);
  }, [load]);

  // The open drill-down follows the same cadence as the fleet table, so a reassignment shows up
  // without the operator reopening it. A read error just leaves the last page on screen.
  useEffect(() => {
    if (!drillId) {
      setDrill(null);
      return;
    }
    let cancelled = false;
    const fetch = () => {
      api
        .listPollerNodes(drillId)
        .then((d) => !cancelled && setDrill(d))
        .catch(() => undefined);
    };
    fetch();
    const id = setInterval(fetch, REFRESH_MS);
    return () => {
      cancelled = true;
      clearInterval(id);
    };
  }, [drillId]);

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
                <div className="ytable-h">{t('pollers.cols.firstSeen')}</div>
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
                      <div className="ytable-cell">
                        {/* A button, not a clickable row: `.ytable-row` has no clickable variant,
                            and a button is keyboard-reachable by construction. */}
                        <button
                          type="button"
                          className="poller-drill mono"
                          onClick={() => setDrillId(drillId === p.id ? null : p.id)}
                        >
                          {p.id}
                        </button>
                      </div>
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
                      <div className="ytable-cell right mono">{formatUtil(p.cpu_pct ?? null)}</div>
                      <div className="ytable-cell right mono">{formatUtil(p.mem_used_pct ?? null)}</div>
                      <div className="ytable-cell right mono">{formatUtil(p.disk_used_pct ?? null)}</div>
                      {/* Date only: this is inventory ("since when has this poller existed"), so a
                          registration date beats a relative age. Exact instant on hover. */}
                      <div className="ytable-cell mono" title={p.first_seen ? formatExactTime(p.first_seen) : undefined}>
                        {p.first_seen ? dateOnly(p.first_seen) : '—'}
                      </div>
                      <div className="ytable-cell">{lastSeenLabel(p.last_seen ?? null, online, t)}</div>
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

          {drill && <PollerNodesSection data={drill} onClose={() => setDrillId(null)} />}

          <MonitoringGapsSection gaps={gaps} />
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
