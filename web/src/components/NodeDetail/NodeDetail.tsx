// SPDX-License-Identifier: AGPL-3.0-only
// The unified node detail — one implementation rendered both inline (right pane of the Nodes split)
// and full-page (the /nodes/:id route), so the two surfaces never drift. It owns the header
// (group-breadcrumb eyebrow · name + status pill · per-variant actions · ip/maker/seen sub line),
// the Poll-now action + transient notice, the Overview/Interfaces/Collection tab bar (with count
// pills + warning dots), and the Edit/Delete modals. Live data (status, RTT, interfaces) refreshes
// on an interval; the active tab is controlled by the caller (URL on the page, local in the split).

import { Fragment, useEffect, useState, type ReactNode } from 'react';
import { Trans, useTranslation } from 'react-i18next';
import { api, errMsg } from '../../services/api';
import { pointsToSeries, relativeTime, stateColorVar, stateLabel } from '../../lib/format';
import { groupPath } from '../../lib/nodeTree';
import { isValidPoolName } from '../../lib/pool';
import { useRefreshTick } from '../../lib/refreshTick';
import type {
  CredentialSummary,
  InterfaceRow,
  NodeDetail as NodeDetailData,
  NodeGroup,
  NodeState,
  NodeStatus,
  NodeSummary,
  ProfileSummary,
} from '../../types/api';
import { Button } from '../ui/Button';
import { ConfirmDeleteModal } from '../ui/ConfirmDeleteModal';
import { Modal } from '../ui/Modal';
import { Select, TextInput } from '../ui/Field';
import { BoxIcon } from '../ui/icons';
import { OverviewTab } from './OverviewTab';
import { InterfacesTab } from './InterfacesTab';
import { NeighborsTab } from './NeighborsTab';
import { CollectionTab } from './CollectionTab';
import { EventsTab } from './EventsTab';
import { FlowTab } from './FlowTab';
import {
  NODE_DETAIL_TAB_META,
  normalizeNodeDetailTab,
  resolveNodeDetailTab,
  visibleNodeDetailTabs,
  type NodeDetailTab,
  type NodeDetailTabStats,
} from './tabs';
import { SetParentModal } from '../SetParentModal/SetParentModal';
import { nodeSubLineParts } from './nodeIdentity';
import { NODE_KIND_SPEC } from '../../lib/nodeKind';
import './NodeDetail.css';

const RTT_WINDOW_SECS = 30 * 60;

interface Props {
  nodeId: string;
  variant: 'inline' | 'page';
  canEdit: boolean;
  /** Controlled active tab + change handler (page wires it to the URL; split keeps local state). */
  tab: string;
  onTabChange: (tab: string) => void;
  /** Group + node lists for the breadcrumb / parent-name resolution (the split already has them;
   *  the page wrapper fetches groups, and parent name falls back to a targeted fetch). */
  groups: NodeGroup[];
  nodes?: NodeSummary[];
  /** Inline only: open the move-to-group picker / jump to the full-page detail. */
  onMove?: () => void;
  onOpenDetail?: () => void;
  /** Page only: navigate away after a delete. */
  onDeleted?: () => void;
}

