// SPDX-License-Identifier: AGPL-3.0-only
// Topology ▸ Geo map. A pin per placed node group on a bundled world outline, coloured by that
// group's worst member state — "which of my sites is in trouble, and where is it".
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

import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { useNavigate } from 'react-router-dom';
import { Card } from '../components/ui/Card';
import { PageHeader } from '../components/ui/PageHeader';
import { useGroupSummary } from '../dashboard/useGroupSummary';
import { usePolled } from '../dashboard/usePolled';
import { countsTotal, worstStateFromCounts } from '../dashboard/widgets/util';
import { stateColorVar, stateLabel } from '../lib/format';
import { api } from '../services/api';
import {
  clampGeoScale,
  fitPins,
  MAP_HEIGHT,
  MAP_WIDTH,
  MAX_GEO_SCALE,
  MIN_GEO_SCALE,
  placedOnly,
  project,
  type GeoView,
} from './geoProjection';
import { WORLD_OUTLINE } from './worldOutline';
import './GeoMapPage.css';

export function GeoMapPage() {
  const { t } = useTranslation('topology');
  const navigate = useNavigate();
  const wrapRef = useRef<HTMLDivElement | null>(null);
  const [view, setView] = useState<GeoView | null>(null);
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
  const groups = usePolled(() => api.listNodeGroups(), []);

  const placed = useMemo(() => placedOnly(groups.data ?? []), [groups.data]);

  const fit = useCallback(() => {
    const el = wrapRef.current;
    if (!el) return;
    setView(fitPins(placed, el.clientWidth, el.clientHeight));
  }, [placed]);

  // Fit once, on the first render that has a measured pane. The `view === null` guard is what stops
  // the 15s poll refresh from stomping the operator's pan/zoom every tick — the same guard, and the
  // same reason, as `TopologyMap`.
  useEffect(() => {
    if (view === null && wrapRef.current && wrapRef.current.clientWidth > 0) {
      setView(fitPins(placed, wrapRef.current.clientWidth, wrapRef.current.clientHeight));
    }
  }, [view, placed]);

  const onWheel = useCallback((e: React.WheelEvent) => {
    const el = wrapRef.current;
    if (!el) return;
    const rect = el.getBoundingClientRect();
    const mx = e.clientX - rect.left;
    const my = e.clientY - rect.top;
    setView((v) => {
      if (!v) return v;
      const scale = clampGeoScale(v.scale * (e.deltaY < 0 ? 1.15 : 1 / 1.15));
      const k = scale / v.scale;
      // Keep the point under the cursor fixed while zooming.
      return { scale, tx: mx - (mx - v.tx) * k, ty: my - (my - v.ty) * k };
    });
  }, []);

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
      setView((v) => {
        if (!v) return v;
        const scale = clampGeoScale(p.scale * (dist / p.dist));
        const k = scale / p.scale;
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
  const counts = summary?.groups ?? {};
  const busy = (loading && !summary) || (groups.loading && !groups.data);

  return (
    <div className="geopage">
      <PageHeader title={t('geo.title')} note={t('geo.subtitle')} />
      {(error || groups.error) && (
        <Card>
          <p className="muted">{t('geo.error')}</p>
        </Card>
      )}
      {busy && <p className="muted">{t('common:loading')}</p>}
      {!busy && !error && !groups.error && (
        <>
          {/* The empty state is a caption over a live map, not instead of one: an operator with no
              coordinates set needs to see what the page is for and be told where to set them. */}
          {placed.length === 0 && <p className="geopage-empty muted">{t('geo.empty')}</p>}
          <div className="geopage-map" ref={wrapRef}>
            <div className="geopage-controls">
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
                onClick={() =>
                  setView((s) => (s ? { ...s, scale: Math.min(MAX_GEO_SCALE, s.scale * 1.3) } : s))
                }
                title={t('map.control.zoomIn')}
                aria-label={t('map.control.zoomIn')}
              >
                +
              </button>
              <button
                className="geopage-ctl"
                onClick={() =>
                  setView((s) => (s ? { ...s, scale: Math.max(MIN_GEO_SCALE, s.scale / 1.3) } : s))
                }
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
                {/* Ocean, so the outline reads as land rather than as floating shapes. */}
                <rect className="geopage-ocean" x={0} y={0} width={MAP_WIDTH} height={MAP_HEIGHT} />
                {WORLD_OUTLINE.map((d, i) => (
                  <path className="geopage-land" d={d} key={i} />
                ))}
                {placed.map((g) => {
                  const p = project(g.latitude, g.longitude);
                  const c = counts[g.id];
                  const worst = c ? worstStateFromCounts(c) : 'ok';
                  const total = c ? countsTotal(c) : 0;
                  // Counter-scaled so a pin stays the same size on screen at any zoom — a pin that
                  // grows with the map turns into a blob that hides the site it marks.
                  const r = 5 / v.scale;
                  return (
                    <g
                      className="geopage-pin"
                      key={g.id}
                      transform={`translate(${p.x} ${p.y})`}
                      role="button"
                      tabIndex={0}
                      onClick={() => navigate(`/nodes?sel=${g.id}`)}
                      onKeyDown={(e) => {
                        if (e.key === 'Enter' || e.key === ' ') {
                          e.preventDefault();
                          navigate(`/nodes?sel=${g.id}`);
                        }
                      }}
                    >
                      <title>
                        {t('geo.pinTitle', {
                          name: g.name,
                          state: stateLabel(worst),
                          count: total,
                        })}
                      </title>
                      {/* Halo first so the dot reads against both land and ocean. */}
                      <circle className="geopage-pin-halo" r={r * 1.9} />
                      <circle
                        className="geopage-pin-dot"
                        r={r}
                        style={{ fill: stateColorVar(worst) }}
                      />
                    </g>
                  );
                })}
              </g>
            </svg>
          </div>
          {/* Never colour alone (ui-conventions.md): the legend names each state in text. */}
          <ul className="geopage-legend">
            {(['ok', 'warning', 'critical', 'unreachable', 'unknown'] as const).map((s) => (
              <li key={s}>
                <span className="geopage-swatch" style={{ background: stateColorVar(s) }} />
                {stateLabel(s)}
              </li>
            ))}
          </ul>
        </>
      )}
    </div>
  );
}
