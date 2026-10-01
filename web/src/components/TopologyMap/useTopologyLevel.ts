// SPDX-License-Identifier: AGPL-3.0-only
// One level of the network map, fetched, laid out and kept live (ADR-191). Shared by the full map
// page and the map drawn inside a folder's pane on the Nodes page, so both draw the same boxes from
// the same rules.

import { useCallback, useMemo, useRef } from 'react';
import { useTranslation } from 'react-i18next';
import { useConfigChanges } from '../../lib/configChanges';
import { stateLabel } from '../../lib/format';
import { overlayLiveStates, type LiveOverlay } from '../../lib/liveOverlay';
import { api } from '../../services/api';
import { usePolled } from '../../dashboard/usePolled';
import { useNodeStates, LIVE_RECONCILE_MS } from '../../dashboard/useNodeStates';
import type { MapLevel } from '../../types/api';
import { layoutGraph, type GraphLayout, type PlacedEdge, type PlacedNode } from './graphLayout';
import {
  edgeShowsChip,
  isLevelFor,
  levelToGraph,
  liveByGraphId,
  splitGraphId,
  type LevelCaptions,
} from './topologyLevel';

export const EMPTY_LAYOUT: GraphLayout = {
  nodes: [],
  edges: [],
  width: 0,
  height: 0,
  isolatedCount: 0,
  componentCount: 0,
};

export interface TopologyLevelView {
  /** The level the caller asked for, or null until its answer arrives. */
  level: MapLevel | null;
  error: string | null;
  /** The laid-out level with the live states overlaid. */
  layout: GraphLayout;
}

/** Fetch one level (`group` null = the whole network) and lay it out. */
export function useTopologyLevel(group: string | null): TopologyLevelView {
  const { t } = useTranslation('topology');
  // A node added, removed or moved between folders is on the map at once, not at the next tick.
  const configChanges = useConfigChanges();
  const { data, error } = usePolled(
    () => api.getTopologyMap(group),
    [group, configChanges],
    LIVE_RECONCILE_MS,
  );
  const level = isLevelFor(data, group) ? data : null;
  const live = useNodeStates();

  const captions: LevelCaptions = useMemo(
    () => ({
      folder: (f) => {
        const bad = f.counts.critical + f.counts.unreachable;
        return bad > 0
          ? t('map.folderSubBad', { count: f.node_count, bad })
          : t('map.folderSub', { count: f.node_count });
      },
      stub: () => t('map.externalSub'),
    }),
    [t],
  );

  // The layout depends on STRUCTURE ONLY — never on live state, which publishes a fresh Map on
  // every fleet-wide flush. The state each box is drawn with is overlaid below.
  const layout = useMemo(
    () => (level ? layoutGraph(levelToGraph(level, captions)) : EMPTY_LAYOUT),
    [level, captions],
  );
  const overlay = useRef<LiveOverlay<PlacedNode> | null>(null);
  const placedNodes = useMemo(() => {
    overlay.current = overlayLiveStates(layout.nodes, liveByGraphId(level, live), overlay.current);
    return overlay.current.out;
  }, [layout.nodes, level, live]);
  const viewLayout = useMemo(
    () => (placedNodes === layout.nodes ? layout : { ...layout, nodes: placedNodes }),
    [layout, placedNodes],
  );
  return { level, error: error ?? null, layout: viewLayout };
}

/** The tooltips and the chip rule every drawing of a level uses. */
export function useMapTitles(level: MapLevel | null) {
  const { t } = useTranslation('topology');
  const names = useMemo(() => new Map((level?.nodes ?? []).map((n) => [n.id, n.name])), [level]);
  const boxTitle = useCallback(
    (n: PlacedNode) => {
      if (n.kind === 'folder') {
        const ref = splitGraphId(n.id);
        const f = level?.folders.find((x) => x.id === ref?.id);
        return t('map.folderTitle', { name: n.name, count: f?.node_count ?? 0, state: stateLabel(n.state) });
      }
      if (n.kind === 'external') return t('map.externalTitle', { name: n.name });
      const cause = n.rootCause ? (names.get(n.rootCause) ?? null) : null;
      const title = cause
        ? t('map.nodeTitleSuppressed', { name: n.name, state: stateLabel(n.state), cause })
        : t('map.nodeTitle', { name: n.name, state: stateLabel(n.state) });
      return n.sub ? `${title} — ${n.sub}` : title;
    },
    [level, names, t],
  );
  const edgeTitle = useCallback(
    (e: PlacedEdge) => t('map.edgeTitle', { count: e.count, source: t(`map.source.${e.source}`) }),
    [t],
  );
  const showChip = useCallback((e: PlacedEdge) => edgeShowsChip(e.id, e.count), []);
  return { boxTitle, edgeTitle, showChip };
}
