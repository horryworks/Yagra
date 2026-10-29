// SPDX-License-Identifier: AGPL-3.0-only
// Topology ▸ Geo map. A pin per placed node group on a bundled world outline, coloured by the
// worst state of everything that resolves to that pin — "which of my sites is in trouble, and
// where is it". "Everything that resolves here" is geo inheritance: a folder with no coordinates
// of its own belongs to its nearest placed ancestor, so a site pin covers its racks and floors
// rather than only the nodes filed directly under it. The server does that resolution
// (`groups.rs::resolve_group_geo` → `geo_group`); this page only folds by it.
//
// The **write** side of this already existed: `PUT /api/v1/node-groups/{id}/geo`, edited from the
// group dialog. What was missing was anywhere to look at the result except a small dashboard widget.
//
// It shares its data and its colour rules with that widget (`useGroupSummary` + `listNodeGroups`,
// `worstStateFromCounts`/`countsTotal`/`stateColorVar`), so a site can never read amber here and
// green there. What it does **not** share is the projection — see the warning in `geoProjection.ts`.
//
// All the geometry lives in `geoProjection.ts` because Vitest never runs `.tsx`; what is left here
// is markup and pointer plumbing, which tsc and the build cover.

import { memo, useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { useConfigChanges } from '../lib/configChanges';
import { Trans, useTranslation } from 'react-i18next';
import { useNavigate } from 'react-router-dom';
import { Card } from '../components/ui/Card';
import { PageHeader } from '../components/ui/PageHeader';
import { ScreenLink } from '../components/ui/ScreenLink';
import { useGroupSummary } from '../dashboard/useGroupSummary';
import { usePolled } from '../dashboard/usePolled';
import { countsTotal, pinLook, pinRollupFromCounts } from '../dashboard/widgets/util';
import { TEST_IDS } from '../testIds';
import {
  alertWhatOf,
  relativeTimeMs,
  severityColorVar,
  stateColorVar,
  stateLabel,
} from '../lib/format';
import { DISPLAY_ORDER } from '../lib/nodeState';
import { api } from '../services/api';
import { sortedAlerts, useAlertStore, useMapPaneStore } from '../store';
import { usePrefsStore } from '../prefs';
import { setGeoMapDayNight, setGeoMapPlace, setGeoMapRelief } from '../serverPrefs';
import {
  clampPaneHeight,
  defaultPaneHeight,
  heightFromDrag,
  heightFromKey,
} from './mapPaneHeight';
import {
  clampGeoScale,
  clampGeoView,
  fitPins,
  geoWraps,
  type GeoPlace,
  type GeoView,
  GEO_TILE_UNITS,
  MAP_HEIGHT,
  MAP_WIDTH,
  MAX_GEO_SCALE,
  placedOnly,
  placeFromView,
  project,
  viewFromPlace,
  visibleGeoTiles,
  wrapPinX,
  zoomGeoView,
} from './geoProjection';
import { type GeoTile, placeGeoTiles, useGeoTiles, wantedTiles } from './geoTiles';
import {
  SKIES,
  type Sky,
  TWILIGHT_LEVELS,
  skyAt,
  sunAltitude,
  sunPosition,
  twilightPaths,
} from './geoDayNight';
import { WORLD_LAKES, WORLD_OUTLINE } from './worldOutline';
import { RELIEF_LEGEND, reliefLayers, useGeoRelief, type ReliefLayer } from './geoRelief';
import { nodeHref, nodesPageHref, topologyMapHref } from '../lib/entityHref';
import { AnchoredPopover } from '../components/ui/AnchoredPopover';
import { AlertWhatText } from '../widgets/AlertWhatText';
import type { NodeGroup } from '../types/api';
import type { StateCounts } from '../dashboard/widgets/util';
import {
  alertNodeIds,
  nodeNameBatches,
  PIN_ALERT_ROWS,
  pinAlertRows,
  placesFromNames,
  stateBreakdown,
  type NodePlace,
} from './geoPinAlerts';
import { clusterPins, splitsAt, type PinCluster } from './geoCluster';
import './GeoMapPage.css';

/** The legend's day/night swatches: how many of the four shades lie over a place with that sky. */
const SKY_DEPTH: Record<Sky, number> = {
  day: 0,
  twilight: 2,
  night: TWILIGHT_LEVELS.length,
};
const SKY_LEGEND = SKIES.map((sky) => ({ sky, depth: SKY_DEPTH[sky] }));

/** How long the map must rest before where it is looking is written down (ADR-218). A drag or a
 *  burst of wheel ticks is one move, not one save per frame. */
const PLACE_SAVE_QUIET_MS = 500;

/** The id the second copy of the world points at (ADR-218). One Geo map is mounted at a time. */
const WORLD_ID = 'geopage-world';

/** A hair beyond a tile's square, as its filled shapes are, so two squares leave no seam. */
const TILE_PAD = 0.01;

/**
 * The fine tiles, drawn over the world outline (ADR-207): each tile's square in the ocean colour
 * first, so the coarser land cannot show past the finer coast, then every tile's land, its inland
 * water, and last the shoreline — the only stroked list, since the filled shapes' edges include
 * the cut along the square. Memoized, so a drag re-renders none of its paths.
 */
const GeoDetailLayer = memo(function GeoDetailLayer({ tiles }: { tiles: [string, GeoTile][] }) {
  if (tiles.length === 0) return null;
  const pad = TILE_PAD;
  const placed = placeGeoTiles(tiles);
  return (
    <g>
      {placed.map(({ key, o }) => (
        <rect
          className="geopage-ocean"
          key={key}
          x={o.x - pad}
          y={o.y - pad}
          width={GEO_TILE_UNITS + 2 * pad}
          height={GEO_TILE_UNITS + 2 * pad}
        />
      ))}
      {placed.map(({ key, tile, at }) => (
        <g transform={at} key={key}>
          {tile.land.map((d, i) => (
            <path className="geopage-detail-land" d={d} key={i} />
          ))}
        </g>
      ))}
      {placed.map(({ key, tile, at }) => (
        <g transform={at} key={key}>
          {tile.water.map((d, i) => (
            <path className="geopage-detail-water" d={d} key={i} />
          ))}
        </g>
      ))}
      {placed.map(({ key, tile, at }) => (
        <g transform={at} key={key}>
          {tile.coast.map((d, i) => (
            <path className="geopage-coast" d={d} key={i} />
          ))}
        </g>
      ))}
    </g>
  );
});

/** The id the relief's sea bands are drawn under, so the fine tiles can show them again. */
const RELIEF_SEA_ID = 'geopage-relief-sea';
/** The clip that limits that second showing to the squares whose fine tiles have loaded. */
const RELIEF_TILES_CLIP_ID = 'geopage-relief-tiles';

/**
 * The world in relief colours (ADR-220), in place of the ocean, outline, lakes and detail layer.
 * The order is the point: the sea bands under the land; under the fine tiles' land, the sea bands
 * again, clipped to the loaded squares, so the coarse outline's overhang into the sea takes the
 * right depth colour instead of a flat square; the land bands over every land; and the water
 * last, so a lake high in the mountains is not painted over by the band it sits in.
 */
const GeoReliefLayer = memo(function GeoReliefLayer({
  sea,
  land,
  tiles,
}: {
  sea: ReliefLayer[];
  land: ReliefLayer[];
  tiles: [string, GeoTile][];
}) {
  const pad = TILE_PAD;
  const placed = placeGeoTiles(tiles);
  return (
    <g>
      <rect className="geopage-relief-sea-0" x={0} y={0} width={MAP_WIDTH} height={MAP_HEIGHT} />
      <g id={RELIEF_SEA_ID}>
        {sea.map((l) => (
          <path className={l.className} d={l.d} key={l.className} />
        ))}
      </g>
      {WORLD_OUTLINE.map((d, i) => (
        <path className="geopage-land geopage-relief-lowland" d={d} key={i} />
      ))}
      {placed.length > 0 && (
        <>
          <clipPath id={RELIEF_TILES_CLIP_ID}>
            {placed.map(({ key, o }) => (
              <rect
                key={key}
                x={o.x - pad}
                y={o.y - pad}
                width={GEO_TILE_UNITS + 2 * pad}
                height={GEO_TILE_UNITS + 2 * pad}
              />
            ))}
          </clipPath>
          <g clipPath={`url(#${RELIEF_TILES_CLIP_ID})`}>
            <rect
              className="geopage-relief-sea-0"
              x={0}
              y={0}
              width={MAP_WIDTH}
              height={MAP_HEIGHT}
            />
            <use href={`#${RELIEF_SEA_ID}`} />
          </g>
          {placed.map(({ key, tile, at }) => (
            <g transform={at} key={key}>
              {tile.land.map((d, i) => (
                <path className="geopage-detail-land geopage-relief-lowland" d={d} key={i} />
              ))}
            </g>
          ))}
        </>
      )}
      {land.map((l) => (
        <path className={l.className} d={l.d} key={l.className} />
      ))}
      {WORLD_LAKES.map((d, i) => (
        <path className="geopage-lake geopage-relief-sea-4" d={d} key={i} />
      ))}
      {placed.map(({ key, tile, at }) => (
        <g transform={at} key={key}>
          {tile.water.map((d, i) => (
            <path className="geopage-detail-water geopage-relief-sea-4" d={d} key={i} />
          ))}
        </g>
      ))}
      {placed.map(({ key, tile, at }) => (
        <g transform={at} key={key}>
          {tile.coast.map((d, i) => (
            <path className="geopage-coast" d={d} key={i} />
          ))}
        </g>
      ))}
    </g>
  );
});

export function GeoMapPage() {
  const { t } = useTranslation('topology');
  const navigate = useNavigate();
  const wrapRef = useRef<HTMLDivElement | null>(null);
  // Where the map is looking. Remembered on the account in degrees (ADR-218), not here in pixels:
  // `geoMapPlace` is read once the pane is measured, and written back once the operator has moved
  // the map and let it rest.
  const [view, setView] = useState<GeoView | null>(null);
  const storedPlace = usePrefsStore((s) => s.geoMapPlace);
  /** Whether the operator has moved the map since this page opened. Until then the account's place
   *  may still replace the view when it arrives, and nothing is saved: opening the page and
   *  fitting the pins is not a choice anyone made (ADR-218 decision 6). */
  const touched = useRef(false);
  /** Every move the operator makes: the view changes and is marked as theirs. */
  const moveView = useCallback(
    (next: GeoView | null | ((prev: GeoView | null) => GeoView | null)) => {
      touched.current = true;
      setView(next);
    },
    [],
  );
  /** The pane's width, for whether the world repeats at this zoom (`geoWraps`). */
  const [paneW, setPaneW] = useState(0);
  const drag = useRef<{ x: number; y: number; tx: number; ty: number } | null>(null);
  // Live pointers by id: one pans, two pinch-zoom. Mirrors `TopologyMap`'s gesture handling so the
  // two maps in this product feel the same under a finger.
  const pointers = useRef<Map<number, { x: number; y: number }>>(new Map());
  const pinch = useRef<{
    dist: number;
    cx: number;
    cy: number;
    scale: number;
    tx: number;
    ty: number;
  } | null>(null);

  const { summary, loading, error } = useGroupSummary();
  // Re-read at once when someone changes a folder (its location lives on it), not only on the tick.
  const configChanges = useConfigChanges();
  const groups = usePolled(() => api.listNodeGroups(), [configChanges]);

  // Pane height: the operator's stored preference, or one derived from this window. Re-clamped on
  // read so a height saved on a big monitor cannot swallow a laptop screen.
  const storedHeight = useMapPaneStore((s) => s.geoHeight);
  const setStoredHeight = useMapPaneStore((s) => s.setGeoHeight);
  const viewportH = typeof window === 'undefined' ? 0 : window.innerHeight;
  const paneHeight =
    storedHeight == null ? defaultPaneHeight(viewportH) : clampPaneHeight(storedHeight, viewportH);
  const resize = useRef<{ y: number; h: number } | null>(null);

  const placed = useMemo(() => placedOnly(groups.data ?? []), [groups.data]);
  const placedById = useMemo(() => new Map(placed.map((g) => [g.id, g])), [placed]);

  // Day/night shading (ADR-189). On unless this account switched it off; the clock it is drawn for
  // moves once a minute, and only while the shading is showing.
  const dayNight = usePrefsStore((s) => s.geoMapDayNight) !== false;
  // Elevation colouring (ADR-220). Off unless this account switched it on; the bands are fetched
  // the first time it is, and the map stays grey until they arrive.
  const reliefOn = usePrefsStore((s) => s.geoMapRelief) === true;
  const relief = useGeoRelief(reliefOn);
  const reliefBands = useMemo(() => (relief ? reliefLayers(relief) : null), [relief]);
  const [skyTime, setSkyTime] = useState(() => new Date());
  /** The pin whose bubble is open, and where (ADR-191). */
  const [pinMenu, setPinMenu] = useState<{ id: string; at: { x: number; y: number } } | null>(null);
  const closePinMenu = useCallback(() => setPinMenu(null), []);
  /** The overlapping sites whose list is open (ADR-206): a snapshot of the members, so zooming
   *  while it is open cannot change what the list names. */
  const [clusterMenu, setClusterMenu] = useState<{
    cluster: PinCluster;
    at: { x: number; y: number };
  } | null>(null);
  const closeClusterMenu = useCallback(() => setClusterMenu(null), []);
  /** Counts the presses that opened a bubble, so each one replays the ripple on its marker
   *  (ADR-206 Inc.2) — pressing the same pin twice is a new key, not the same element. */
  const [pulse, setPulse] = useState(0);
  const openPin = useCallback((id: string, at: { x: number; y: number }) => {
    setPinMenu({ id, at });
    setPulse((n) => n + 1);
  }, []);
  const openCluster = useCallback((cluster: PinCluster, at: { x: number; y: number }) => {
    setClusterMenu({ cluster, at });
    setPulse((n) => n + 1);
  }, []);
  useEffect(() => {
    if (!dayNight) return;
    setSkyTime(new Date());
    const id = window.setInterval(() => setSkyTime(new Date()), 60_000);
    return () => window.clearInterval(id);
  }, [dayNight]);
  const nightPaths = useMemo(() => (dayNight ? twilightPaths(skyTime) : []), [dayNight, skyTime]);
  const sun = useMemo(() => sunPosition(skyTime), [skyTime]);

  const fit = useCallback(() => {
    const el = wrapRef.current;
    if (!el) return;
    moveView(fitPins(placed, el.clientWidth, el.clientHeight));
  }, [placed, moveView]);

  // Place the map once, on the first render that has a measured pane: where the operator left it
  // (ADR-218), or around the pins. The `view === null` guard is what stops the 15s poll refresh
  // from stomping the operator's pan/zoom every tick — the same guard, and the same reason, as
  // `TopologyMap`.
  // 🚨 `paneW` is what makes this run when the pane first appears. The pane is drawn only once
  // both the folders and their tallies have arrived; when the folders came first, this ran with
  // no pane, and nothing else it watched changed when the tallies followed — the map sat unplaced
  // until the 15s refresh re-read the folders (seen on a lab box in a fresh browser).
  useEffect(() => {
    const el = wrapRef.current;
    if (view === null && el && paneW > 0) {
      const w = el.clientWidth;
      const h = el.clientHeight;
      setView(storedPlace ? viewFromPlace(storedPlace, w, h) : fitPins(placed, w, h));
    }
  }, [view, placed, storedPlace, paneW]);

  // The account's place can arrive after the page has opened (a sign-in moments ago). Take it
  // while the operator has not moved the map yet; once they have, their view wins. The page's own
  // saves come back through here too, and are skipped by the same flag. `paneW` for the same
  // reason as above: the place may arrive while the pane is not drawn yet.
  useEffect(() => {
    const el = wrapRef.current;
    if (!storedPlace || touched.current || !el || paneW <= 0) return;
    setView(viewFromPlace(storedPlace, el.clientWidth, el.clientHeight));
  }, [storedPlace, paneW]);

  // Write down where the map is looking once it has rested (ADR-218), and on the way out, so a
  // move made just before leaving the page is not lost.
  const unsaved = useRef<GeoPlace | null>(null);
  useEffect(() => {
    const el = wrapRef.current;
    if (!touched.current || !view || !el) return;
    const place = placeFromView(view, el.clientWidth, el.clientHeight);
    if (!place) return;
    unsaved.current = place;
    const id = window.setTimeout(() => {
      unsaved.current = null;
      setGeoMapPlace(place);
    }, PLACE_SAVE_QUIET_MS);
    return () => window.clearTimeout(id);
  }, [view]);
  useEffect(
    () => () => {
      if (unsaved.current) setGeoMapPlace(unsaved.current);
    },
    [],
  );

  // Refit when the operator resizes the pane. This deliberately *does* stomp the current pan/zoom,
  // unlike the poll refresh above: dragging the pane is a direct request for more (or less) map, and
  // leaving the old transform would just add empty space at the new size. The inline height is
  // applied in the same render, so `clientHeight` is already the new one by the time this runs.
  // Not on the first render: that height is the stored one, and the view is the placement above.
  const fittedHeight = useRef(paneHeight);
  useEffect(() => {
    if (fittedHeight.current === paneHeight) return;
    fittedHeight.current = paneHeight;
    const el = wrapRef.current;
    if (el && el.clientWidth > 0) moveView(fitPins(placed, el.clientWidth, el.clientHeight));
    // `placed` is intentionally not a dependency — a poll that returns the same sites must not
    // refit. Only a height change should.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [paneHeight]);

  // The pane's width follows the window, so hold the view inside the pane whenever the pane is
  // measured — first when it mounts, then on every resize (ADR-188). The pane is not drawn
  // while the page is loading or failed, so this has to wait for it rather than run once on mount.
  const busy = (loading && !summary) || (groups.loading && !groups.data);
  const mapShown = !busy && !error && !groups.error;
  useEffect(() => {
    const el = wrapRef.current;
    if (!mapShown || !el || typeof ResizeObserver === 'undefined') return;
    const hold = () => {
      const w = el.clientWidth;
      const h = el.clientHeight;
      setPaneW(w);
      if (w > 0 && h > 0) setView((v) => (v ? clampGeoView(v, w, h) : v));
    };
    const ro = new ResizeObserver(hold);
    ro.observe(el);
    return () => ro.disconnect();
  }, [mapShown]);

  /** Zoom around the pane's centre — the buttons have no cursor to anchor on. */
  const zoomCentre = useCallback(
    (factor: number) => {
      const el = wrapRef.current;
      if (!el) return;
      const w = el.clientWidth;
      const h = el.clientHeight;
      moveView((v) => (v ? zoomGeoView(v, factor, w / 2, h / 2, w, h) : v));
    },
    [moveView],
  );

  const onWheel = useCallback((e: React.WheelEvent) => {
    const el = wrapRef.current;
    if (!el) return;
    const rect = el.getBoundingClientRect();
    const mx = e.clientX - rect.left;
    const my = e.clientY - rect.top;
    const w = el.clientWidth;
    const h = el.clientHeight;
    // Keep the point under the cursor fixed while zooming.
    moveView((v) => (v ? zoomGeoView(v, e.deltaY < 0 ? 1.15 : 1 / 1.15, mx, my, w, h) : v));
  }, [moveView]);

  const onPointerDown = useCallback(
    (e: React.PointerEvent) => {
      if (!view) return;
      (e.target as Element).setPointerCapture?.(e.pointerId);
      pointers.current.set(e.pointerId, { x: e.clientX, y: e.clientY });
      const el = wrapRef.current;
      if (pointers.current.size >= 2 && el) {
        const [p1, p2] = [...pointers.current.values()];
        const rect = el.getBoundingClientRect();
        pinch.current = {
          dist: Math.hypot(p2.x - p1.x, p2.y - p1.y) || 1,
          cx: (p1.x + p2.x) / 2 - rect.left,
          cy: (p1.y + p2.y) / 2 - rect.top,
          scale: view.scale,
          tx: view.tx,
          ty: view.ty,
        };
        drag.current = null;
      } else {
        drag.current = { x: e.clientX, y: e.clientY, tx: view.tx, ty: view.ty };
      }
    },
    [view],
  );

  const onPointerMove = useCallback((e: React.PointerEvent) => {
    if (!pointers.current.has(e.pointerId)) return;
    pointers.current.set(e.pointerId, { x: e.clientX, y: e.clientY });
    const p = pinch.current;
    const el = wrapRef.current;
    if (p && pointers.current.size >= 2 && el) {
      const [p1, p2] = [...pointers.current.values()];
      const dist = Math.hypot(p2.x - p1.x, p2.y - p1.y) || 1;
      const rect = el.getBoundingClientRect();
      const mx = (p1.x + p2.x) / 2 - rect.left;
      const my = (p1.y + p2.y) / 2 - rect.top;
      const w = el.clientWidth;
      const h = el.clientHeight;
      moveView((v) => {
        if (!v) return v;
        const scale = clampGeoScale(p.scale * (dist / p.dist), w, h);
        const k = scale / p.scale;
        return clampGeoView(
          {
            scale,
            tx: p.cx - (p.cx - p.tx) * k + (mx - p.cx),
            ty: p.cy - (p.cy - p.ty) * k + (my - p.cy),
          },
          w,
          h,
        );
      });
      return;
    }
    const d = drag.current;
    if (!d || !el) return;
    const w = el.clientWidth;
    const h = el.clientHeight;
    moveView((v) =>
      v
        ? clampGeoView({ ...v, tx: d.tx + (e.clientX - d.x), ty: d.ty + (e.clientY - d.y) }, w, h)
        : v,
    );
  }, [moveView]);

  const onPointerUp = useCallback((e: React.PointerEvent) => {
    pointers.current.delete(e.pointerId);
    (e.target as Element).releasePointerCapture?.(e.pointerId);
    if (pointers.current.size < 2) pinch.current = null;
    if (pointers.current.size === 1) {
      const [only] = [...pointers.current.values()];
      setView((v) => {
        drag.current = v ? { x: only.x, y: only.y, tx: v.tx, ty: v.ty } : null;
        return v;
      });
    } else if (pointers.current.size === 0) {
      drag.current = null;
    }
  }, []);

  // ── Pane resize (the handle along the bottom edge) ─────────────────────────
  const onResizeDown = useCallback(
    (e: React.PointerEvent) => {
      (e.target as Element).setPointerCapture?.(e.pointerId);
      resize.current = { y: e.clientY, h: paneHeight };
    },
    [paneHeight],
  );
  const onResizeMove = useCallback(
    (e: React.PointerEvent) => {
      const r = resize.current;
      if (!r) return;
      setStoredHeight(heightFromDrag(r.h, r.y, e.clientY, window.innerHeight));
    },
    [setStoredHeight],
  );
  const onResizeUp = useCallback((e: React.PointerEvent) => {
    (e.target as Element).releasePointerCapture?.(e.pointerId);
    resize.current = null;
  }, []);
  const onResizeKey = useCallback(
    (e: React.KeyboardEvent) => {
      // Keyboard-operable, like every other primary control (ui-conventions.md).
      const dir = e.key === 'ArrowDown' ? 1 : e.key === 'ArrowUp' ? -1 : 0;
      if (dir === 0) return;
      e.preventDefault();
      setStoredHeight(heightFromKey(paneHeight, dir, window.innerHeight));
    },
    [paneHeight, setStoredHeight],
  );

  const v = view ?? { tx: 0, ty: 0, scale: 1 };
  // Per-pin totals, not per-folder: a group with no coordinates of its own is counted at the
  // nearest placed ancestor's pin (geo inheritance), which the server resolved into `geo_group`.
  const pins = useMemo(
    () => pinRollupFromCounts(summary?.groups ?? {}, groups.data ?? []),
    [summary, groups.data],
  );
  // The legend names the hollow ring only while one is on the map.
  const hasEmptyPin = placed.some((g) => pinLook(pins[g.id]) === 'empty');
  // Pins that overlap on screen are drawn as one marker (ADR-206). Grouped by scale only, so a
  // drag never moves a site from one marker to another.
  const clusters = useMemo(
    () =>
      clusterPins(
        placed.map((g) => ({
          id: g.id,
          name: g.name,
          ...project(g.latitude, g.longitude),
          counts: pins[g.id],
        })),
        v.scale,
      ),
    [placed, pins, v.scale],
  );

  // The fine coastline tiles under the pane (ADR-207), none below the detail zoom. Kept as state
  // so a drag that stays inside the same tiles changes nothing the tile layer reads.
  const [tileKeys, setTileKeys] = useState<string[]>([]);
  useEffect(() => {
    const el = wrapRef.current;
    const next =
      el && view ? wantedTiles(visibleGeoTiles(view, el.clientWidth, el.clientHeight)) : [];
    setTileKeys((prev) => (prev.join(',') === next.join(',') ? prev : next));
  }, [view]);
  const detailTiles = useGeoTiles(tileKeys);

  /** Zoom in around a marker, from its list. */
  const zoomAt = useCallback(
    (c: PinCluster) => {
      const el = wrapRef.current;
      if (!el) return;
      const w = el.clientWidth;
      const h = el.clientHeight;
      moveView((cur) =>
        cur
          ? zoomGeoView(
              cur,
              3,
              wrapPinX(c.x, cur) * cur.scale + cur.tx,
              c.y * cur.scale + cur.ty,
              w,
              h,
            )
          : cur,
      );
    },
    [moveView],
  );

  return (
    <div className="geopage">
      <PageHeader title={t('geo.title')} />
      {(error || groups.error) && (
        <Card>
          <p className="muted">{t('geo.error')}</p>
        </Card>
      )}
      {busy && <p className="muted">{t('common:loading')}</p>}
      {mapShown && (
        <>
          {/* The empty state is a caption over a live map, not instead of one: an operator with no
              coordinates set needs to see what the page is for and be told where to set them. */}
          {placed.length === 0 && (
            <p className="geopage-empty muted">
              <Trans t={t} i18nKey="geo.empty" components={{ lnk: <ScreenLink to="/nodes" /> }} />
            </p>
          )}
          {/* No wider than the world is at this height (ADR-188 Inc.2), so a wide monitor does
              not get a band of grey either side of the map. The aspect comes from the projection,
              not from the stylesheet — see `.geopage-frame`. */}
          <div
            className="geopage-frame"
            style={
              {
                '--geo-pane-h': `${paneHeight}px`,
                '--geo-aspect': MAP_WIDTH / MAP_HEIGHT,
              } as React.CSSProperties
            }
          >
            <div className="geopage-map" ref={wrapRef} style={{ height: `${paneHeight}px` }}>
              <div className="geopage-controls">
                <button
                  className="geopage-ctl geopage-ctl-daynight"
                  onClick={() => setGeoMapDayNight(!dayNight)}
                  aria-pressed={dayNight}
                  title={t('geo.dayNight.toggle')}
                  aria-label={t('geo.dayNight.toggle')}
                >
                  <svg viewBox="0 0 16 16" aria-hidden="true">
                    <circle cx="8" cy="8" r="6" fill="none" stroke="currentColor" strokeWidth="1.3" />
                    <path d="M8 2a6 6 0 0 1 0 12z" fill="currentColor" />
                  </svg>
                  {t('geo.dayNight.label')}
                </button>
                <button
                  className="geopage-ctl geopage-ctl-relief"
                  onClick={() => setGeoMapRelief(!reliefOn)}
                  aria-pressed={reliefOn}
                  title={t('geo.relief.toggle')}
                  aria-label={t('geo.relief.toggle')}
                >
                  <svg viewBox="0 0 16 16" aria-hidden="true">
                    <path d="M1 13.5 6 5l3 5 2-3 4 6.5z" fill="currentColor" />
                  </svg>
                  {t('geo.relief.label')}
                </button>
                <button
                  className="geopage-ctl"
                  onClick={fit}
                  title={t('map.control.fitToView')}
                  aria-label={t('map.control.fitToView')}
                >
                  {t('map.control.fit')}
                </button>
                <button
                  className="geopage-ctl"
                  onClick={() => zoomCentre(1.3)}
                  title={t('map.control.zoomIn')}
                  aria-label={t('map.control.zoomIn')}
                >
                  +
                </button>
                <button
                  className="geopage-ctl"
                  onClick={() => zoomCentre(1 / 1.3)}
                  title={t('map.control.zoomOut')}
                  aria-label={t('map.control.zoomOut')}
                >
                  −
                </button>
              </div>
              <svg
                className="geopage-svg"
                width="100%"
                height="100%"
                role="img"
                aria-label={t('geo.aria', { count: placed.length })}
                onWheel={onWheel}
                onPointerDown={onPointerDown}
                onPointerMove={onPointerMove}
                onPointerUp={onPointerUp}
                onPointerCancel={onPointerUp}
                onPointerLeave={onPointerUp}
              >
                <g transform={`translate(${v.tx} ${v.ty}) scale(${v.scale})`}>
                  {/* One world, drawn once and shown a second time one width to the right by the
                      `<use>` below, so a drag across the date line keeps going (ADR-218). A pane
                      is never wider than one world, so two copies cover every view. */}
                  <g id={WORLD_ID}>
                    {reliefBands ? (
                      <GeoReliefLayer
                        sea={reliefBands.sea}
                        land={reliefBands.land}
                        tiles={detailTiles}
                      />
                    ) : (
                      <>
                        {/* Ocean, so the outline reads as land rather than as floating shapes. */}
                        <rect
                          className="geopage-ocean"
                          x={0}
                          y={0}
                          width={MAP_WIDTH}
                          height={MAP_HEIGHT}
                        />
                        {WORLD_OUTLINE.map((d, i) => (
                          <path className="geopage-land" d={d} key={i} />
                        ))}
                        {/* Inland water, in the ocean colour, over the land it sits in. */}
                        {WORLD_LAKES.map((d, i) => (
                          <path className="geopage-lake" d={d} key={i} />
                        ))}
                        {/* The fine coastline over the world outline where it has loaded (ADR-207). */}
                        <GeoDetailLayer tiles={detailTiles} />
                      </>
                    )}
                    {/* The night side, four stacked shades from sunset to the end of astronomical
                        twilight (ADR-189). Over the land, under the pins: a site's colour is its
                        health and must read the same at midnight as at noon. */}
                    {nightPaths.map((d, i) => (
                      <path className="geopage-night" d={d} key={i} />
                    ))}
                  </g>
                  {geoWraps(v.scale, paneW) && <use href={`#${WORLD_ID}`} x={MAP_WIDTH} />}
                  {clusters.map((c) => {
                    // Counter-scaled so a pin stays the same size on screen at any zoom — a pin that
                    // grows with the map turns into a blob that hides the site it marks.
                    const r = 5 / v.scale;
                    if (c.members.length > 1) {
                      const look = pinLook(c.counts);
                      const label = t('geo.cluster.title', {
                        count: c.members.length,
                      });
                      return (
                        <g
                          className="geopage-pin geopage-cluster"
                          data-testid={TEST_IDS.geoMapCluster}
                          key={`cluster:${c.key}`}
                          transform={`translate(${wrapPinX(c.x, v)} ${c.y})`}
                          role="button"
                          tabIndex={0}
                          aria-haspopup="dialog"
                          onClick={(e) => openCluster(c, { x: e.clientX, y: e.clientY })}
                          onKeyDown={(e) => {
                            if (e.key === 'Enter' || e.key === ' ') {
                              e.preventDefault();
                              const b = e.currentTarget.getBoundingClientRect();
                              openCluster(c, {
                                x: b.left + b.width / 2,
                                y: b.top + b.height / 2,
                              });
                            }
                          }}
                        >
                          <title>
                            {look === 'empty' ? label : `${label} — ${stateLabel(look)}`}
                          </title>
                          {clusterMenu?.cluster.key === c.key && (
                            <PressMark r={r * 3} pulse={pulse} />
                          )}
                          <circle className="geopage-pin-halo" r={r * 2.6} />
                          {look === 'empty' ? (
                            <circle className="geopage-pin-dot geopage-pin-empty" r={r * 1.7} />
                          ) : (
                            <circle
                              className="geopage-pin-dot"
                              r={r * 1.7}
                              style={{ fill: stateColorVar(look) }}
                            />
                          )}
                          <text
                            className="geopage-cluster-count"
                            textAnchor="middle"
                            dominantBaseline="central"
                            fontSize={9 / v.scale}
                          >
                            {c.members.length}
                          </text>
                        </g>
                      );
                    }
                    const g = c.members[0];
                    const look = pinLook(g.counts);
                    const total = g.counts ? countsTotal(g.counts) : 0;
                    const lat = placedById.get(g.id)?.latitude ?? 0;
                    const lon = placedById.get(g.id)?.longitude ?? 0;
                    const sky = dayNight
                      ? t(`geo.dayNight.sky.${skyAt(sunAltitude(sun, lat, lon))}`)
                      : null;
                    return (
                      <g
                        className="geopage-pin"
                        data-testid={TEST_IDS.geoMapPin}
                        key={g.id}
                        transform={`translate(${wrapPinX(g.x, v)} ${g.y})`}
                        role="button"
                        tabIndex={0}
                        // ⚠️ The `group:` prefix is not decoration. `parseSelection` splits on the
                        // first colon and returns null without one, so a bare UUID landed on All
                        // nodes with nothing selected at all — the pin navigated, and then quietly
                        // did nothing. (`PollersPage` has always written the `node:` form.)
                        // A pin opens its bubble (ADR-191 Inc.15): the site, its alerting
                        // nodes, and where to go next. It opens at the pointer, or at the pin's
                        // centre for the keyboard.
                        aria-haspopup="dialog"
                        onClick={(e) => openPin(g.id, { x: e.clientX, y: e.clientY })}
                        onKeyDown={(e) => {
                          if (e.key === 'Enter' || e.key === ' ') {
                            e.preventDefault();
                            const r = e.currentTarget.getBoundingClientRect();
                            openPin(g.id, { x: r.left + r.width / 2, y: r.top + r.height / 2 });
                          }
                        }}
                      >
                        <title>
                          {look === 'empty'
                            ? t('geo.pinTitleEmpty', { name: g.name })
                            : t('geo.pinTitle', {
                                name: g.name,
                                state: stateLabel(look),
                                count: total,
                              })}
                          {sky && ` · ${sky}`}
                        </title>
                        {/* Halo first so the dot reads against both land and ocean. */}
                        {pinMenu?.id === g.id && <PressMark r={r * 2.3} pulse={pulse} />}
                        <circle className="geopage-pin-halo" r={r * 1.9} />
                        {/* A site with no nodes is a hollow ring: nothing there is monitored, so
                            it gets no state colour — not green, and not the grey of `unknown`. */}
                        {look === 'empty' ? (
                          <circle className="geopage-pin-dot geopage-pin-empty" r={r} />
                        ) : (
                          <circle
                            className="geopage-pin-dot"
                            r={r}
                            style={{ fill: stateColorVar(look) }}
                          />
                        )}
                      </g>
                    );
                  })}
                </g>
              </svg>
            </div>
            {/* Drag the bottom edge to make the map taller or shorter; the size is remembered.
                A slider role rather than a bare div so it is announced, focusable and arrow-key
                operable — the map is the content of this page, so how much of the screen it gets is
                a real control, not decoration. */}
            <div
              className="geopage-resize"
              role="slider"
              tabIndex={0}
              aria-label={t('geo.resize')}
              aria-orientation="vertical"
              aria-valuenow={paneHeight}
              onPointerDown={onResizeDown}
              onPointerMove={onResizeMove}
              onPointerUp={onResizeUp}
              onPointerCancel={onResizeUp}
              onKeyDown={onResizeKey}
              onDoubleClick={() => setStoredHeight(defaultPaneHeight(window.innerHeight))}
              title={t('geo.resize')}
            >
              <span className="geopage-resize-grip" aria-hidden="true" />
            </div>
            {/* Never colour alone (ui-conventions.md): the legend names each state in text. Derived
                from `DISPLAY_ORDER` rather than a local list, because a pin's colour comes from
                `worstStateFromCounts`, which walks the full `NodeState` union — a hand-written legend
                drops whichever state nobody remembered (it omitted `maintenance`, so a site whose
                only non-ok nodes were in a maintenance window drew a colour the legend never named). */}
            <ul className="geopage-legend">
              {DISPLAY_ORDER.map((s) => (
                <li key={s}>
                  <span className="geopage-swatch" style={{ background: stateColorVar(s) }} />
                  {stateLabel(s)}
                </li>
              ))}
              {hasEmptyPin && (
                <li>
                  <span className="geopage-swatch geopage-swatch-empty" />
                  {t('geo.noNodes')}
                </li>
              )}
              {reliefBands && (
                <>
                  <li className="geopage-legend-sep" aria-hidden="true" />
                  {RELIEF_LEGEND.map(({ className, level }) => (
                    <li key={className}>
                      <svg className="geopage-relief-swatch" aria-hidden="true">
                        <rect className={className} width="100%" height="100%" />
                      </svg>
                      {t('geo.relief.level', { level })}
                    </li>
                  ))}
                </>
              )}
              {dayNight && (
                <>
                  <li className="geopage-legend-sep" aria-hidden="true" />
                  {/* Each swatch is the map's ground with the night shade stacked as deep as the map
                      stacks it there, so the legend cannot drift from what is drawn. */}
                  {SKY_LEGEND.map(({ sky, depth }) => (
                    <li key={sky}>
                      <span className="geopage-sky-swatch">
                        {Array.from({ length: depth }, (_, k) => (
                          <span className="geopage-sky-shade" key={k} />
                        ))}
                      </span>
                      {t(`geo.dayNight.sky.${sky}`)}
                    </li>
                  ))}
                  <li className="geopage-legend-time">
                    {t('geo.dayNight.asOf', {
                      time: skyTime.toISOString().slice(0, 16).replace('T', ' '),
                    })}
                  </li>
                </>
              )}
            </ul>
          </div>
        </>
      )}
      {clusterMenu && (
        <ClusterBubble
          cluster={clusterMenu.cluster}
          at={clusterMenu.at}
          canZoom={
            v.scale < MAX_GEO_SCALE && splitsAt(clusterMenu.cluster.members, MAX_GEO_SCALE)
          }
          onClose={closeClusterMenu}
          onPick={(id) => {
            const at = clusterMenu.at;
            closeClusterMenu();
            openPin(id, at);
          }}
          onZoom={() => {
            const c = clusterMenu.cluster;
            closeClusterMenu();
            zoomAt(c);
          }}
        />
      )}
      {pinMenu && (
        <PinBubble
          pinId={pinMenu.id}
          at={pinMenu.at}
          name={placed.find((g) => g.id === pinMenu.id)?.name ?? ''}
          counts={pins[pinMenu.id]}
          groups={groups.data ?? []}
          onClose={closePinMenu}
          onOpen={(href) => {
            closePinMenu();
            navigate(href);
          }}
        />
      )}
    </div>
  );
}

/**
 * The mark on the marker whose bubble is open (ADR-206 Inc.2): a ring that stays until the bubble
 * closes, and a ripple that runs once from it each time a press opens one. Drawn under the halo, so
 * the dot and its count stay on top. `r` is already counter-scaled, so both are the same size on
 * screen at any zoom; the ripple grows by a CSS transform, which leaves the radius alone.
 */
function PressMark({ r, pulse }: { r: number; pulse: number }) {
  return (
    <>
      <circle className="geopage-pin-ripple" r={r} key={pulse} />
      <circle className="geopage-pin-ring" r={r} />
    </>
  );
}

/** The bubble a pin opens (ADR-191 Inc.15): the site's name, its state and size, the nodes under
 *  it that have an active alert, and the two places to go next. */
function PinBubble({
  pinId,
  at,
  name,
  counts,
  groups,
  onClose,
  onOpen,
}: {
  pinId: string;
  at: { x: number; y: number };
  name: string;
  counts: StateCounts | undefined;
  groups: NodeGroup[];
  onClose: () => void;
  onOpen: (href: string) => void;
}) {
  const { t } = useTranslation('topology');
  const alerts = useAlertStore((s) => s.alerts);
  const sorted = useMemo(() => sortedAlerts(alerts), [alerts]);
  const ids = useMemo(() => alertNodeIds(sorted), [sorted]);
  const idsKey = ids.join(',');
  // undefined = still asking; null = this core cannot say where a node is filed.
  const [places, setPlaces] = useState<Map<string, NodePlace> | null | undefined>(undefined);
  useEffect(() => {
    let live = true;
    const wanted = idsKey ? idsKey.split(',') : [];
    Promise.all(nodeNameBatches(wanted).map((b) => api.getNodeNames(b)))
      .then((answers) => {
        if (live) setPlaces(placesFromNames(answers.flat()));
      })
      .catch(() => {
        // A failed read places nothing; the bubble still offers both ways out.
        if (live) setPlaces(null);
      });
    return () => {
      live = false;
    };
  }, [idsKey]);

  const pinOf = useMemo(
    () => new Map(groups.flatMap((g) => (g.geo_group ? [[g.id, g.geo_group] as const] : []))),
    [groups],
  );
  const rows = places ? pinAlertRows(sorted, places, pinOf, pinId) : null;
  const total = counts ? countsTotal(counts) : 0;
  const empty = total === 0;

  return (
    <AnchoredPopover
      open
      at={at}
      role="dialog"
      label={name}
      className="geopage-pin-menu"
      onDismiss={onClose}
      // Focus the first thing to press once the bubble is placed (it is hidden while being
      // measured, and a hidden element cannot take focus), so a keyboard-opened bubble is usable.
      // An empty site's bubble has no way out but ✕, so that takes the focus instead.
      onPlacedChange={(placed) => {
        if (placed)
          document
            .querySelector<HTMLButtonElement>(
              `.geopage-pin-menu ${empty ? '.geopage-bubble-close' : '.geopage-bubble-go'}`,
            )
            ?.focus({ preventScroll: true });
      }}
    >
      <div className="geopage-bubble-head">
        <span className="geopage-bubble-name" title={name}>
          {name}
        </span>
        <button
          type="button"
          className="geopage-bubble-close"
          aria-label={t('common:actions.close')}
          onClick={onClose}
        >
          ✕
        </button>
      </div>
      <div className="geopage-bubble-sum">
        {stateBreakdown(counts).map(({ state, count }) => (
          <span key={state} className="geopage-bubble-state">
            <span className="geopage-swatch" style={{ background: stateColorVar(state) }} />
            {count} {stateLabel(state)}
          </span>
        ))}
        <span>{t('geo.bubble.nodes', { count: total })}</span>
        {rows && rows.length > 0 && <span>{t('geo.bubble.withAlerts', { count: rows.length })}</span>}
      </div>
      {/* An empty site has nothing to report and nowhere worth going: both ways out open an
          empty screen, and "No active alerts" would be true of nothing. */}
      {!empty && places === undefined && (
        <p className="geopage-bubble-note muted">{t('common:loading')}</p>
      )}
      {!empty && rows && rows.length === 0 && (
        <p className="geopage-bubble-note muted">{t('geo.bubble.noAlerts')}</p>
      )}
      {rows && rows.length > 0 && (
        <div className="geopage-bubble-list">
          {rows.slice(0, PIN_ALERT_ROWS).map((r) => (
            <button
              key={r.nodeId}
              type="button"
              className="geopage-bubble-go geopage-bubble-row"
              onClick={() => onOpen(nodeHref(r.nodeId))}
            >
              <span
                className="geopage-bubble-dot"
                style={{ background: severityColorVar(r.worst.severity) }}
              />
              <span className="geopage-bubble-what">
                <span className="mono">{r.name}</span>{' '}
                <AlertWhatText what={alertWhatOf(r.worst)} />
              </span>
              <span className="geopage-bubble-age muted">{relativeTimeMs(r.worst.at_unix_ms)}</span>
            </button>
          ))}
          {rows.length > PIN_ALERT_ROWS && (
            <p className="geopage-bubble-more muted">
              {t('geo.bubble.more', { count: rows.length - PIN_ALERT_ROWS })}
            </p>
          )}
        </div>
      )}
      {!empty && (
        <div className="geopage-bubble-actions">
          <button
            type="button"
            className="geopage-bubble-go"
            onClick={() => onOpen(nodesPageHref({ kind: 'group', id: pinId }))}
          >
            {t('geo.pinMenu.nodes')}
          </button>
          <button
            type="button"
            className="geopage-bubble-go"
            onClick={() => onOpen(topologyMapHref({ group: pinId }))}
          >
            {t('geo.pinMenu.map')}
          </button>
        </div>
      )}
    </AnchoredPopover>
  );
}

/** The list an overlapping marker opens (ADR-206): each site under it, worst first. A row hands
 *  over to that site's own bubble; "Zoom in" is offered only while zooming can pull them apart. */
function ClusterBubble({
  cluster,
  at,
  canZoom,
  onClose,
  onPick,
  onZoom,
}: {
  cluster: PinCluster;
  at: { x: number; y: number };
  canZoom: boolean;
  onClose: () => void;
  onPick: (id: string) => void;
  onZoom: () => void;
}) {
  const { t } = useTranslation('topology');
  const title = t('geo.cluster.title', { count: cluster.members.length });
  return (
    <AnchoredPopover
      open
      at={at}
      role="dialog"
      label={title}
      className="geopage-pin-menu"
      onDismiss={onClose}
      onPlacedChange={(placed) => {
        if (placed)
          document
            .querySelector<HTMLButtonElement>('.geopage-pin-menu .geopage-bubble-row')
            ?.focus({ preventScroll: true });
      }}
    >
      <div className="geopage-bubble-head">
        <span className="geopage-bubble-name">{title}</span>
        <button
          type="button"
          className="geopage-bubble-close"
          aria-label={t('common:actions.close')}
          onClick={onClose}
        >
          ✕
        </button>
      </div>
      <div className="geopage-bubble-list geopage-cluster-list">
        {cluster.members.map((m) => {
          const look = pinLook(m.counts);
          const total = m.counts ? countsTotal(m.counts) : 0;
          return (
            <button
              key={m.id}
              type="button"
              className="geopage-bubble-go geopage-bubble-row"
              onClick={() => onPick(m.id)}
            >
              <span
                className={`geopage-bubble-dot${look === 'empty' ? ' geopage-bubble-dot-empty' : ''}`}
                style={look === 'empty' ? undefined : { background: stateColorVar(look) }}
              />
              <span className="geopage-bubble-what" title={m.name}>
                {m.name}
              </span>
              <span className="geopage-bubble-age muted">
                {look === 'empty' ? t('geo.noNodes') : t('geo.bubble.nodes', { count: total })}
              </span>
            </button>
          );
        })}
      </div>
      {canZoom && (
        <div className="geopage-bubble-actions">
          <button type="button" className="geopage-bubble-go" onClick={onZoom}>
            {t('geo.cluster.zoomIn')}
          </button>
        </div>
      )}
    </AnchoredPopover>
  );
}
