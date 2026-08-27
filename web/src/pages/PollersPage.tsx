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

import { useCallback, useEffect, useMemo, useState } from 'react';
import { Trans, useTranslation } from 'react-i18next';
import { Link } from 'react-router-dom';
import { api, errMsg } from '../services/api';
import { useCan } from '../store';
import type {
  MonitoringGap,
  PollerInfo,
  PollerNodesResponse,
  PoolSummary,
} from '../types/api';
import { PageHeader } from '../components/ui/PageHeader';
import { Button } from '../components/ui/Button';
import { ConfirmDeleteModal } from '../components/ui/ConfirmDeleteModal';
import { Modal } from '../components/ui/Modal';
import { Badge } from '../components/ui/Badge';
import { IconButton } from '../components/ui/IconButton';
import { TextInput, FieldHint } from '../components/ui/Field';
import { TableToolbar, TableSpacer, ResultCount } from '../components/ui/TableToolbar';
import { DataTable, type Column } from '../components/ui/DataTable';
import { ClearFilters } from '../components/ui/ClearFilters';
import { FilterButton, MobileFilterSheet } from '../components/ui/MobileFilterSheet';
import { useClientFilters } from '../lib/useClientFilters';
import { pollerFilters } from './pollerFilters';
import { TrashIcon, WarningIcon } from '../components/ui/icons';
import { NodePicker } from '../components/NodePicker/NodePicker';
import { EntityName, useEntityNames } from '../components/ui/EntityName';
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
import { saveBlob } from '../lib/download';
import { BusPanel } from './BusPanel';
import './PollersPage.css';
import { classifyLoadError, type LoadBlock } from '../lib/loadState';
import { LoadBlockNotice } from '../components/ui/LoadBlockNotice';

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

/** Point a poller at the node it attaches to, or clear it (ADR-043).
 *
 *  Why this exists at all: the derived dependency graph gets its direction from distance to a
 *  poller, so core has to know where each poller sits. It works that out from the addresses the
 *  poller reports — but a poller running in a container reports a container-network address that
 *  matches no monitored node, which is the *common* deployment rather than an edge case. Until every
 *  pool that has nodes has a placed poller, derived suppression stays blocked. */
function SetAnchorModal({
  poller,
  onClose,
  onDone,
}: {
  poller: PollerInfo;
  onClose: () => void;
  onDone: () => void;
}) {
  const { t } = useTranslation('system');
  const [node, setNode] = useState<{ id: string; name: string } | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  // Resolve the current anchor's name for the picker's trigger. `getNodeNames` is the fleet-wide
  // resolver (a bounded `listNodes()` would miss an anchor outside the first page).
  useEffect(() => {
    let cancelled = false;
    if (!poller.anchor_node_id) return;
    api
      .getNodeNames([poller.anchor_node_id])
      .then((rows) => {
        const hit = rows[0];
        if (!cancelled && hit) setNode({ id: hit.id, name: hit.name });
      })
      .catch(() => {});
    return () => {
      cancelled = true;
    };
  }, [poller.anchor_node_id]);

  const save = () => {
    setBusy(true);
    setError(null);
    api
      .setPollerAnchor(poller.id, node?.id ?? null)
      .then(onDone)
      .catch((e: unknown) => {
        setError(errMsg(e, t('pollers.err.anchor')));
        setBusy(false);
      });
  };

  return (
    <Modal
      title={t('pollers.anchor.title', { id: poller.id })}
      onClose={onClose}
      footer={
        <>
          <Button variant="outline" onClick={onClose} disabled={busy}>
            {t('common:actions.cancel')}
          </Button>
          <Button variant="primary" onClick={save} disabled={busy}>
            {t('common:actions.save')}
          </Button>
        </>
      }
    >
      <div className="form-stack">
        <label className="form-label">
          {t('pollers.anchor.field')}
          <NodePicker
            value={node?.id ?? null}
            valueLabel={node?.name}
            onChange={setNode}
            placeholder={t('pollers.anchor.none')}
          />
        </label>
        <p className="form-hint">{t('pollers.anchor.hint')}</p>
        {poller.mgmt_addrs.length > 0 && (
          <p className="form-hint mono">
            {t('pollers.anchor.reported', { addrs: poller.mgmt_addrs.join(', ') })}
          </p>
        )}
        {error && <p className="form-error">{error}</p>}
      </div>
    </Modal>
  );
}

