// SPDX-License-Identifier: AGPL-3.0-only
// The links behind one bundled line, port to port (ADR-191). Drawn by the full map's side panel and
// under the map in a folder's pane, so both list a line the same way.
//
// Every name here is device-supplied and renders as a React text child (auto-escaped).

import { useTranslation } from 'react-i18next';
import type { MapEdge } from '../../types/api';
import { EntityName } from '../ui/EntityName';
import { useEntityNames } from '../ui/entityNames';
import { memberPorts } from './topologyLevel';
import './TopologyMapSidePanel.css';

export function MapEdgeMembers({ edge }: { edge: MapEdge }) {
  const { t } = useTranslation('topology');
  const { nodeName } = useEntityNames();
  const more = edge.count - edge.members.length;
  const noPort = t('map.panel.edge.noPort');
  return (
    <>
      <p className="muted">
        {t('map.panel.edge.members', { count: edge.count })} ·{' '}
        {edge.sources.map((s) => t(`map.source.${s}`)).join(', ')}
      </p>
      <ul className="topomap-panel-list">
        {edge.members.map((m) => {
          const ports = memberPorts(m, noPort);
          return (
            <li key={m.link_id} className="topomap-panel-member">
              <div>
                <EntityName name={nodeName(m.a_node)} id={m.a_node} />{' '}
                <span className="mono">{ports.a}</span>
              </div>
              <div>
                <EntityName name={nodeName(m.b_node)} id={m.b_node} />{' '}
                <span className="mono">{ports.b}</span>
              </div>
              <div className="muted">
                {t(`map.source.${m.source}`)}
                {m.subnet && (
                  <>
                    {' · '}
                    <span className="mono">{m.subnet}</span>
                  </>
                )}
              </div>
            </li>
          );
        })}
      </ul>
      {more > 0 && <p className="muted">{t('map.panel.edge.more', { count: more })}</p>}
    </>
  );
}
