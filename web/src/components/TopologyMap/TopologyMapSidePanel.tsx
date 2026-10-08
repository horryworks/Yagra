// SPDX-License-Identifier: AGPL-3.0-only
// The network map's right-hand panel (ADR-191): what the level holds when nothing is selected,
// one node's lines when a node is, the ports behind a bundled line when a line is, and the access
// points behind a bundle (ADR-191 Inc.9), port by port, when a bundle is.
//
// Every name here is device-supplied and renders as a React text child (auto-escaped).

import { useTranslation } from 'react-i18next';
import { Link } from 'react-router-dom';
import type { MapLevel } from '../../types/api';
import { LINK_SOURCES, MAP_ENDPOINT_KINDS } from '../../types/api';
import { nodeHref } from '../../lib/entityHref';
import { stateColorVar, stateLabel } from '../../lib/format';
import { SEVERITY_ORDER } from '../../lib/nodeState';
import { Button } from '../ui/Button';
import { StatusDot } from '../ui/StatusDot';
import { WifiIcon } from '../ui/icons';
import { useEntityNames } from '../ui/entityNames';
import {
  edgesOf,
  hasTiers,
  levelNodesHref,
  nodePlace,
  unresolvedCount,
  type MapSelection,
} from './topologyLevel';
import { MapEdgeMembers } from './MapEdgeMembers';
import { MapBundleMembers } from './MapBundleMembers';
import type { TextCondition } from '../../lib/filterCondition';
import { ROW_ORDER, type PlacedNode } from './graphLayout';
import './TopologyMapSidePanel.css';

interface Props {
  level: MapLevel;
  selection: MapSelection;
  /** The selected bundle's box, with its members' live states, when a bundle is selected. */
  bundle?: PlacedNode | null;
  /** The map's search while one is active (ADR-191 Inc.11): a bundle's matching access points are
   *  listed first and their names marked. */
  cond?: TextCondition | null;
  /** The title of the level ("Whole network" or the folder's name), for the node's place. */
  levelName: string;
  onSelectEdge: (id: string) => void;
  onClear: () => void;
}