export function NodeDetail({
  nodeId,
  variant,
  canEdit,
  tab,
  onTabChange,
  groups,
  nodes,
  onMove,
  onOpenDetail,
  onDeleted,
}: Props) {
  const { t } = useTranslation('nodes');
  const tick = useRefreshTick();
  const [node, setNode] = useState<NodeDetailData | null>(null);
  // Kind-aware: a tab this node's kind does not show falls back to Overview. `node` is null until
  // the config load resolves, and the render below returns a loading pane until then, so the bar is
  // never painted from an unresolved kind.
  const activeTab = resolveNodeDetailTab(tab, node?.kind ?? null);
  const [status, setStatus] = useState<NodeStatus | null>(null);
  const [series, setSeries] = useState<{ timestamps: number[]; values: number[] }>({
    timestamps: [],
    values: [],
  });
  const [interfaces, setInterfaces] = useState<InterfaceRow[]>([]);
  const [ifLoaded, setIfLoaded] = useState(false);
  const [ifError, setIfError] = useState<string | null>(null);
  const [collCount, setCollCount] = useState<number | null>(null);
  const [polling, setPolling] = useState(false);
  const [pollMsg, setPollMsg] = useState<{ text: string; tone: 'info' | 'error' } | null>(null);
  const [refreshNonce, setRefreshNonce] = useState(0);
  const [editingBindings, setEditingBindings] = useState(false);
  const [editingParent, setEditingParent] = useState(false);
  const [deleting, setDeleting] = useState(false);

  // Node config (rarely changes): once per node, re-fetched after an edit (refreshNonce bump).
  useEffect(() => {
    let cancelled = false;
    setNode(null);
    api
      .getNode(nodeId)
      .then((n) => !cancelled && setNode(n))
      .catch(() => undefined);
    return () => {
      cancelled = true;
    };
  }, [nodeId, refreshNonce]);

  // Blank the live panes when the node changes (or after an edit) so a switch never flashes the
  // previous node's readings. A periodic refresh must NOT blank — that would flicker every tick —
  // so the reset is its own effect keyed on identity, separate from the load below.
  useEffect(() => {
    setStatus(null);
    setSeries({ timestamps: [], values: [] });
    setIfLoaded(false);
  }, [nodeId, refreshNonce]);

  // Whether this node can have interfaces at all. Gates the eager fetch below: a URL/DNS/Meraki
  // node never gets an ifTable walk, so asking every refresh tick is a request that can only ever
  // come back empty. Undecided while the config is still loading — the pane is not rendered yet.
  const hasInterfaces = node != null && visibleNodeDetailTabs(node.kind).includes('interfaces');

  // The series that says "we heard from this node". Kind-driven: only an ordinary device is pinged,
  // so asking every kind for `icmp_rtt_ms` meant a URL monitor, a DNS monitor and a Meraki device
  // each had a permanently empty series — and therefore no "seen …" in the header. Only a device's
  // is also charted (Overview's ICMP sparkline); for the rest it is read for the timestamp alone.
  const livenessMetric = node ? NODE_KIND_SPEC[node.kind].livenessMetric : null;

  // Live data: status + liveness history + interfaces. Loads on node change and on every shared
  // refresh tick (S24 — one clock for the whole detail instead of a per-card setInterval). Each tick
  // re-runs this effect, so its cleanup cancels an in-flight round before the next one starts.
  useEffect(() => {
    let cancelled = false;
    const to = Math.floor(Date.now() / 1000);
    api
      .getNodeStatus(nodeId)
      .then((s) => !cancelled && setStatus(s))
      .catch(() => undefined);
    if (livenessMetric) {
      api
        .getNodeMetricRange(nodeId, livenessMetric, { from: to - RTT_WINDOW_SECS, to })
        .then((r) => !cancelled && setSeries(pointsToSeries(r.points)))
        .catch(() => undefined);
    }
    if (!hasInterfaces) {
      // Settle the badge state rather than leaving it "loading" forever for a kind that has none.
      setInterfaces([]);
      setIfError(null);
      setIfLoaded(true);
      return () => {
        cancelled = true;
      };
    }
    api
      .listNodeInterfaces(nodeId)
      .then((r) => {
        if (cancelled) return;
        setInterfaces(r);
        setIfError(null);
        setIfLoaded(true);
      })
      .catch((e: unknown) => {
        if (cancelled) return;
        setIfError(errMsg(e, t('err.loadInterfaces')));
        setIfLoaded(true);
      });
    return () => {
      cancelled = true;
    };
  }, [nodeId, refreshNonce, tick, t, hasInterfaces, livenessMetric]);

  // Correct a tab the loaded kind does not show, so the URL (page) or pane state (split) matches
  // what is drawn instead of quietly diverging. Comparing the *normalized* value is deliberate:
  // `?tab=bogus` keeps today's behaviour of rendering Overview and leaving the param alone, and
  // only a real-but-inapplicable tab is rewritten. It cannot loop — after the correction the two
  // sides agree, and 'overview' is visible to every kind (pinned in tabs.test.ts).
  useEffect(() => {
    if (!node) return;
    const requested = normalizeNodeDetailTab(tab);
    const allowed = resolveNodeDetailTab(tab, node.kind);
    if (requested !== allowed) onTabChange(allowed);
  }, [node, tab, onTabChange]);

  // Collection-set count for the Collection tab badge (the profile's attached templates).
  useEffect(() => {
    let cancelled = false;
    setCollCount(null);
    if (node?.profile_id) {
      api
        .listProfileTemplates(node.profile_id)
        .then((ts) => !cancelled && setCollCount(ts.length))
        .catch(() => undefined);
    }
    return () => {
      cancelled = true;
    };
  }, [node?.profile_id]);

  // Poll now: dispatch an immediate poll, then re-fetch the readings a few seconds later.
  const pollNow = () => {
    setPolling(true);
    setPollMsg(null);
    api
      .pollNode(nodeId)
      .then((r) => {
        const n = r.dispatched;
        setPollMsg({
          text: t('detail.pollDispatched', { count: n }),
          tone: 'info',
        });
        window.setTimeout(() => setRefreshNonce((v) => v + 1), 4000);
        window.setTimeout(() => setPollMsg(null), 8000);
      })
      .catch((e: unknown) => setPollMsg({ text: errMsg(e, t('err.requestPoll')), tone: 'error' }))
      .finally(() => setPolling(false));
  };

  if (!node) {
    return (
      <div className="nd">
        <div className="nd-body nd-tabpad">
          <p className="nd-muted">{t('common:loading')}</p>
        </div>
      </div>
    );
  }

  const state = status?.state ?? 'unknown';
  const path = groupPath(groups, node.group_id ?? null);
  const lastSeen = series.timestamps.at(-1);
  // What the tab bar decorates itself from (count pills / warning dots) — the rules themselves live
  // beside the whitelist in tabs.ts.
  const tabStats: NodeDetailTabStats = {
    interfaces,
    collCount,
    hasCredential: !!node.credential_id,
    state,
  };
  // Keyed by NodeDetailTab, so adding a tab to NODE_DETAIL_TABS without a body fails to compile.
  const tabBodies: Record<NodeDetailTab, ReactNode> = {
    overview: (
      <OverviewTab
        node={node}
        groups={groups}
        nodes={nodes}
        status={status}
        series={series}
        unreachable={state === 'unreachable'}
        canEdit={canEdit}
        onChanged={() => setRefreshNonce((v) => v + 1)}
      />
    ),
    interfaces: (
      <InterfacesTab nodeId={node.id} rows={interfaces} loaded={ifLoaded} error={ifError} />
    ),
    neighbors: <NeighborsTab node={node} />,
    collection: <CollectionTab node={node} canEdit={canEdit} />,
    events: <EventsTab node={node} />,
    flow: <FlowTab node={node} />,
  };

  return (
    <div className="nd">
      <div className="nd-head">
        <div className="nd-eyebrow">
          <BoxIcon width={12} height={12} /> {path.length ? path.join(' / ') : t('ungrouped')}
        </div>
        <div className="nd-namerow">
          <div className="nd-namewrap">
            <span className="nd-name">{node.name}</span>
            {/* What this node is, when it is not an ordinary device. Unmarked is the default so a
                normal device's name is not decorated — the badge means "read this differently". */}
            {NODE_KIND_SPEC[node.kind].badge && (
              <span className="nd-kind" title={t(NODE_KIND_SPEC[node.kind].labelKey)}>
                {NODE_KIND_SPEC[node.kind].badge}
              </span>
            )}
            {status && <StatePill state={state} />}
          </div>
          <div className="nd-actions">
            {canEdit && (
              <Button variant="outline" onClick={pollNow} disabled={polling}>
                {polling ? t('detail.polling') : t('detail.pollNow')}
              </Button>
            )}
            {canEdit && (
              <Button variant="outline" onClick={() => setEditingParent(true)}>
                {t('detail.dependency')}
              </Button>
            )}
            {variant === 'inline' && canEdit && onMove && (
              <Button variant="outline" onClick={onMove}>
                {t('detail.move')}
              </Button>
            )}
            {variant === 'inline' && onOpenDetail && (
              <Button variant="outline" onClick={onOpenDetail}>
                {t('detail.openDetail')}
              </Button>
            )}
            {variant === 'page' && canEdit && (
              <Button variant="outline" onClick={() => setEditingBindings(true)}>
                {t('detail.editNode')}
              </Button>
            )}
            {variant === 'page' && canEdit && (
              <Button variant="danger" onClick={() => setDeleting(true)}>
                {t('common:actions.delete')}
              </Button>
            )}
          </div>
        </div>
        <div className="nd-sub">
          {nodeSubLineParts(node, t).map((part, i) => (
            <Fragment key={part.id}>
              {i > 0 && <span className="nd-sep">·</span>}
              <span className={part.mono ? 'mono' : undefined}>{part.text}</span>
            </Fragment>
          ))}
          {lastSeen != null && (
            <>
              <span className="nd-sep">·</span>
              <span>
                {t('detail.seen', {
                  time: relativeTime(new Date(lastSeen * 1000).toISOString()),
                })}
              </span>
            </>
          )}
        </div>
      </div>

      {pollMsg && (
        <p className={`nd-pollmsg${pollMsg.tone === 'error' ? ' err' : ''}`}>{pollMsg.text}</p>
      )}

      <div className="nd-tabs" role="tablist">
        {visibleNodeDetailTabs(node.kind).map((key) => {
          const meta = NODE_DETAIL_TAB_META[key];
          const n = meta.badge?.(tabStats) ?? null;
          return (
            <button
              key={key}
              type="button"
              role="tab"
              aria-selected={activeTab === key}
              className={`nd-tab${activeTab === key ? ' on' : ''}`}
              onClick={() => onTabChange(key)}
            >
              {t(meta.labelKey)}
              {n != null && <span className="nd-tab-n">{n}</span>}
              {meta.warn?.(tabStats) && (
                <span className="nd-tab-warn" aria-label={t('detail.needsAttention')}>
                  ●
                </span>
              )}
            </button>
          );
        })}
      </div>

      <div className="nd-body">{tabBodies[activeTab]}</div>

      {editingBindings && (
        <BindingsModal
          nodeId={nodeId}
          onClose={() => setEditingBindings(false)}
          onDone={() => {
            setEditingBindings(false);
            setRefreshNonce((v) => v + 1);
          }}
        />
      )}
      {editingParent && (
        <SetParentModal
          nodeId={nodeId}
          nodeName={node.name}
          currentParentId={node.parent_id ?? null}
          onClose={() => setEditingParent(false)}
          onSaved={() => {
            setEditingParent(false);
            setRefreshNonce((v) => v + 1);
          }}
        />
      )}
      {deleting && (
        <DeleteNodeModal
          nodeId={nodeId}
          name={node.name}
          onClose={() => setDeleting(false)}
          onDeleted={() => onDeleted?.()}
        />
      )}
    </div>
  );
}

