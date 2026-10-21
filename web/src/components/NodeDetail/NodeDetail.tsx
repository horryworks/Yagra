// SPDX-License-Identifier: AGPL-3.0-only
// The unified node detail — one implementation rendered both inline (right pane of the Nodes split)
// and full-page (the /nodes/:id route), so the two surfaces never drift. It owns the header
// (group-breadcrumb eyebrow · name + status pill · per-variant actions · ip/maker/seen sub line),
// the Poll-now action + transient notice, the Overview/Interfaces/Collection tab bar (with count
// pills + warning dots), and the Edit/Delete modals. Live data (status, RTT, interfaces) refreshes
// on an interval; the active tab is controlled by the caller (URL on the page, local in the split).

import { Fragment, useEffect, useMemo, useRef, useState, type ReactNode } from 'react';
import { useConfigChanges } from '../../lib/configChanges';
import { Trans, useTranslation } from 'react-i18next';
import { useNavigate } from 'react-router-dom';
import { topologyMapHref } from '../../lib/entityHref';
import { api, errMsg } from '../../services/api';
import { pointsToSeries, relativeTime, stateColorVar, stateLabel } from '../../lib/format';
import { groupTrail } from '../../lib/nodeTree';
import { GroupCrumbs } from './GroupCrumbs';
import { POLL_NOW_REFRESH_MS } from './pollNowRefresh';
import { useRefreshTick } from '../../lib/refreshTick';
import { useNodeTabStore } from '../../store';
import type {
  InterfaceRow,
  NodeDetail as NodeDetailData,
  NodeGroup,
  NodeState,
  NodeStatus,
  NodeSummary,
} from '../../types/api';
import { Button } from '../ui/Button';
import { ConfirmDeleteModal } from '../ui/ConfirmDeleteModal';
import { BoxIcon } from '../ui/icons';
import { EditNodeModal } from './EditNodeModal';
import { OverviewTab } from './OverviewTab';
import { InterfacesTab } from './InterfacesTab';
import { NeighborsTab } from './NeighborsTab';
import { ApTab } from './ApTab';
import { CollectionTab } from './CollectionTab';
import { EventsTab } from './EventsTab';
import { FlowTab } from './FlowTab';
import {
  NODE_DETAIL_TAB_META,
  normalizeNodeDetailTab,
  resolveNodeDetailTab,
  visibleNodeDetailTabs,
  type NodeDetailSubject,
  type NodeDetailTab,
  type NodeDetailTabStats,
} from './tabs';
import { SetParentModal } from '../SetParentModal/SetParentModal';
import { ActionMenu, type ActionMenuItem } from '../ui/ActionMenu';
import { AddMaintenanceWindowModal } from '../suppression/AddMaintenanceWindowModal';
import { AddMuteModal } from '../suppression/AddMuteModal';
import { SetPoolModal } from '../SetPoolModal/SetPoolModal';
import { RediscoverModal } from '../NodeTree/RediscoverModal';
import { NameFromDeviceModal } from '../NameFromDeviceModal/NameFromDeviceModal';
import { MoveNodeModal } from '../MoveNodeModal/MoveNodeModal';
import { MoveByPrefixModal } from '../MoveByPrefixModal/MoveByPrefixModal';
import { fallbackPool } from '../../lib/pool';
import { usePinsStore } from '../../pinsStore';
import { useCan } from '../../store';
import {
  nodeHeaderActions,
  type NodeHeaderDialog,
  type NodeHeaderMore,
} from './nodeHeaderActions';
import { nodeSubLineParts } from './nodeIdentity';
import { NODE_KIND_SPEC, nodeBadges } from '../../lib/nodeKind';
import { NodeBadgeTag } from '../ui/NodeBadgeTag';
import type { MoveTarget } from '../MoveNodeModal/MoveNodeModal';
import './NodeDetail.css';

const RTT_WINDOW_SECS = 30 * 60;

