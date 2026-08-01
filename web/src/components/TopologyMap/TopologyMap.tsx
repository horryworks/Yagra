// SPDX-License-Identifier: AGPL-3.0-only
// The dependency map itself: an SVG render of the tidy-tree layout with wheel-zoom + drag-pan.
// Status color is the canonical palette (stateColorVar) — a node's color here is identical to its
// dot in the table and its threshold line in a chart. Clicking (or Enter/Space on a focused) node
// drills into that node's detail page. Device-supplied names render as React <text> children, so
// they're auto-escaped (no dangerouslySetInnerHTML) — device data is untrusted.

import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { useNavigate } from 'react-router-dom';
import { stateColorVar, stateLabel } from '../../lib/format';
import type { PlacedNode, TopologyLayout } from './layout';
import { NODE_H, NODE_W } from './layout';
import { clampScale, fitView, MAX_SCALE, MIN_SCALE, type View } from './fitView';
import './TopologyMap.css';

function NodeBox({
  node,
  onOpen,
  nameById,
}: {
  node: PlacedNode;
  onOpen: (id: string) => void;
  nameById: Map<string, string>;
}) {
  const { t } = useTranslation('topology');
  const cause = node.rootCause ? nameById.get(node.rootCause) ?? null : null;
  const title = cause
    ? t('map.nodeTitleSuppressed', { name: node.name, state: stateLabel(node.state), cause })
    : t('map.nodeTitle', { name: node.name, state: stateLabel(node.state) });
  const cls = ['topomap-node', node.suppressed ? 'suppressed' : ''].filter(Boolean).join(' ');
  return (
    <g
      className={cls}
      transform={`translate(${node.cx - NODE_W / 2}, ${node.cy - NODE_H / 2})`}
      role="button"
      tabIndex={0}
      aria-label={title}
      onClick={() => onOpen(node.id)}
      onKeyDown={(e) => {
        if (e.key === 'Enter' || e.key === ' ') {
          e.preventDefault();
          onOpen(node.id);
        }
      }}
    >
      <title>{title}</title>
      {/* Status is carried by the left accent bar + dot, never color-alone: the name text and the
          <title> label read the same state for color-blind/AT users. */}
      <rect className="topomap-box" width={NODE_W} height={NODE_H} rx={6} />
      <rect
        className="topomap-accent"
        width={4}
        height={NODE_H}
        rx={2}
        style={{ fill: stateColorVar(node.state) }}
      />
      <circle cx={20} cy={NODE_H / 2} r={5} style={{ fill: stateColorVar(node.state) }} />
      <text className="topomap-label" x={34} y={NODE_H / 2 + 4}>
        {node.name}
      </text>
    </g>
  );
}

export function TopologyMap({ layout }: { layout: TopologyLayout }) {
  const { t } = useTranslation('topology');
  const navigate = useNavigate();
  const wrapRef = useRef<HTMLDivElement | null>(null);
  const [view, setView] = useState<View | null>(null);
  const drag = useRef<{ x: number; y: number; tx: number; ty: number } | null>(null);
  // Live pointers by id. One pointer pans; two pointers pinch-zoom (touch). `pinch` freezes the
  // view at the moment the second finger lands so scale/pan stay anchored to the gesture.
  const pointers = useRef<Map<number, { x: number; y: number }>>(new Map());
  const pinch = useRef<{ dist: number; cx: number; cy: number; scale: number; tx: number; ty: number } | null>(
    null,
  );

  const nameById = useMemo(
    () => new Map(layout.nodes.map((n) => [n.id, n.name])),
    [layout.nodes],
  );

  // Manual "Fit to view" (also the initial fit). Re-measures the current container each call.
  const fit = useCallback(() => {
    const el = wrapRef.current;
    if (!el) return;
    setView(fitView(layout, el.clientWidth, el.clientHeight));
  }, [layout]);

  // Fit once, on the first render that has both a measured container and a laid-out diagram.
  // A poll refresh re-creates `layout` every 15s, but the `view === null` guard keeps it from
  // stomping the operator's current pan/zoom — only the very first paint auto-fits.
  useEffect(() => {
    if (view === null && wrapRef.current && layout.width > 0) {
      setView(fitView(layout, wrapRef.current.clientWidth, wrapRef.current.clientHeight));
    }
  }, [view, layout]);

  const onWheel = useCallback((e: React.WheelEvent) => {
    const el = wrapRef.current;
    if (!el) return;
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
  }, []);

  const onPointerDown = useCallback(
    (e: React.PointerEvent) => {
      if (!view) return;
      // Capture on the actual target (bubbles to this SVG either way) so a node's click/keyboard
      // drill-through keeps working exactly as before — unchanged from the single-pointer version.
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
  }, []);
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
  }, []);

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
      <svg
        className="topomap-svg"
        width="100%"
        height="100%"
        onWheel={onWheel}
        onPointerDown={onPointerDown}
        onPointerMove={onPointerMove}
        onPointerUp={onPointerUp}
        onPointerCancel={onPointerUp}
        onPointerLeave={onPointerUp}
      >
        <g transform={`translate(${v.tx}, ${v.ty}) scale(${v.scale})`}>
          {layout.edges.map((e) => (
            <line
              key={e.id}
              className={`topomap-edge${e.suppressed ? ' suppressed' : ''}`}
              x1={e.x1}
              y1={e.y1}
              x2={e.x2}
              y2={e.y2}
            />
          ))}
          {layout.nodes.map((n) => (
            <NodeBox key={n.id} node={n} onOpen={(id) => navigate(`/nodes/${id}`)} nameById={nameById} />
          ))}
        </g>
      </svg>
    </div>
  );
}