/** Rounded status pill (dot + label, bordered in the state color) shown beside the node name. */
function StatePill({ state }: { state: NodeState }) {
  return (
    <span className="nd-statepill" style={{ color: stateColorVar(state) }}>
      <span className="nd-statepill-dot" />
      {stateLabel(state)}
    </span>
  );
}

/** Confirm + delete a node (destructive-consent modal). On success the caller navigates away
 *  (full-page detail) or clears the selection + reloads (the All-nodes tree right-click path). */
export function DeleteNodeModal({
  nodeId,
  name,
  onClose,
  onDeleted,
}: {
  nodeId: string;
  name: string;
  onClose: () => void;
  onDeleted: () => void;
}) {
  const { t } = useTranslation('nodes');
  return (
    <ConfirmDeleteModal
      title={t('deleteNode.title')}
      onConfirm={() => api.deleteNode(nodeId)}
      errorFallback={t('err.deleteNode')}
      onClose={onClose}
      onDone={onDeleted}
    >
      <Trans t={t} i18nKey="deleteNode.body" values={{ name }} components={{ b: <strong /> }} />
    </ConfirmDeleteModal>
  );
}

/** Edit a node: device profile + SNMP credential bindings and its descriptive maker/model.
 *  Pre-fills the current values; saving resends all so an unchanged field is preserved. */