interface Props {
  nodeId: string;
  variant: 'inline' | 'page';
  canEdit: boolean;
  /** Controlled active tab + change handler. **Both hosts wire it to the URL** (`?tab=`), falling
   *  back to the session's remembered tab when the URL names none — see `requestedNodeDetailTab`.
   *  (This used to say the split kept local state; it has been URL-backed since ADR-055.) */
  tab: string;
  onTabChange: (tab: string) => void;
  /** Group + node lists for the breadcrumb / parent-name resolution (the split already has them;
   *  the page wrapper fetches groups, and parent name falls back to a targeted fetch). */
  groups: NodeGroup[];
  nodes?: NodeSummary[];
  /** Open the move-to-group picker; without it the pane opens its own. Handed the node this pane
   *  has loaded. 🚨 The host
   *  used to look the node up among its own loaded rows instead, and the button did nothing
   *  whenever it was not there — a deep link into a folder below the fold, or a selection the
   *  active filter no longer matches. */
  onMove?: (node: MoveTarget) => void;
  /** Inline only: jump to the full-page detail. */
  onOpenDetail?: () => void;
  /** Open a folder from the eyebrow breadcrumb (ADR-142) — a pane in the split, All nodes on the
   *  route. Absent ⇒ the breadcrumb stays plain text. */
  onOpenGroup?: (groupId: string) => void;
  /** After a delete: the page navigates away, the split clears its selection. */
  onDeleted?: () => void;
  /** 🚨 **Something about this node changed and the surrounding page shows it too.**
   *
   *  The inline variant sits beside the inventory tree, whose rows carry the node's **name** and
   *  **pool** — both editable from this pane's own dialog. Without this, an edit refreshed the
   *  right-hand pane and left the row on the left saying what it said before, until something
   *  else happened to refetch. That was already true for the pool; ADR-135 made it true for the
   *  name, which nobody would read as anything but a broken save. */
  onChanged?: () => void;
  /** Bumped by the host when something it did changed this node (an edit or a rediscover from the
   *  tree's menu, a bulk tag). Read like `quietNonce`: the pane re-reads and keeps what it shows
   *  until the answer lands. 🚨 The host used to put a nonce in this component's `key` instead,
   *  which remounted the pane, reset its scroll and closed the Interfaces dock (ADR-203). */
  reloadKey?: number;
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
  onOpenGroup,
  onDeleted,
  onChanged,
  reloadKey = 0,
}: Props) {
  const { t } = useTranslation('nodes');
  const navigate = useNavigate();
  const tick = useRefreshTick();
  const [node, setNode] = useState<NodeDetailData | null>(null);
  // What the tab rules are asked about. `null` until the config load resolves — the render below
  // returns a loading pane until then, so the bar is never painted from an unresolved node. All
  // three fields come off the same fetch, so there is no window where one is known and another is
  // not — including `wireless.controller`, which the node read fills in (ADR-064 Inc.B3).
  // Memoized so it is referentially stable: the tab-correction effect below depends on it, and a
  // fresh object every render would re-run that effect — which writes the URL — on every render.
  const subject: NodeDetailSubject | null = useMemo(
    () =>
      node
        ? {
            kind: node.kind,
            snmpConfigured: node.snmp_configured,
            isWlanController: node.wireless?.controller != null,
            merakiProductType: node.meraki_device?.product_type ?? null,
          }
        : null,
    [node],
  );
  // A tab this node does not show falls back to Overview.
  const activeTab = resolveNodeDetailTab(tab, subject);
  // Written by the tab bar's onClick alone — see the comment there.
  const rememberTab = useNodeTabStore((s) => s.rememberTab);
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
  // A re-read that does NOT blank the pane — after **Poll now**, and since ADR-203 after an edit too.
  // There used to be a second, blanking nonce for edits: it put the pane back to "Loading…", which
  // unmounted every tab, so the scroll went back to the top and the Interfaces dock closed after
  // every save. The pane is blanked only when it is about a different node (the effect below).
  // ⚠️ A tab that reads something an edit can change keys that read on the node's fields (pool,
  // folder, profile, credential), since it is no longer remounted to ask again.
  const [quietNonce, setQuietNonce] = useState(0);
  const pollTimers = useRef<number[]>([]);
  // The timers outlived the pane: on the un-keyed page route they fired into the NEXT node.
  useEffect(
    () => () => {
      for (const h of pollTimers.current) window.clearTimeout(h);
    },
    [],
  );
  const [editingBindings, setEditingBindings] = useState(false);
  const [editingParent, setEditingParent] = useState(false);
  const [deleting, setDeleting] = useState(false);
  // The dialogs the header opens that used to be reachable only from the tree's right-click menu
  // (ADR-209 decision 8). Held here, like the three above, so the pane and the full page have them
  // alike; the tree keeps its own for the selection and for folders.
  const [dialog, setDialog] = useState<NodeHeaderDialog | null>(null);
  const canMaintenance = useCan('manage_maintenance');
  const canMute = useCan('ack_alerts');
  const pinsReady = usePinsStore((s) => s.status === 'ready');
  const pinned = usePinsStore((s) => s.nodeIds.has(nodeId));
  const setNodePinned = usePinsStore((s) => s.setNodePinned);
  /** After a dialog wrote something about this node: re-read the pane, tell the page. */
  const changed = () => {
    setQuietNonce((v) => v + 1);
    onChanged?.();
  };

  // Node config (rarely changes): once per node, re-fetched quietly after an edit or a Poll now,
  // whose identity read (ADR-149) lands on this same document, or when someone else changes the
  // configuration (ADR-019 Inc.2).
  const configChanges = useConfigChanges();
  const blankedFor = useRef<string | null>(null);
  useEffect(() => {
    let cancelled = false;
    // Blank for a new node; a re-read keeps what is on screen until it answers.
    if (blankedFor.current !== nodeId) {
      blankedFor.current = nodeId;
      setNode(null);
    }
    api
      .getNode(nodeId)
      .then((n) => !cancelled && setNode(n))
      .catch(() => undefined);
    return () => {
      cancelled = true;
    };
  }, [nodeId, quietNonce, reloadKey, configChanges]);

  // Blank the live panes when the node changes so a switch never flashes the previous node's
  // readings. A periodic refresh must NOT blank — that would flicker every tick — so the reset is
  // its own effect keyed on identity, separate from the load below.
  //
  // 🚨 `interfaces` and `ifError` belong here and were missing. The page route does not remount
  // this component per node (the split view does), so node B's Interfaces tab listed node A's
  // ports under B's name until B's answer landed — and for good when B's read failed, because the
  // failure path sets the error and leaves the rows. `InterfacesTab` draws rows whatever `loaded`
  // says; only `loaded && rows.length === 0` is its empty state.
  useEffect(() => {
    setStatus(null);
    setSeries({ timestamps: [], values: [] });
    setInterfaces([]);
    setIfError(null);
    setIfLoaded(false);
  }, [nodeId]);

  // Whether this node can have interfaces at all. Gates the eager fetch below: a URL/DNS/Meraki
  // node never gets an ifTable walk, and neither does a ping-only device, so asking every refresh
  // tick is a request that can only ever come back empty. Undecided while the config is still
  // loading — the pane is not rendered yet.
  const hasInterfaces = subject != null && visibleNodeDetailTabs(subject).includes('interfaces');

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
  }, [nodeId, quietNonce, reloadKey, tick, t, hasInterfaces, livenessMetric]);

  // Correct a tab the loaded kind does not show, so the URL (page) or pane state (split) matches
  // what is drawn instead of quietly diverging. Comparing the *normalized* value is deliberate:
  // `?tab=bogus` keeps today's behaviour of rendering Overview and leaving the param alone, and
  // only a real-but-inapplicable tab is rewritten. It cannot loop — after the correction the two
  // sides agree, and 'overview' is visible to every node (pinned in tabs.test.ts).
  useEffect(() => {
    if (!subject) return;
    const requested = normalizeNodeDetailTab(tab);
    const allowed = resolveNodeDetailTab(tab, subject);
    if (requested !== allowed) onTabChange(allowed);
  }, [subject, tab, onTabChange]);

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

  // Poll now: dispatch an immediate poll, then re-fetch the readings a few seconds later and again
  // once the identity and row names the same press reads have had time to arrive (ADR-149).
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
        for (const ms of POLL_NOW_REFRESH_MS) {
          pollTimers.current.push(window.setTimeout(() => setQuietNonce((v) => v + 1), ms));
        }
        pollTimers.current.push(window.setTimeout(() => setPollMsg(null), 8000));
      })
      .catch((e: unknown) => setPollMsg({ text: errMsg(e, t('err.requestPoll')), tone: 'error' }))
      .finally(() => setPolling(false));
  };

  // Both, though the memo makes them resolve together: naming `subject` here is what lets the
  // render below pass it straight to the tab rules instead of carrying a fallback that can
  // never run.
  if (!node || !subject) {
    return (
      <div className="nd">
        <div className="nd-body nd-tabpad">
          <p className="nd-muted">{t('common:loading')}</p>
        </div>
      </div>
    );
  }

  const state = status?.state ?? 'unknown';
  const header = nodeHeaderActions(
    { canConfig: canEdit, canMaintenance, canMute, pinsReady, inline: variant === 'inline' },
    node,
    groups,
  );
  /** The node as the suppression and pool dialogs name their target. */
  const self = { kind: 'node' as const, id: node.id, name: node.name };
  const moreItem = (key: NodeHeaderMore): ActionMenuItem => {
    switch (key) {
      case 'openDetail':
        return { key, label: t('detail.openDetail'), onSelect: () => onOpenDetail?.() };
      case 'map':
        // The level holding this node, with the node selected (ADR-191).
        return {
          key,
          label: t('detail.showOnMap'),
          onSelect: () =>
            navigate(
              topologyMapHref({ group: node.group_id ?? null, sel: { kind: 'node', id: node.id } }),
            ),
        };
      case 'pin':
        return {
          key,
          label: pinned ? t('detail.unpin') : t('detail.pin'),
          onSelect: () =>
            void setNodePinned(nodeId, !pinned).catch((e: unknown) =>
              setPollMsg({ text: errMsg(e, t('tree.pinFailed')), tone: 'error' }),
            ),
        };
      case 'upstream':
        return { key, label: t('detail.dependency'), onSelect: () => setEditingParent(true) };
      case 'move':
        return {
          key,
          label: t('detail.move'),
          onSelect: () =>
            onMove
              ? onMove({ id: node.id, name: node.name, group_id: node.group_id ?? null })
              : setDialog('move'),
        };
      case 'delete':
        return { key, label: t('detail.delete'), danger: true, onSelect: () => setDeleting(true) };
      case 'moveByPrefix':
      case 'pool':
      case 'rediscover':
      case 'nameFromDevice':
        return { key, label: t(`detail.${key}`), onSelect: () => setDialog(key) };
    }
  };
  const trail = groupTrail(groups, node.group_id ?? null);
  const lastSeen = series.timestamps.at(-1);
  // What the tab bar decorates itself from (count pills / warning dots) — the rules themselves live
  // beside the whitelist in tabs.ts.
  const tabStats: NodeDetailTabStats = {
    interfaces,
    collCount,
    hasSnmp: node.snmp_configured,
    state,
    wlanController: node.wireless?.controller ?? null,
  };
  // Keyed by NodeDetailTab, so adding a tab to NODE_DETAIL_TABS without a body fails to compile.
  const tabBodies: Record<NodeDetailTab, ReactNode> = {
    overview: (
      <OverviewTab
        node={node}
        groups={groups}
        nodes={nodes}
        status={status}
        unreachable={state === 'unreachable'}
        canEdit={canEdit}
        onChanged={() => setQuietNonce((v) => v + 1)}
      />
    ),
    // Takes `onChanged` for the same reason Overview does: importing an AP creates a node, which
    // changes this page's own folder contents and the tree beside it.
    ap: (
      <ApTab
        node={node}
        groups={groups}
        // Quiet: importing an AP changes nothing this pane draws from the controller's own
        // document, and a remount would take the "imported" note with it.
        onChanged={() => setQuietNonce((v) => v + 1)}
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
          <BoxIcon width={12} height={12} />{' '}
          {trail.length ? (
            // Every segment is a link here, the last included: it is the node's folder, not this pane.
            <GroupCrumbs trail={trail} onOpenGroup={onOpenGroup} linkLast />
          ) : (
            t('ungrouped')
          )}
        </div>
        <div className="nd-namerow">
          <div className="nd-namewrap">
            <span className="nd-name">{node.name}</span>
            {/* What this node is, when it is not an ordinary device. Unmarked is the default so a
                normal device's name is not decorated — the badge means "read this differently". */}
            {nodeBadges({
              kind: node.kind,
              merakiProductType: node.meraki_device?.product_type,
              merakiRepeater: node.meraki_repeater,
            }).map((badge) => (
              <NodeBadgeTag
                key={badge.text}
                badge={badge}
                className="nd-kind"
                label={t(badge.labelKey)}
              />
            ))}
            {status && <StatePill state={state} />}
          </div>
          {/* Four buttons and "More" (ADR-209 decision 8), the same set in the split and on the
              page. Which ones, and in what order, is `nodeHeaderActions` — tested, and gated on the
              permission each action's handler checks (ADR-056). The tree's right-click menu stays
              as the shortcut. */}
          <div className="nd-actions">
            {header.primary.map((key) =>
              key === 'pollNow' ? (
                <Button key={key} variant="outline" onClick={pollNow} disabled={polling}>
                  {polling ? t('detail.polling') : t('detail.pollNow')}
                </Button>
              ) : (
                <Button
                  key={key}
                  variant="outline"
                  onClick={() =>
                    key === 'edit' ? setEditingBindings(true) : setDialog(key)
                  }
                >
                  {key === 'edit' ? t('detail.editNode') : t(`detail.${key}`)}
                </Button>
              ),
            )}
            <ActionMenu
              label={t('detail.more')}
              align="end"
              items={header.more.map((key) => moreItem(key))}
              trigger={(p) => (
                <Button {...p} variant="outline">
                  {t('detail.more')} ▾
                </Button>
              )}
            />
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
        {visibleNodeDetailTabs(subject).map((key) => {
          const meta = NODE_DETAIL_TAB_META[key];
          const n = meta.badge?.(tabStats) ?? null;
          return (
            <button
              key={key}
              type="button"
              role="tab"
              aria-selected={activeTab === key}
              className={`nd-tab${activeTab === key ? ' on' : ''}`}
              // 🚨 A **click** is the only thing that writes the memory (ADR-134 decision 2). The
              // correction effect above rewrites a tab the loaded node does not offer, and if that
              // recorded too, walking a row of switches on Interfaces with one URL monitor among
              // them would leave the memory on Overview — so the memory would mean "the last screen
              // I was dropped onto" instead of "the last one I chose".
              onClick={() => {
                rememberTab(key);
                onTabChange(key);
              }}
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
        <EditNodeModal
          node={node}
          onClose={() => setEditingBindings(false)}
          onDone={() => {
            setEditingBindings(false);
            setQuietNonce((v) => v + 1);
            // Both, not either: the nonce refetches this pane, `onChanged` tells the page that the
            // row it is drawing on the left is now stale too.
            onChanged?.();
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
            setQuietNonce((v) => v + 1);
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
      {dialog === 'maintenance' && (
        <AddMaintenanceWindowModal
          groups={groups}
          initialScope={self}
          onClose={() => setDialog(null)}
          onSaved={changed}
        />
      )}
      {dialog === 'mute' && (
        <AddMuteModal
          groups={groups}
          initialScope={self}
          onClose={() => setDialog(null)}
          onSaved={changed}
        />
      )}
      {dialog === 'pool' && (
        <SetPoolModal
          target={self}
          currentPool={node.pool ?? null}
          inheritedPool={fallbackPool(groups, node.group_id ?? null)}
          onClose={() => setDialog(null)}
          onSaved={() => {
            setDialog(null);
            changed();
          }}
        />
      )}
      {dialog === 'rediscover' && (
        <RediscoverModal
          nodeId={nodeId}
          name={node.name}
          onClose={() => setDialog(null)}
          onApplied={() => {
            setDialog(null);
            changed();
          }}
        />
      )}
      {dialog === 'nameFromDevice' && (
        <NameFromDeviceModal
          nodeIds={[nodeId]}
          onClose={() => setDialog(null)}
          onApplied={() => {
            setDialog(null);
            changed();
          }}
        />
      )}
      {dialog === 'move' && (
        <MoveNodeModal
          targets={[{ id: node.id, name: node.name, group_id: node.group_id ?? null }]}
          groups={groups}
          onClose={() => setDialog(null)}
          onMoved={changed}
        />
      )}
      {dialog === 'moveByPrefix' && (
        <MoveByPrefixModal
          source={{
            kind: 'nodes',
            targets: [
              {
                id: node.id,
                name: node.name,
                address: node.address,
                state,
                vendor: node.vendor ?? null,
                model: node.model ?? null,
                group_id: node.group_id ?? null,
                sort_order: 0,
                kind: node.kind,
              },
            ],
          }}
          groups={groups}
          onClose={() => setDialog(null)}
          onMoved={changed}
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
