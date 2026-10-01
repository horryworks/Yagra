// SPDX-License-Identifier: AGPL-3.0-only
// The network map canvas: an SVG render of one folder level (ADR-191) with wheel-zoom + drag-pan.
// The layout is in `graphLayout.ts` and is deterministic — see its header for why that and the
// fit-once guard below are both required and neither substitutes for the other.
//
// Controlled: the page owns what is selected and what a click means (select a node, enter a
// folder, follow a stub). Three kinds of box share one renderer; a stub is drawn dashed.
// Status color is the canonical palette (stateColorVar) — a node's color here is identical to its
// dot in the table and its threshold line in a chart. Device-supplied names render as React <text>
// children, so they're auto-escaped (no dangerouslySetInnerHTML) — device data is untrusted.

import { useCallback, useEffect, useRef } from 'react';
import { useTranslation } from 'react-i18next';
import { stateColorVar } from '../../lib/format';
import { useStoredMapView } from '../../lib/storedMapView';
import { useMapViewStore, type MapViewKey } from '../../store';
import type { GraphLayout, PlacedEdge, PlacedNode } from './graphLayout';
import { clampScale, fitView, MAX_SCALE, MIN_SCALE } from './fitView';
import { activateOnKey, fitLabel, wheelZooms } from './topologyLevel';
import './TopologyMap.css';

/** How long the "hold Ctrl to zoom" hint stays up after a plain wheel turn. */
const WHEEL_HINT_MS = 1200;

/** The accent bar and dot take this much of a box's left edge. */
const LABEL_X = 34;

function Box({
  node,
  selected,
  title,
  onActivate,
}: {
  node: PlacedNode;
  selected: boolean;
  title: string;
  onActivate: (node: PlacedNode) => void;
}) {
  const cls = [
    'topomap-node',
    `topomap-kind-${node.kind}`,
    node.suppressed ? 'suppressed' : '',
    selected ? 'selected' : '',
  ]
    .filter(Boolean)
    .join(' ');
  const external = node.kind === 'external';
  const nameY = node.sub ? node.h / 2 - 3 : node.h / 2 + 4;
  return (
    <g
      className={cls}
      transform={`translate(${node.cx - node.w / 2}, ${node.cy - node.h / 2})`}
      role="button"
      tabIndex={0}
      aria-label={title}
      aria-pressed={selected}
      onClick={() => onActivate(node)}
      onKeyDown={(e) => activateOnKey(e, () => onActivate(node))}
    >
      <title>{title}</title>
      <rect className="topomap-box" width={node.w} height={node.h} rx={6} />
      {/* Status is carried by the left accent bar + dot, never color-alone: the <title> label
          reads the same state. A stub has no state of its own and carries neither. */}
      {!external && (
        <>
          <rect
            className="topomap-accent"
            width={4}
            height={node.h}
            rx={2}
            style={{ fill: stateColorVar(node.state) }}
          />
          <circle cx={20} cy={node.h / 2} r={5} style={{ fill: stateColorVar(node.state) }} />
        </>
      )}
      <text className="topomap-label" x={external ? 12 : LABEL_X} y={nameY}>
        {fitLabel(node.name, node.w, external ? 12 : LABEL_X)}
      </text>
      {node.sub && (
        <text className="topomap-sub" x={external ? 12 : LABEL_X} y={nameY + 17}>
          {fitLabel(node.sub, node.w, external ? 12 : LABEL_X)}
        </text>
      )}
    </g>
  );
}

function Edge({
  edge,
  selected,
  title,
  showChip,
  onSelect,
}: {
  edge: PlacedEdge;
  selected: boolean;
  title: string;
  showChip: boolean;
  onSelect: (id: string) => void;
}) {
  const cls = `topomap-edge ${edge.source}${edge.suppressed ? ' suppressed' : ''}${
    selected ? ' selected' : ''
  }`;
  const width = Math.min(1.5 + Math.log2(Math.max(1, edge.count)), 6);
  const shape = (className: string, strokeWidth?: number) =>
    edge.kind === 'bow' ? (
      <path className={className} d={edge.path} fill="none" style={{ strokeWidth }} />
    ) : (
      <line
        className={className}
        x1={edge.x1}
        y1={edge.y1}
        x2={edge.x2}
        y2={edge.y2}
        style={{ strokeWidth }}
      />
    );
  const label = String(edge.count);
  return (
    <g className="topomap-edge-group" onClick={() => onSelect(edge.id)}>
      <title>{title}</title>
      {/* A wide transparent twin makes a thin line clickable without making it look thick. */}
      {shape('topomap-edge-hit')}
      {shape(cls, width)}
      {showChip && (
        <g
          className={`topomap-chip${selected ? ' selected' : ''}`}
          transform={`translate(${edge.chip.x}, ${edge.chip.y})`}
          role="button"
          tabIndex={0}
          aria-label={title}
          aria-pressed={selected}
          onKeyDown={(e) => activateOnKey(e, () => onSelect(edge.id))}
        >
          <rect x={-(8 + label.length * 3.5)} y={-10} width={16 + label.length * 7} height={20} rx={10} />
          <text y={4} textAnchor="middle">
            {label}
          </text>
        </g>
      )}
    </g>
  );
}

