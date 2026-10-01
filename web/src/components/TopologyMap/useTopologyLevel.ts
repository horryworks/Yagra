// SPDX-License-Identifier: AGPL-3.0-only
// One level of the network map, fetched, laid out and kept live (ADR-191). Shared by the full map
// page and the map drawn inside a folder's pane on the Nodes page, so both draw the same boxes from
// the same rules.

import { useCallback, useMemo, useRef, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { useConfigChanges } from '../../lib/configChanges';
import { stateLabel } from '../../lib/format';
import { overlayLiveStates, type LiveOverlay } from '../../lib/liveOverlay';
import { api } from '../../services/api';
import { usePolled } from '../../dashboard/usePolled';
import { useNodeStates, LIVE_RECONCILE_MS } from '../../dashboard/useNodeStates';
import type { MapLevel } from '../../types/api';
import { overlayBundleStates, troubleCounts } from './apBundle';
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
  /** The selected line's id on this level, or null. */
  edge: string | null;
  /** Select a line; pressing the selected one again lets it go. */
  selectEdge: (id: string) => void;
  clearEdge: () => void;
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

  // A line's id names two ends of one level, so the selection is kept with the level it was made on
  // and simply stops applying when another level is opened.
  const [edgeSel, setEdgeSel] = useState<{ group: string | null; id: string } | null>(null);
  const edge = edgeSel && edgeSel.group === group ? edgeSel.id : null;
  const selectEdge = useCallback(
    (id: string) => setEdgeSel((cur) => (cur?.id === id && cur.group === group ? null : { group, id })),
    [group],
  );
  const clearEdge = useCallback(() => setEdgeSel(null), []);

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
    const byBox = liveByGraphId(level, live);
    overlay.current = overlayLiveStates(layout.nodes, byBox, overlay.current);
    // A bundle (ADR-191 Inc.9) is not a node of the level; its colour follows its members.
    return overlayBundleStates(overlay.current.out, byBox);
  }, [layout.nodes, level, live]);
  const viewLayout = useMemo(
    () => (placedNodes === layout.nodes ? layout : { ...layout, nodes: placedNodes }),
    [layout, placedNodes],
  );
  return { level, error: error ?? null, layout: viewLayout, edge, selectEdge, clearEdge };
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
      if (n.bundle) {
        const trouble = troubleCounts(n.bundle.members);
        const summary = trouble.length
          ? trouble.map(([s, c]) => t('map.bundle.part', { state: stateLabel(s), count: c })).join(t('map.bundle.sep'))
          : t('map.bundle.allOk');
        return t('map.bundle.title', { name: n.name, count: n.bundle.members.length, summary });
      }
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
  // A bundle's line stands for its members, and the bundle already shows how many.
  const showChip = useCallback((e: PlacedEdge) => !e.box && edgeShowsChip(e.id, e.count), []);
  return { boxTitle, edgeTitle, showChip };
}