function BindingsModal({
  nodeId,
  onClose,
  onDone,
}: {
  nodeId: string;
  onClose: () => void;
  onDone: () => void;
}) {
  const { t } = useTranslation('nodes');
  const [profiles, setProfiles] = useState<ProfileSummary[]>([]);
  const [credentials, setCredentials] = useState<CredentialSummary[]>([]);
  const [profileId, setProfileId] = useState('');
  const [credentialId, setCredentialId] = useState('');
  const [vendor, setVendor] = useState('');
  const [model, setModel] = useState('');
  // The node's OWN pool ('' ⇒ inherited), plus what it would inherit if cleared — used as the
  // field placeholder so blanking it is a visible choice rather than a mystery.
  const [pool, setPool] = useState('');
  const [inheritedPool, setInheritedPool] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const poolInvalid = !isValidPoolName(pool);

  useEffect(() => {
    api
      .getNode(nodeId)
      .then((n) => {
        setProfileId(n.profile_id ?? '');
        setCredentialId(n.credential_id ?? '');
        setVendor(n.vendor ?? '');
        setModel(n.model ?? '');
        setPool(n.pool ?? '');
      })
      .catch(() => undefined);
    // The effective pool doubles as the "what you'd inherit" hint. Asking the server keeps the
    // folder walk in one place (yagra-core's resolver) instead of duplicating it here.
    api
      .getNodeAssignment(nodeId)
      .then((a) => setInheritedPool(a.pool_source === 'node' ? null : a.pool))
      .catch(() => undefined);
    api.listProfiles().then(setProfiles).catch(() => setProfiles([]));
    api
      .listCredentials()
      .then((c) => setCredentials(c.filter((cr) => cr.kind === 'snmp_v2c')))
      .catch(() => setCredentials([]));
  }, [nodeId]);

  const save = () => {
    setBusy(true);
    setError(null);
    api
      .setNodeBindings(nodeId, {
        profile_id: profileId || null,
        credential_id: credentialId || null,
        vendor: vendor.trim() || null,
        model: model.trim() || null,
        // Always sent (never null): '' clears the assignment back to inherited. A JSON null would
        // read server-side as "leave unchanged" and silently drop the edit.
        pool: pool.trim(),
      })
      .then(onDone)
      .catch((e: unknown) => {
        setError(errMsg(e, t('err.saveNode')));
        setBusy(false);
      });
  };

  return (
    <Modal
      title={t('detail.editNode')}
      onClose={onClose}
      footer={
        <>
          <Button variant="outline" onClick={onClose}>
            {t('common:actions.cancel')}
          </Button>
          <Button variant="primary" onClick={save} disabled={busy || poolInvalid}>
            {t('common:actions.save')}
          </Button>
        </>
      }
    >
      <div className="form-stack">
        <label className="form-label">
          {t('field.deviceProfile')}
          <Select value={profileId} onChange={(e) => setProfileId(e.target.value)}>
            <option value="">{t('add.none')}</option>
            {profiles.map((p) => (
              <option key={p.id} value={p.id}>
                {p.name}
              </option>
            ))}
          </Select>
        </label>
        <label className="form-label">
          {t('field.snmpCredential')}
          <Select value={credentialId} onChange={(e) => setCredentialId(e.target.value)}>
            <option value="">{t('add.none')}</option>
            {credentials.map((c) => (
              <option key={c.id} value={c.id}>
                {c.name}
              </option>
            ))}
          </Select>
        </label>
        <div className="form-row">
          <label className="form-label">
            {t('field.maker')}
            <TextInput
              value={vendor}
              onChange={(e) => setVendor(e.target.value)}
              placeholder={t('field.makerPlaceholder')}
            />
          </label>
          <label className="form-label">
            {t('field.model')}
            <TextInput
              className="mono"
              value={model}
              onChange={(e) => setModel(e.target.value)}
              placeholder={t('field.modelPlaceholder')}
            />
          </label>
        </div>
        <label className="form-label">
          {t('field.pool')}
          <TextInput
            className="mono"
            value={pool}
            onChange={(e) => setPool(e.target.value)}
            placeholder={
              inheritedPool ? t('field.poolInheritPlaceholder', { pool: inheritedPool }) : ''
            }
          />
          <span className={`form-hint${poolInvalid ? ' form-hint-error' : ''}`}>
            {poolInvalid ? t('field.poolInvalid') : t('field.poolHint')}
          </span>
        </label>
        {error && <p className="form-error">{error}</p>}
      </div>
    </Modal>
  );
}
