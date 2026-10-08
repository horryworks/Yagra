// SPDX-License-Identifier: AGPL-3.0-only
// The access points behind one bundle (ADR-191 Inc.9), port by port. Drawn by the full map's side
// panel and under the map in a folder's pane, so pressing a bundle lists the same thing on both
// (ADR-191 Inc.14).
//
// Every name here is device-supplied and renders as a React text child (auto-escaped).

import { useTranslation } from 'react-i18next';
import { Link } from 'react-router-dom';
import type { MapLevel } from '../../types/api';
import { nodeHref } from '../../lib/entityHref';
import { stateColorVar, stateLabel } from '../../lib/format';
import { compileCondition, type TextCondition } from '../../lib/filterCondition';
import { Marked } from '../ui/Marked';
import { membersByPort, stateCounts } from './apBundle';
import type { PlacedNode } from './graphLayout';
import './TopologyMapSidePanel.css';

interface Props {
  level: MapLevel;
  /** The bundle's box, with its members' live states. */
  bundle: PlacedNode;
  /** The map's search while one is active (ADR-191 Inc.11): matching access points are listed
   *  first and their names marked. */
  cond?: TextCondition | null;
}

export function MapBundleMembers({ level, bundle, cond }: Props) {
  const { t } = useTranslation('topology');
  if (!bundle.bundle) return null;
  const members = bundle.bundle.members;
  // Under a search, a port holding a hit comes first, and so does the hit inside its port. The
  // sort is stable, so everything else keeps the order `membersByPort` gave it.
  // Compiled once per render, never per comparison: the condition may be a regular expression.
  const test = cond ? compileCondition(cond) : null;
  const hits = new Set(members.filter((m) => test?.([m.name])).map((m) => m.name));
  const hit = (name: string) => hits.has(name);
  const groups = membersByPort(level, bundle.bundle)
    .map((g) => ({ ...g, members: [...g.members].sort((x, y) => Number(hit(y.name)) - Number(hit(x.name))) }))
    .sort((x, y) => Number(y.members.some((m) => hit(m.name))) - Number(x.members.some((m) => hit(m.name))));
  const highlight = cond ? { cond, semantics: 'substring' as const, widened: false } : undefined;
  // Several access points on one port: the line runs through a switch nobody monitors.
  const shared = groups.find((g) => g.port !== null && g.members.length > 1);
  return (
    <>
      <ul className="topomap-panel-chips">
        {stateCounts(members).map(([st, n]) => (
          <li key={st} className="topomap-panel-chip">
            <span className="topomap-legend-dot" style={{ background: stateColorVar(st) }} />
            {t('map.panel.bundle.stateCount', { state: stateLabel(st), count: n })}
          </li>
        ))}
      </ul>
      {shared && (
        <p className="topomap-panel-note muted">
          {t('map.panel.bundle.sharedPort', { port: shared.port, count: shared.members.length })}
        </p>
      )}
      {groups.map((g) => (
        <section key={g.port ?? ''} className="topomap-panel-port">
          <h3 className="topomap-panel-port-head">
            <span className="mono">{g.port ?? t('map.panel.edge.noPort')}</span>
            <span className="muted">{t('map.panel.bundle.portCount', { count: g.members.length })}</span>
          </h3>
          <ul className="topomap-panel-list">
            {g.members.map((m) => (
              <li key={m.id}>
                <Link className="topomap-panel-row topomap-panel-ap" to={nodeHref(m.id)} title={t('map.panel.node.open')}>
                  <span className="topomap-legend-dot" style={{ background: stateColorVar(m.state) }} />
                  <span className="topomap-panel-row-name">
                    <Marked text={m.name} highlight={hit(m.name) ? highlight : undefined} />
                  </span>
                  <span className="muted">{stateLabel(m.state)}</span>
                </Link>
              </li>
            ))}
          </ul>
        </section>
      ))}
    </>
  );
}