export interface TopologyMapProps {
  layout: GraphLayout;
  /** The selected box's id, or null. */
  selectedId: string | null;
  /** The selected edge's id, or null. */
  selectedEdge: string | null;
  /** Changes when the level changes; a new level starts from a fresh fit. */
  fitKey: string;
  /** The tooltip for a box. */
  boxTitle: (node: PlacedNode) => string;
  /** The tooltip for an edge. */
  edgeTitle: (edge: PlacedEdge) => string;
  /** Whether an edge shows its count chip. */
  showChip: (edge: PlacedEdge) => boolean;
  onActivate: (node: PlacedNode) => void;
  onSelectEdge: (id: string) => void;
  /** Which remembered view this drawing uses. The full map and a folder pane's map keep separate
   *  ones (ADR-191 Inc.2). */
  viewKey?: MapViewKey;
  /** Inside a scrolling pane: the plain wheel scrolls the page and Ctrl/⌘ + wheel zooms. */
  wheelNeedsModifier?: boolean;
  /** The hint shown when the plain wheel is left to the page. */
  wheelHint?: string;
  /** Whether one finger pans the map. Off inside a pane, where one finger scrolls the page. */
  touchPans?: boolean;
}

export function TopologyMap({
  layout,
  selectedId,
  selectedEdge,
  fitKey,
  boxTitle,
  edgeTitle,
  showChip,
  onActivate,
  onSelectEdge,
  viewKey = 'topo',
  wheelNeedsModifier = false,
  wheelHint,
  touchPans = true,
}: TopologyMapProps) {
  const { t } = useTranslation('topology');
  const wrapRef = useRef<HTMLDivElement | null>(null);
  // Where the operator panned and zoomed to, remembered for the session (ADR-134). The `view ===
  // null` guard below stops the refresh from re-fitting the diagram; a new level clears it.
  const [view, setView] = useStoredMapView(viewKey);
  // The hint is toggled on the element itself: a state change here would redraw every box and line
  // of the map twice per wheel turn, for a label.
  const hintRef = useRef<HTMLDivElement | null>(null);
  const drag = useRef<{ x: number; y: number; tx: number; ty: number } | null>(null);
  // Live pointers by id. One pointer pans; two pointers pinch-zoom (touch). `pinch` freezes the
  // view at the moment the second finger lands so scale/pan stay anchored to the gesture.
  const pointers = useRef<Map<number, { x: number; y: number }>>(new Map());
  const pinch = useRef<{ dist: number; cx: number; cy: number; scale: number; tx: number; ty: number } | null>(
    null,
  );

  // A different level than the one the stored view belongs to: fit again. The level is stored with
  // the view (sessionStorage), so a reload keeps the position rather than reading as a new level.
  const enterLevel = useMapViewStore((s) => s.enterLevel);
  useEffect(() => {
    if (enterLevel(viewKey, fitKey)) setView(null);
  }, [enterLevel, fitKey, viewKey, setView]);

  // Manual "Fit to view" (also the initial fit). Re-measures the current container each call.
  const fit = useCallback(() => {
    const el = wrapRef.current;
    if (!el) return;
    setView(fitView(layout, el.clientWidth, el.clientHeight));
  }, [layout, setView]);

  // Fit once, on the first render that has both a measured container and a laid-out diagram.
  useEffect(() => {
    if (view === null && wrapRef.current && layout.width > 0) {
      setView(fitView(layout, wrapRef.current.clientWidth, wrapRef.current.clientHeight));
    }
  }, [view, layout, setView]);

  // The wheel is a native listener, not React's `onWheel`: React registers wheel listeners as
  // passive, so a `preventDefault()` there cannot stop the page from scrolling under the zoom.
  const svgRef = useRef<SVGSVGElement | null>(null);
  useEffect(() => {
    const svg = svgRef.current;
    const el = wrapRef.current;
    if (!svg || !el) return;
    let hintTimer: ReturnType<typeof setTimeout> | undefined;
    const onWheel = (e: WheelEvent) => {
      if (!wheelZooms(e, wheelNeedsModifier)) {
        // Left to the page; say once, briefly, how to zoom instead.
        hintRef.current?.classList.add('on');
        clearTimeout(hintTimer);
        hintTimer = setTimeout(() => hintRef.current?.classList.remove('on'), WHEEL_HINT_MS);
        return;
      }
      e.preventDefault();
      const rect = el.getBoundingClientRect();
      const mx = e.clientX - rect.left;
      const my = e.clientY - rect.top;
      setView((v) => {
        if (!v) return v;
        const factor = e.deltaY < 0 ? 1.1 : 1 / 1.1;
        const scale = clampScale(v.scale * factor);
        const k = scale / v.scale;
        // Keep the point under the cursor fixed while zooming.
        return { scale, tx: mx - (mx - v.tx) * k, ty: my - (my - v.ty) * k };
      });
    };
    svg.addEventListener('wheel', onWheel, { passive: false });
    return () => {
      svg.removeEventListener('wheel', onWheel);
      clearTimeout(hintTimer);
    };
  }, [setView, wheelNeedsModifier]);

  const onPointerDown = useCallback(
    (e: React.PointerEvent) => {
      if (!view) return;
      // One finger scrolls the pane instead (`touch-action: pan-y`); buttons still move the map.
      if (!touchPans && e.pointerType === 'touch') return;
      // Capture on the actual target (bubbles to this SVG either way) so a box's click/keyboard
      // action keeps working.
      (e.target as Element).setPointerCapture?.(e.pointerId);
      pointers.current.set(e.pointerId, { x: e.clientX, y: e.clientY });
      const el = wrapRef.current;
      if (pointers.current.size >= 2 && el) {
        // Second finger down → begin a pinch. Freeze the current view + finger midpoint as the
        // baseline the move handler scales/translates against.
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
        drag.current = null; // a pan in progress yields to the pinch
      } else {
        drag.current = { x: e.clientX, y: e.clientY, tx: view.tx, ty: view.ty };
      }
    },
    [view, touchPans],
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
      setView((v) => {
        if (!v) return v;
        const scale = clampScale(p.scale * (dist / p.dist));
        const k = scale / p.scale;
        // Zoom anchored on the pinch's start midpoint (keeps that world point fixed), then translate
        // by however far the live midpoint has drifted since — that's the two-finger pan.
        return {
          scale,
          tx: p.cx - (p.cx - p.tx) * k + (mx - p.cx),
          ty: p.cy - (p.cy - p.ty) * k + (my - p.cy),
        };
      });
      return;
    }
    const d = drag.current;
    if (!d) return;
    setView((v) => (v ? { ...v, tx: d.tx + (e.clientX - d.x), ty: d.ty + (e.clientY - d.y) } : v));
  }, [setView]);
  const onPointerUp = useCallback((e: React.PointerEvent) => {
    pointers.current.delete(e.pointerId);
    (e.target as Element).releasePointerCapture?.(e.pointerId);
    if (pointers.current.size < 2) pinch.current = null;
    if (pointers.current.size === 1) {
      // Lifted one finger of a pinch → hand off to a pan anchored on the finger still down, from
      // the current view (read via the setView updater so we don't need `view` in the deps).
      const [only] = [...pointers.current.values()];
      setView((v) => {
        drag.current = v ? { x: only.x, y: only.y, tx: v.tx, ty: v.ty } : null;
        return v;
      });
    } else if (pointers.current.size === 0) {
      drag.current = null;
    }
  }, [setView]);

  const v = view ?? { tx: 0, ty: 0, scale: 1 };

  return (
    <div className="topomap" ref={wrapRef}>
      <div className="topomap-controls">
        <button
          className="topomap-ctl"
          onClick={fit}
          title={t('map.control.fitToView')}
          aria-label={t('map.control.fitToView')}
        >
          {t('map.control.fit')}
        </button>
        <button
          className="topomap-ctl"
          onClick={() => setView((s) => (s ? { ...s, scale: Math.min(MAX_SCALE, s.scale * 1.2) } : s))}
          title={t('map.control.zoomIn')}
          aria-label={t('map.control.zoomIn')}
        >
          +
        </button>
        <button
          className="topomap-ctl"
          onClick={() => setView((s) => (s ? { ...s, scale: Math.max(MIN_SCALE, s.scale / 1.2) } : s))}
          title={t('map.control.zoomOut')}
          aria-label={t('map.control.zoomOut')}
        >
          −
        </button>
      </div>
      {wheelHint && (
        <div ref={hintRef} className="topomap-wheel-hint" aria-hidden="true">
          {wheelHint}
        </div>
      )}
      <svg
        ref={svgRef}
        className={`topomap-svg${touchPans ? '' : ' scrolls'}`}
        width="100%"
        height="100%"
        onPointerDown={onPointerDown}
        onPointerMove={onPointerMove}
        onPointerUp={onPointerUp}
        onPointerCancel={onPointerUp}
        onPointerLeave={onPointerUp}
      >
        <g transform={`translate(${v.tx}, ${v.ty}) scale(${v.scale})`}>
          {layout.edges.map((e) => (
            <Edge
              key={e.id}
              edge={e}
              selected={e.id === selectedEdge}
              title={edgeTitle(e)}
              showChip={showChip(e)}
              onSelect={onSelectEdge}
            />
          ))}
          {layout.nodes.map((n) => (
            <Box
              key={n.id}
              node={n}
              selected={n.id === selectedId}
              title={boxTitle(n)}
              onActivate={onActivate}
            />
          ))}
        </g>
      </svg>
    </div>
  );
}