/** Issue a poller its own bus token and download the archive its site needs (ADR-065 Inc.3/4).
 *
 *  The download IS the token: only a digest is stored, so nothing can hand it over a second time.
 *  Revocation lives here too because it is the same subject and the same consequence — the site
 *  stops connecting until it is given a new archive. */
function PollerTokenModal({
  poller,
  onClose,
  onChanged,
}: {
  poller: PollerInfo;
  onClose: () => void;
  onChanged: () => void;
}) {
  const { t } = useTranslation('system');
  const [host, setHost] = useState('');
  // On by default (ADR-051 Inc.4 decision 15). The site can turn it off later in its own `.env`,
  // which is the file no upgrade replaces — so this is a starting point rather than a commitment.
  const [selfUpgrade, setSelfUpgrade] = useState(true);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [done, setDone] = useState(false);

  const issue = () => {
    setBusy(true);
    setError(null);
    api
      .issuePollerToken(poller.id, {
        pool: poller.pool || undefined,
        host: host.trim() || undefined,
        self_upgrade: selfUpgrade,
      })
      .then(({ blob, filename }) => {
        saveBlob(blob, filename || `yagra-poller-${poller.id}.tar.gz`);
        setDone(true);
        onChanged();
      })
      .catch((e) => setError(errMsg(e, t('pollers.token.issueFailed'))))
      .finally(() => setBusy(false));
  };

  const revoke = () => {
    setBusy(true);
    setError(null);
    api
      .revokePollerToken(poller.id)
      .then(() => {
        onChanged();
        onClose();
      })
      .catch((e) => setError(errMsg(e, t('pollers.token.revokeFailed'))))
      .finally(() => setBusy(false));
  };

  return (
    <Modal
      title={t('pollers.token.title', { id: poller.id })}
      onClose={onClose}
      footer={
        <>
          <Button variant="outline" onClick={onClose} disabled={busy}>
            {done ? t('pollers.register.done') : t('common:actions.cancel')}
          </Button>
          {poller.has_token && (
            <Button variant="danger" onClick={revoke} disabled={busy}>
              {t('pollers.token.revoke')}
            </Button>
          )}
          <Button variant="primary" onClick={issue} disabled={busy}>
            {poller.has_token ? t('pollers.token.reissue') : t('pollers.token.issue')}
          </Button>
        </>
      }
    >
      <div className="form-stack">
        <p className="modal-confirm-text">
          {poller.has_token ? t('pollers.token.introOwn') : t('pollers.token.introShared')}
        </p>
        <label className="form-label">
          {t('pollers.token.host.label')}
          <TextInput
            value={host}
            onChange={(e) => setHost(e.target.value)}
            placeholder="yagra.example.net"
          />
        </label>
        <FieldHint>{t('pollers.token.host.hint')}</FieldHint>
        {/* What the site is being asked to run is said here, before the download — not in the
            README alone, which is read at the site by whoever unpacks it and not by whoever
            decided. Ticked by default; the hint names the Docker socket rather than only the
            convenience, and says where the site can change its mind (ADR-055 R6). */}
        <label className="poller-check">
          <input
            type="checkbox"
            checked={selfUpgrade}
            disabled={busy}
            onChange={(e) => setSelfUpgrade(e.target.checked)}
          />
          <span>{t('pollers.token.selfUpgrade.label')}</span>
        </label>
        <FieldHint>{t('pollers.token.selfUpgrade.hint')}</FieldHint>
        {/* Said before the click. Re-issuing invalidates the archive the site is currently using. */}
        {poller.has_token && <p className="form-hint">{t('pollers.token.reissueWarning')}</p>}
        {done && <p className="form-hint">{t('pollers.token.downloaded')}</p>}
        {error && <p className="form-error">{error}</p>}
      </div>
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

const GAP_COLS = '1.4fr 120px 1fr 1.2fr 120px';

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
      {/* Shared `.ytable` markup rather than `DataTable`, deliberately. This page's *fleet* table
          is a `DataTable` — it is the pane's content and can own a scroll viewport. This one is a
          stacked subsection under it, and `DataTable` is `flex: 1` (`DataTable.css`), so it would
          claim height a section below another table has none of. `.ytable-scroll` gives exactly
          what a subsection wants: content height, capped at 520px. The row count is bounded
          server-side at 200 (`api/pollers.rs::monitoring_gaps`), so virtualization buys nothing
          here, and there is no filter row — which `MutesPage` names as the trigger for migrating.
          `styles/table.css` documents this markup as the v2 standard. */}
      <div className="ytable">
        <div className="ytable-scroll">
          <div className="ytable-head" style={{ gridTemplateColumns: GAP_COLS }}>
            <div className="ytable-h">{t('pollers.gaps.cols.poller')}</div>
            <div className="ytable-h">{t('pollers.gaps.cols.pool')}</div>
            <div className="ytable-h">{t('pollers.gaps.cols.window')}</div>
            <div className="ytable-h">{t('pollers.gaps.cols.passive')}</div>
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
              {/* Active polling is backfilled from the poller's buffer; passive reception is not,
                  so naming the listeners turns an unexplained silence in the event log into a
                  known loss. */}
              <div className="ytable-cell">
                {g.listeners.length === 0 ? (
                  <span className="muted">{t('pollers.gaps.passiveNone')}</span>
                ) : (
                  <span className="mono" title={t('pollers.gaps.passiveHint')}>
                    {g.listeners.join(', ')}
                  </span>
                )}
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
        /* Shared `.ytable` markup, same reason as the gaps table above: this is a drill-down panel
           stacked under the fleet `DataTable`, not the pane's content, so it needs content height
           rather than `DataTable`'s `flex: 1` viewport. The list is capped by the server and says
           so — `data.truncated` renders the "showing N of M" line above — so the paging `DataTable`
           exists to do is already answered, and two columns carry no filter row. */
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
  // The poller fleet is deployment topology rather than monitoring configuration (ADR-057).
  const canSystem = useCan('manage_system');
  const [pollers, setPollers] = useState<PollerInfo[]>([]);
  const [pools, setPools] = useState<PoolSummary[]>([]);
  const [gaps, setGaps] = useState<MonitoringGap[]>([]);
  const [block, setBlock] = useState<LoadBlock | null>(null);
  const [loading, setLoading] = useState(true);
  const [registering, setRegistering] = useState(false);
  const [deleting, setDeleting] = useState<PollerInfo | null>(null);
  const [anchoring, setAnchoring] = useState<PollerInfo | null>(null);
  const [tokenFor, setTokenFor] = useState<PollerInfo | null>(null);
  const { nodeName } = useEntityNames();
  /** The poller whose node drill-down is open (`null` ⇒ closed), and its last loaded page. */
  const [drillId, setDrillId] = useState<string | null>(null);
  const [drill, setDrill] = useState<PollerNodesResponse | null>(null);
  /** Core's own version, so a poller's can be read as "same" or "behind" rather than as a number
   *  the operator has to compare by eye. Fetched once — it cannot change without this page
   *  reloading, since core restarting is what changes it. */
  const [coreVersion, setCoreVersion] = useState<string | null>(null);
  // Client-side: a 50k-node deployment still has a handful of pollers, so the list is bounded by
  // how many were deployed rather than by fleet size (ui-conventions). The pool choices come from
  // the pools the page already loaded, not from a second list to keep in step.
  const [sheet, setSheet] = useState(false);
  const columns = useMemo<Column<PollerInfo>[]>(() => {
    const specs = pollerFilters(t, pools.map((p) => p.pool));
    const cols: Column<PollerInfo>[] = [
      {
        key: 'poller',
        header: t('pollers.cols.poller'),
        width: '1.2fr',
        render: (p) => (
          // A button, not a clickable row: a row click would fight the drill-down toggle, and a
          // button is keyboard-reachable by construction.
          <button
            type="button"
            className="poller-drill mono"
            onClick={() => setDrillId((cur) => (cur === p.id ? null : p.id))}
          >
            {p.id}
          </button>
        ),
      },
      {
        key: 'pool',
        header: t('pollers.cols.pool'),
        width: '120px',
        render: (p) => <Badge tone="neutral">{p.pool}</Badge>,
      },
      {
        key: 'status',
        header: t('pollers.cols.status'),
        width: '108px',
        render: (p) => {
          const online = p.status === 'online';
          return (
            <span className={`poller-status ${online ? 'online' : 'offline'}`}>
              <span className="poller-status-dot" />
              {online ? t('pollers.status.online') : t('pollers.status.offline')}
            </span>
          );
        },
      },
      {
        // Version skew, not just a version. A remote-site poller is upgraded on its own host, so
        // drifting behind core is the normal outcome of an upgrade and the operator has no other
        // place to notice it (ADR-051).
        key: 'version',
        header: t('pollers.cols.version'),
        width: '150px',
        render: (p) => {
          const online = p.status === 'online';
          return (
            <span className="mono">
              {p.version ?? '—'}
              {online && p.version && coreVersion && p.version !== coreVersion && (
                <>
                  {' '}
                  <Badge tone="warning" title={t('pollers.skewHint', { core: coreVersion })}>
                    {t('pollers.skew')}
                  </Badge>
                </>
              )}
              {online && p.caps.includes('self-upgrade') && (
                <>
                  {' '}
                  <Badge tone="neutral" title={t('pollers.selfUpgradeHint')}>
                    {t('pollers.selfUpgrade')}
                  </Badge>
                </>
              )}
              {/* What the site is doing right now (ADR-051 Inc.4). A WAN pull is minutes, so
                  without this the operator who pressed "bring them to this build" watches an
                  unchanged screen and cannot tell a site that is working from one that is stuck.
                  Only while it is happening: a finished report is what the version column above
                  already answers, and leaving it up would read as an operation still in flight.
                  `message` is written at the site — rendered as text, never as a key. */}
              {online && p.upgrade?.state === 'running' && (
                <>
                  {' '}
                  <Badge tone="neutral" title={p.upgrade.message || undefined}>
                    {t(`pollers.upgradeStep.${p.upgrade.command}`)}
                  </Badge>
                </>
              )}
            </span>
          );
        },
      },
      {
        key: 'workingSet',
        header: t('pollers.cols.workingSet'),
        width: '150px',
        render: (p) =>
          workingSetLabel(p.working_set_nodes, p.working_set_specs, p.status === 'online', t),
      },
      {
        key: 'results',
        header: t('pollers.cols.results'),
        width: '82px',
        align: 'right',
        render: (p) => <span className="mono">{formatCount(p.results_total)}</span>,
      },
      {
        key: 'cpu',
        header: t('pollers.cols.cpu'),
        width: '64px',
        align: 'right',
        render: (p) => <span className="mono">{formatUtil(p.cpu_pct ?? null)}</span>,
      },
      {
        key: 'mem',
        header: t('pollers.cols.mem'),
        width: '64px',
        align: 'right',
        render: (p) => <span className="mono">{formatUtil(p.mem_used_pct ?? null)}</span>,
      },
      {
        key: 'disk',
        header: t('pollers.cols.disk'),
        width: '64px',
        align: 'right',
        render: (p) => <span className="mono">{formatUtil(p.disk_used_pct ?? null)}</span>,
      },
      {
        // Date only: this is inventory ("since when has this poller existed"), so a registration
        // date beats a relative age. Exact instant on hover.
        key: 'firstSeen',
        header: t('pollers.cols.firstSeen'),
        width: '96px',
        render: (p) => (
          <span className="mono" title={p.first_seen ? formatExactTime(p.first_seen) : undefined}>
            {p.first_seen ? dateOnly(p.first_seen) : '—'}
          </span>
        ),
      },
      {
        key: 'lastSeen',
        header: t('pollers.cols.lastSeen'),
        width: '112px',
        render: (p) => lastSeenLabel(p.last_seen ?? null, p.status === 'online', t),
      },
      {
        // Editable even while the poller is online, unlike Remove: this is the operator's column,
        // not the poller's, and it is what unblocks derived suppression.
        key: 'anchor',
        header: t('pollers.cols.anchor'),
        width: '130px',
        render: (p) =>
          canSystem ? (
            <button type="button" className="poller-drill" onClick={() => setAnchoring(p)}>
              {p.anchor_node_id ? (
                <EntityName name={nodeName(p.anchor_node_id)} id={p.anchor_node_id} />
              ) : (
                t('pollers.anchor.set')
              )}
            </button>
          ) : p.anchor_node_id ? (
            <EntityName name={nodeName(p.anchor_node_id)} id={p.anchor_node_id} />
          ) : (
            '—'
          ),
      },
      {
        // Which sites still share one credential (ADR-065 Inc.3). Worth a column rather than a
        // detail: "shared" is the state that makes one site's leaked `.env` everyone's problem, and
        // an operator cannot otherwise tell the two apart.
        key: 'token',
        header: t('pollers.cols.token'),
        width: '128px',
        render: (p) =>
          canSystem ? (
            <button
              type="button"
              className="poller-drill"
              onClick={() => setTokenFor(p)}
              title={
                p.has_token && p.token_issued_at
                  ? t('pollers.token.issuedAt', { when: formatExactTime(p.token_issued_at) })
                  : t('pollers.token.sharedHint')
              }
            >
              {p.has_token ? t('pollers.token.own') : t('pollers.token.shared')}
            </button>
          ) : (
            <span className="muted">
              {p.has_token ? t('pollers.token.own') : t('pollers.token.shared')}
            </span>
          ),
      },
      {
        key: 'actions',
        header: t('pollers.cols.actions'),
        width: '58px',
        align: 'right',
        render: (p) =>
          canSystem && p.status !== 'online' ? (
            <span className="ytable-actions">
              <IconButton
                title={t('pollers.remove.title')}
                danger
                onClick={() => setDeleting(p)}
              >
                <TrashIcon />
              </IconButton>
            </span>
          ) : null,
      },
    ];
    for (const c of cols) c.filter = specs[c.key];
    return cols;
  }, [t, canSystem, pools, coreVersion, nodeName]);

  // URL-backed: the pollers table is the only filtered table on this route — the gap and
  // drill-down subsections below carry no filters of their own.
  const { filterCols, filters, setFilters, clear, shown, counts, anyFiltered } = useClientFilters(
    columns,
    pollers,
    { url: true },
  );

  // Refresh without flashing the initial loading state on every poll (loading only gates the very
  // first paint, like the sibling list pages).
  const load = useCallback(() => {
    api
      .listPollers()
      .then((res) => {
        setPollers(res.pollers);
        setPools(res.pools);
        setBlock(null);
      })
      .catch((e: unknown) => setBlock(classifyLoadError(e)))
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

  // Best-effort context, like the gaps below: if this read fails the version column simply shows
  // the number with no verdict, which is what it did before.
  useEffect(() => {
    api
      .getVersion()
      .then((v) => setCoreVersion(v.core))
      .catch(() => {});
  }, []);

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

      {block ? (
        <LoadBlockNotice block={block} unavailable={t('pollers.unavailable')} />
      ) : (
        <>
          {/* The bus itself, above the pools it carries (ADR-065). Renders nothing for a caller
              who may not manage the deployment, and nothing on a deployment with no bus
              certificate store — so a viewer's page is unchanged. */}
          <BusPanel />

          {pools.length > 0 && (
            <div className="pool-strip">
              {pools.map((p) => (
                <PoolCard key={p.pool} pool={p} />
              ))}
            </div>
          )}

          <TableToolbar>
            <FilterButton
              columns={filterCols}
              filters={filters}
              onOpen={() => setSheet(true)}
            />
            <ClearFilters columns={filterCols} filters={filters} onClear={clear} />
            <TableSpacer />
            <ResultCount
              shown={shown.length}
              total={anyFiltered ? pollers.length : undefined}
              noun={t('common:noun.poller', { count: shown.length })}
            />
            {canSystem && (
              <Button variant="primary" onClick={() => setRegistering(true)}>
                {t('pollers.registerButton')}
              </Button>
            )}
          </TableToolbar>

          <DataTable
            rows={shown}
            columns={columns}
            rowKey={(p) => p.id}
            filters={filters}
            onFiltersChange={setFilters}
            filterCounts={counts}
            loading={loading}
            empty={anyFiltered ? t('common:filter.noMatch') : t('pollers.empty.title')}
          />
          {sheet && (
            <MobileFilterSheet
              columns={filterCols}
              filters={filters}
              onChange={setFilters}
              counts={counts}
              labels={Object.fromEntries(columns.map((c) => [c.key, String(c.header)]))}
              onClose={() => setSheet(false)}
            />
          )}

          {drill && <PollerNodesSection data={drill} onClose={() => setDrillId(null)} />}

          <MonitoringGapsSection gaps={gaps} />
        </>
      )}

      {registering && <RegisterPollerModal onClose={() => setRegistering(false)} />}
      {tokenFor && (
        <PollerTokenModal
          poller={tokenFor}
          onClose={() => setTokenFor(null)}
          // Reload rather than close: after issuing, the dialog stays open showing that the
          // download happened, and the row behind it has to stop saying "shared".
          onChanged={load}
        />
      )}
      {anchoring && (
        <SetAnchorModal
          poller={anchoring}
          onClose={() => setAnchoring(null)}
          onDone={() => {
            setAnchoring(null);
            load();
          }}
        />
      )}
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