export function TopologyMapSidePanel({
  level,
  selection,
  bundle,
  cond,
  levelName,
  onSelectEdge,
  onClear,
}: Props) {
  const { t } = useTranslation('topology');
  const { nodeName } = useEntityNames();

  if (selection?.kind === 'node') {
    const node = level.nodes.find((n) => n.id === selection.id);
    if (node) {
      const lines = edgesOf(level, node.id);
      const endName = (e: MapLevel['edges'][number]) => {
        const other = e.a.kind === 'node' && e.a.id === node.id ? e.b : e.a;
        const hit =
          level.folders.find((f) => f.id === other.id)?.name ??
          level.stubs.find((s) => s.id === other.id)?.name ??
          level.nodes.find((n) => n.id === other.id)?.name;
        return hit ?? nodeName(other.id);
      };
      return (
        <aside className="topomap-panel" aria-label={node.name}>
          <h2 className="topomap-panel-title">{node.name}</h2>
          <StatusDot state={node.state} />
          <dl className="topomap-panel-facts">
            <dt>{t('map.panel.node.path')}</dt>
            <dd>{nodePlace(level, node, levelName)}</dd>
            {/* An N-1 core sends no role; show nothing rather than a raw key. */}
            {node.role && (
              <>
                <dt>{t('map.panel.node.role')}</dt>
                <dd>
                  {t(`map.role.${node.role}`)} —{' '}
                  {t(`map.roleReason.${node.role_reason}`, { count: node.subnet_count ?? 0 })}
                </dd>
              </>
            )}
          </dl>
          <h3 className="topomap-panel-sub">{t('map.panel.node.edges')}</h3>
          <ul className="topomap-panel-list">
            {lines.map((e) => (
              <li key={e.id}>
                <button type="button" className="topomap-panel-row" onClick={() => onSelectEdge(e.id)}>
                  <span className="topomap-panel-row-name">{endName(e)}</span>
                  <span className="muted">
                    {t('map.panel.edge.members', { count: e.count })} · {t(`map.source.${e.source}`)}
                  </span>
                </button>
              </li>
            ))}
          </ul>
          <div className="topomap-panel-actions">
            <Link className="topomap-panel-link" to={nodeHref(node.id)}>
              {t('map.panel.node.open')}
            </Link>
            <Button onClick={onClear}>{t('map.panel.clear')}</Button>
          </div>
        </aside>
      );
    }
  }

  if (selection?.kind === 'bundle' && bundle?.bundle) {
    const title = t('map.panel.bundle.title', { name: bundle.name });
    return (
      <aside className="topomap-panel" aria-label={title}>
        <h2 className="topomap-panel-title">{title}</h2>
        <MapBundleMembers level={level} bundle={bundle} cond={cond} />
        <div className="topomap-panel-actions">
          <Button onClick={onClear}>{t('map.panel.clear')}</Button>
        </div>
      </aside>
    );
  }

  if (selection?.kind === 'edge') {
    const edge = level.edges.find((e) => e.id === selection.id);
    if (edge) {
      return (
        <aside className="topomap-panel" aria-label={t('map.panel.edge.title')}>
          <h2 className="topomap-panel-title">{t('map.panel.edge.title')}</h2>
          <MapEdgeMembers edge={edge} />
          <div className="topomap-panel-actions">
            <Button onClick={onClear}>{t('map.panel.clear')}</Button>
          </div>
        </aside>
      );
    }
  }

  // Nothing (or a folder) selected: what this level holds, and how to read it.
  const presentStates = SEVERITY_ORDER.filter((s) => level.nodes.some((n) => n.state === s));
  const presentSources = LINK_SOURCES.filter((s) => level.edges.some((e) => e.source === s));
  const facts: [string, number][] = [
    [t('map.panel.summary.folders'), level.subfolder_count],
    [t('map.panel.summary.linked'), level.linked_node_count],
    [t('map.panel.summary.isolated'), level.isolated_count],
    [t('map.panel.summary.edges'), level.edge_count],
    [t('map.panel.summary.stubs'), level.stubs.length],
  ];
  const unresolved = unresolvedCount(level);
  return (
    <aside className="topomap-panel" aria-label={t('map.panel.summary.title')}>
      <h2 className="topomap-panel-title">{t('map.panel.summary.title')}</h2>
      <dl className="topomap-panel-facts">
        {facts.map(([label, n]) => (
          <div key={label} className="topomap-panel-fact">
            <dt>{label}</dt>
            <dd>{n}</dd>
          </div>
        ))}
      </dl>
      {hasTiers(level) && (
        <>
          <h3 className="topomap-panel-sub">{t('map.rows')}</h3>
          <ol className="topomap-panel-rows">
            {ROW_ORDER.map((r) => (
              <li key={r}>{t(`map.role.${r}`)}</li>
            ))}
          </ol>
        </>
      )}
      {unresolved > 0 && (
        <p className="topomap-panel-note muted">{t('map.unresolved', { count: unresolved })}</p>
      )}
      <Link className="topomap-panel-link" to={levelNodesHref(level)}>
        {t('map.panel.openLevelInNodes')}
      </Link>
      <h3 className="topomap-panel-sub">{t('map.legend')}</h3>
      <ul className="topomap-panel-legend">
        {MAP_ENDPOINT_KINDS.map((k) => (
          <li key={k}>
            <span className={`topomap-legend-kind ${k}`} />
            {t(`map.kind.${k}`)}
          </li>
        ))}
        {level.nodes.some((n) => n.access_point) && (
          <li>
            <span className="topomap-legend-ap">
              <WifiIcon width={10} height={10} />
            </span>
            {t('map.accessPoint')}
          </li>
        )}
        {presentStates.map((s) => (
          <li key={s}>
            <span className="topomap-legend-dot" style={{ background: stateColorVar(s) }} />
            {stateLabel(s)}
          </li>
        ))}
        {presentSources.map((s) => (
          <li key={s}>
            <span className={`topomap-legend-edge ${s}`} />
            {t(`map.source.${s}`)}
          </li>
        ))}
        {level.nodes.some((n) => n.root_cause) && (
          <li>
            <span className="topomap-legend-line" />
            {t('map.suppressed')}
          </li>
        )}
      </ul>
    </aside>
  );
}
