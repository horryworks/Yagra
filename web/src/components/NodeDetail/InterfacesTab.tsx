// SPDX-License-Identifier: AGPL-3.0-only
// Interfaces tab of the unified node detail — Direction C: keep the interface LIST and the selected
// interface's CHARTS on screen together. Each row carries a small last-1h throughput sparkline so
// trends across all interfaces are visible without selecting anything (fast triage of "which port is
// busy / flapping"); selecting a row opens a pinned detail dock at the bottom (throughput + error
// charts, range control, stat tiles). The list scrolls above the dock; the dock stays put. The tab
// body is a flex column — toolbar (none) → list (flex:1, scrolls) → dock/hint (none). The list
// refreshes on an interval (shared with the tab badge); per-interface series are loaded lazily.

import { useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState } from 'react';
import { useTranslation } from 'react-i18next';
import type { TFunction } from 'i18next';
import { api } from '../../services/api';
import { formatBps, formatPps, formatSi } from '../../lib/format';
import type { InterfaceRow, InterfaceSeries } from '../../types/api';
import { StatusDot } from '../ui/StatusDot';
import { MetricChart, SERIES_IN, SERIES_OUT } from '../MetricChart/MetricChart';
import { operState } from './healthTone';
import { RangeControl, resolveRange } from './RangeControl';
import { useRangeStore } from '../../store';
import { usePrefsStore } from '../../prefs';
import { setInterfaceDockHeight } from '../../serverPrefs';
import { useViewportMode } from '../../lib/viewport';
import { useRefreshTick } from '../../lib/refreshTick';
import {
  latestDiscardRate,
  latestErrorRate,
  sparklinePath,
  throughputBandwidthOverlay,
  throughputPair,
} from './interfaceMetrics';
import {
  defaultDockHeight,
  heightFromDrag,
  heightFromKey,
  resolveDockHeight,
  DOCK_MIN_PX,
  LIST_MIN_PX,
} from './interfaceDockHeight';
import { interfaceColumns } from './tabFilters';
import { ColumnFilterRow } from '../ui/ColumnFilterRow';
import { ClearFilters } from '../ui/ClearFilters';
import { FilterButton, MobileFilterSheet } from '../ui/MobileFilterSheet';
import { defaultFilters, isAnyFiltered, type FilterState } from '../../lib/columnFilter';
import { facetCounts } from '../../lib/filterCounts';
import { buildPredicate } from '../../lib/filterPredicate';

// In-row sparkline window: last hour at a coarse step (cheap; trend, not precision).
const SPARK_WINDOW_SECS = 3600;
const SPARK_STEP_SECS = 120;
const SPARK_W = 120;
const SPARK_H = 26;
/** The list's sticky chrome — the header and, on a desktop, the filter row under it — measured
 *  rather than mirrored from CSS. It was a `LIST_HEAD_H = 32` constant, and both of the things a
 *  mirror does went wrong: ADR-053 added a 34px sticky filter row below the header and nobody
 *  updated the number, so a selected row near the top was scrolled *under* the filter row; and
 *  mobile hides both, so the number reserved 32px of nothing. A `display: none` element measures 0,
 *  which is exactly the answer wanted in that case. */
function stickyChromeHeight(list: HTMLElement): number {
  return Array.from(list.querySelectorAll('.nd-if-head, .nd-if-filters')).reduce(
    (h, el) => h + el.getBoundingClientRect().height,
    0,
  );
}

/** The height `.nd-if-list` and the dock share: the tab body minus the fixed chrome above them —
 *  the toolbar, plus the error banner when one is showing. Measured rather than mirrored, for the
 *  same reason `stickyChromeHeight` measures.
 *
 *  ⚠️ NOT `window.innerHeight`. This pane sits under the app shell's top bar, the node identity
 *  header and the tab strip, which cost roughly 230px on a 720p window — sizing the dock against the
 *  window would let it push the list off the bottom.
 *  ⚠️ A future `flex: none` sibling of `.nd-if` must be added to this selector, or the dock
 *  over-claims by that sibling's height. */
function dockBudget(root: HTMLElement): number {
  const chrome = Array.from(
    root.querySelectorAll(':scope > .nd-if-toolbar, :scope > .form-error'),
  ).reduce((h, el) => h + el.getBoundingClientRect().height, 0);
  return root.clientHeight - chrome;
}

/** Human oper label from ifOperStatus (1 = up). */
function operLabel(oper: number | null, t: TFunction): string {
  if (oper == null) return t('interfaces.operUnknown');
  return oper === 1 ? t('interfaces.operUp') : t('interfaces.operDown');
}

interface Props {
  nodeId: string;
  /** Interface rows (loaded + interval-refreshed by the orchestrator, shared with the tab badge). */
  rows: InterfaceRow[];
  loaded: boolean;
  error: string | null;
}

/** Everything the dock needs to be resizable, or `null` where it is not (mobile). Bundled because
 *  the alternative is seven props whose one shared meaning is "the operator can drag this". */
interface DockResize {
  /** Current height in px, already clamped for the container. */
  height: number;
  /** Bounds, for the handle's `aria-valuemin`/`aria-valuemax`. Same numbers the drag obeys —
   *  they come from `interfaceDockHeight.ts` rather than being restated here. */
  min: number;
  max: number;
  /** Double-click: back to the height an operator who never dragged would get. */
  onReset: () => void;
  handleProps: {
    onPointerDown: (e: React.PointerEvent) => void;
    onPointerMove: (e: React.PointerEvent) => void;
    onPointerUp: (e: React.PointerEvent) => void;
    onPointerCancel: (e: React.PointerEvent) => void;
    onKeyDown: (e: React.KeyboardEvent) => void;
  };
}

export function InterfacesTab({ nodeId, rows, loaded, error }: Props) {
  const { t } = useTranslation('nodes');
  // The predicate lives in `tabFilters.ts`: it was hand-rolled here and searched two fields
  // where the row shows five, and a `.tsx` is a file no test runs (testing.md).
  const columns = useMemo(() => interfaceColumns(t), [t]);
  const [filters, setFilters] = useState<FilterState>(() => defaultFilters(columns));
  const [sheet, setSheet] = useState(false);
  const [selected, setSelected] = useState<number | null>(null);
  const listRef = useRef<HTMLDivElement>(null);
  // The tab root, held in *state* rather than a ref. ⚠️ A ref does not work here: this component
  // returns an early tree while it has no rows, so on the render the measuring effect first ran
  // `rootRef.current` was null — and with the element arriving later and no dependency naming it,
  // the effect never ran again. The dock then sized against a budget of 0, which means an infinite
  // ceiling, which means a drag could squeeze the interface list to nothing. State makes the
  // element's arrival a render the effect can depend on.
  const [rootEl, setRootEl] = useState<HTMLDivElement | null>(null);

  // ── Dock resize (issue #65) ──────────────────────────────────────────────────────────────────
  // The state and the handlers live HERE rather than in `InterfaceDock`, and that is a constraint
  // rather than a preference: the keep-in-view ResizeObserver below is in this component and has to
  // be able to see that a drag is in progress. Moving `drag` down into the dock silently
  // reintroduces the scroll defect described on that observer.
  const mobile = useViewportMode() === 'mobile';
  const storedHeight = usePrefsStore((s) => s.interfaceDockHeight);
  // The height while a gesture is in flight. Kept local so a drag does not write to the store (and
  // therefore to the server) once per frame — the store gets one write on release.
  const [liveHeight, setLiveHeight] = useState<number | null>(null);
  const [budget, setBudget] = useState(0);
  const drag = useRef<{ y: number; h: number } | null>(null);
  const raf = useRef(0);
  const pointerY = useRef(0);

  const dockHeight = resolveDockHeight(liveHeight ?? storedHeight, budget);

  // Measure the space the list and the dock share. Observes `.nd-if` ONLY, which is `height: 100%`
  // — nothing inside it can change its size, so this observer cannot loop with the dock's own
  // height. (Observing the list or the dock instead would.) The toolbar re-wraps only when the
  // width changes, which resizes `.nd-if` too; the error banner does not, hence the `error` dep.
  useLayoutEffect(() => {
    if (!rootEl) return;
    const measure = () => setBudget(dockBudget(rootEl));
    measure();
    if (typeof ResizeObserver === 'undefined') return;
    const ro = new ResizeObserver(measure);
    ro.observe(rootEl);
    return () => ro.disconnect();
  }, [rootEl, error]);

  const shown = useMemo(
    () => rows.filter(buildPredicate(columns, filters, Date.now())),
    [rows, columns, filters],
  );
  const counts = useMemo(
    () =>
      Object.fromEntries(
        columns
          .filter((c) => c.filter.kind === 'enum')
          .map((c) => [c.key, facetCounts(rows, columns, filters, c.key, Date.now())]),
      ),
    [rows, columns, filters],
  );
  // Keyed by the column each control sits under. ⚠️ Untyped, exactly like `DataTable`'s
  // `specs[c.key]` lookup: rename a key in `tabFilters.ts` and the cell silently stops rendering.
  const labels: Record<string, string> = {
    if_name: t('interfaces.colInterface'),
    if_alias: t('interfaces.colDescription'),
    oper: t('interfaces.colOper'),
  };
  const up = rows.filter((r) => r.oper_status === 1).length;
  const selectedRow = rows.find((r) => r.ifindex === selected) ?? null;

  // Opening the dock shrinks the list, which can push the just-clicked row behind/below the dock —
  // then re-clicking it to close becomes impossible. Keep the selected row scrolled flush into the
  // visible list area (just above the dock) so toggle-to-close is always one click. Computed via
  // getBoundingClientRect + scrollTop (never scrollIntoView, which can disrupt the app); the scale
  // factor keeps it correct under any ancestor transform, and the measured sticky chrome clears
  // whatever is pinned at the top of the list here.
  const keepSelectedInView = useCallback(() => {
    const list = listRef.current;
    if (!list) return;
    const row = list.querySelector('.nd-if-row.selected');
    if (!row) return;
    const lr = list.getBoundingClientRect();
    const rr = row.getBoundingClientRect();
    const scale = (list.offsetHeight ? lr.height / list.offsetHeight : 1) || 1;
    // Already in the same coordinate space as `lr`/`rr`, so it is NOT scaled again.
    const headH = stickyChromeHeight(list);
    if (rr.bottom > lr.bottom) list.scrollTop += (rr.bottom - lr.bottom) / scale;
    else if (rr.top < lr.top + headH) list.scrollTop -= (lr.top + headH - rr.top) / scale;
  }, []);

  // Apply it on selection change AND whenever the list resizes. The dock grows after its charts
  // finish loading (and on a pane resize / narrow-pane chart stacking), which shrinks the list and
  // would re-cover a row that was only scrolled clear of the shorter, still-loading dock. The
  // ResizeObserver re-applies the keep-in-view against the dock's final height — without it the
  // selected row ends up hidden behind the dock (only partly scrolled into view).
  useLayoutEffect(() => {
    if (selected == null) return;
    keepSelectedInView();
  }, [selected, keepSelectedInView]);

  useEffect(() => {
    if (selected == null) return;
    const list = listRef.current;
    if (!list || typeof ResizeObserver === 'undefined') return;
    const ro = new ResizeObserver(() => {
      // ⚠️ Not during a drag. A drag resizes the list every frame, and the selected row is sitting
      // exactly on the boundary being dragged (the branch below leaves it flush with `lr.bottom`),
      // so this would scroll the list under the operator's cursor one frame at a time — and in one
      // direction only, because the shrink branch guards the top edge alone. Dragging back would
      // not restore where they were. Applied once on pointer-up instead.
      if (drag.current) return;
      keepSelectedInView();
    });
    ro.observe(list);
    return () => ro.disconnect();
  }, [selected, keepSelectedInView]);

  const onResizeDown = useCallback(
    (e: React.PointerEvent) => {
      (e.target as Element).setPointerCapture?.(e.pointerId);
      e.preventDefault(); // no text selection across the list while dragging
      drag.current = { y: e.clientY, h: dockHeight };
      pointerY.current = e.clientY;
      setLiveHeight(dockHeight);
    },
    [dockHeight],
  );

  const onResizeMove = useCallback(
    (e: React.PointerEvent) => {
      if (!drag.current) return;
      pointerY.current = e.clientY;
      // Coalesce to one update per frame: pointermove can outrun paint, and each update resizes two
      // uPlot instances. Same shape as `dashboard/useResizeHandle.ts`.
      cancelAnimationFrame(raf.current);
      raf.current = requestAnimationFrame(() => {
        const d = drag.current;
        if (!d) return;
        setLiveHeight(heightFromDrag(d.h, d.y, pointerY.current, budget));
      });
    },
    [budget],
  );

  const endResize = useCallback(
    (e: React.PointerEvent) => {
      const d = drag.current;
      if (!d) return;
      (e.target as Element).releasePointerCapture?.(e.pointerId);
      cancelAnimationFrame(raf.current);
      const final = heightFromDrag(d.h, d.y, pointerY.current, budget);
      drag.current = null;
      setLiveHeight(null);
      // One store write — and therefore one PUT and one audit row — per gesture.
      setInterfaceDockHeight(final);
      keepSelectedInView();
    },
    [budget, keepSelectedInView],
  );

  const onResizeKey = useCallback(
    (e: React.KeyboardEvent) => {
      // Keyboard-operable, like every other primary control (ui-conventions.md). ⚠️ ArrowUp grows
      // here — the handle sits above the dock — which is the opposite of the Geo map's handle.
      const next = heightFromKey(dockHeight, e.key, budget);
      if (next == null) return;
      e.preventDefault();
      setInterfaceDockHeight(next);
    },
    [dockHeight, budget],
  );

  // Bundled so the dock takes one prop rather than seven, and so `null` says "no resize here"
  // in one place. Null on mobile: the dock is content-sized there and its charts keep a fixed
  // height — see the `data-viewport` block in NodeDetail.css for why.
  const resize: DockResize | null = mobile
    ? null
    : {
        height: dockHeight,
        min: DOCK_MIN_PX,
        max: Math.max(DOCK_MIN_PX, budget - LIST_MIN_PX),
        onReset: () => setInterfaceDockHeight(defaultDockHeight(budget)),
        handleProps: {
          onPointerDown: onResizeDown,
          onPointerMove: onResizeMove,
          onPointerUp: endResize,
          onPointerCancel: endResize,
          onKeyDown: onResizeKey,
        },
      };

  if (loaded && rows.length === 0) {
    return (
      <div className="nd-tabpad">
        {error && <p className="form-error">{error}</p>}
        <p className="nd-muted">{t('interfaces.emptyDiscovered')}</p>
      </div>
    );
  }

  return (
    <div className="nd-if" ref={setRootEl}>
      <div className="nd-if-toolbar">
        <span className="nd-if-summary">
          <b>{up}</b> {t('interfaces.ofUp', { total: rows.length })}
          <span className="nd-if-summary-hint">{t('interfaces.sparklineHint')}</span>
        </span>
        <FilterButton columns={columns} filters={filters} onOpen={() => setSheet(true)} />
        <ClearFilters
          columns={columns}
          filters={filters}
          onClear={() => setFilters(defaultFilters(columns))}
        />
      </div>
      {sheet && (
        <MobileFilterSheet
          columns={columns}
          labels={labels}
          filters={filters}
          onChange={setFilters}
          counts={counts}
          onClose={() => setSheet(false)}
        />
      )}
      {error && <p className="form-error nd-tabpad">{error}</p>}

      <div className="nd-if-list" ref={listRef}>
        <div className="nd-if-head">
          <div className="nd-if-h">{t('interfaces.colInterface')}</div>
          <div className="nd-if-h">{t('interfaces.colDescription')}</div>
          <div className="nd-if-h" title={t('interfaces.colOperTitle')}>
            {t('interfaces.colOper')}
          </div>
          <div className="nd-if-h">{t('interfaces.colThroughput')}</div>
          <div className="nd-if-h right">{t('interfaces.colInOut')}</div>
        </div>
        {/* A real filter row: same grid rule as `.nd-if-head` and `.nd-if-row` (one CSS
            declaration, three selectors — the same discipline `DataTable`'s shared template
            const enforces in TS). The two throughput columns are pictures, not values, so they
            carry an empty cell rather than a control that could not mean anything.

            Not drawn on a phone — `FilterButton` above is its other half, and since Inc.10 that
            gate is `ColumnFilterRow`'s own rather than a flag read here. It shipped without any
            gate and was the worst-looking of the four: the row is `position: sticky` at
            `top: 32px`, the height of the header, and mobile hides the header — so it pinned itself
            halfway down the scroller and the interface rows ran through the gap above it. */}
        <ColumnFilterRow
          columns={columns}
          slots={['if_name', 'if_alias', 'oper', null, null]}
          filters={filters}
          onChange={setFilters}
          counts={counts}
          labels={labels}
          className="nd-if-filters"
        />
        {shown.map((r) => {
          const down = r.oper_status != null && r.oper_status !== 1;
          return (
            <button
              type="button"
              key={r.ifindex}
              className={`nd-if-row${r.ifindex === selected ? ' selected' : ''}${down ? ' down' : ''}${
                r.stale ? ' stale' : ''
              }`}
              onClick={() => setSelected((cur) => (cur === r.ifindex ? null : r.ifindex))}
            >
              <span className="nd-if-id">
                <span className="nd-if-name mono">{r.if_name ?? `if${r.ifindex}`}</span>
              </span>
              <span className="nd-if-cell nd-if-desc">{r.if_alias || '—'}</span>
              <span className="nd-if-oper">
                <StatusDot state={operState(r.oper_status ?? null)} withLabel={false} />
                {operLabel(r.oper_status ?? null, t)}
              </span>
              <span className="nd-if-spark">
                <Sparkline nodeId={nodeId} ifindex={r.ifindex} down={down} />
              </span>
              <span className="nd-if-cell right">
                {r.oper_status === 1
                  ? `${formatBps(r.in_bps ?? null)} / ${formatBps(r.out_bps ?? null)}`
                  : t('interfaces.operDown')}
              </span>
            </button>
          );
        })}
        {shown.length === 0 && (
          <p className="nd-muted nd-tabpad">
            {/* The wording no longer quotes the term: with three columns filterable, naming one
                of them would point at the wrong control as often as the right one. */}
            {isAnyFiltered(columns, filters)
              ? t('common:filter.noMatch')
              : t('interfaces.emptyDiscovered')}
          </p>
        )}
      </div>

      {selectedRow ? (
        <InterfaceDock
          nodeId={nodeId}
          row={selectedRow}
          onClose={() => setSelected(null)}
          resize={resize}
        />
      ) : (
        <div className="nd-if-dockhint">{t('interfaces.dockHint')}</div>
      )}
    </div>
  );
}

/** In-row throughput sparkline: a cheap last-1h in-bps trend mark (area + line, no axes). Down
 *  interfaces show a dashed flat baseline and never fetch. Series load lazily once the row scrolls
 *  into view, so a 48-port switch doesn't fire a request per row up front. */
function Sparkline({
  nodeId,
  ifindex,
  down,
}: {
  nodeId: string;
  ifindex: number;
  down: boolean;
}) {
  const ref = useRef<HTMLSpanElement>(null);
  const [inView, setInView] = useState(false);
  const [values, setValues] = useState<number[] | null>(null);

  // Fetch only when visible. IntersectionObserver may be absent (tests/SSR) — fall back to eager.
  useEffect(() => {
    if (down) return;
    const el = ref.current;
    if (!el || typeof IntersectionObserver === 'undefined') {
      setInView(true);
      return;
    }
    const io = new IntersectionObserver(
      (entries) => {
        if (entries.some((e) => e.isIntersecting)) {
          setInView(true);
          io.disconnect();
        }
      },
      { rootMargin: '120px' },
    );
    io.observe(el);
    return () => io.disconnect();
  }, [down]);

  useEffect(() => {
    if (down || !inView) return;
    let cancelled = false;
    const to = Math.floor(Date.now() / 1000);
    api
      .getInterfaceSeries(nodeId, ifindex, {
        from: to - SPARK_WINDOW_SECS,
        to,
        step: SPARK_STEP_SECS,
      })
      .then((s) => {
        if (cancelled) return;
        setValues(s.in_bps.filter((v): v is number => v != null));
      })
      .catch(() => undefined);
    return () => {
      cancelled = true;
    };
  }, [nodeId, ifindex, inView, down]);

  if (down) {
    return (
      <span className="nd-if-spark-wrap" ref={ref}>
        <svg className="nd-if-spark-svg" viewBox={`0 0 ${SPARK_W} ${SPARK_H}`} preserveAspectRatio="none">
          <line
            x1="2"
            y1={SPARK_H / 2}
            x2={SPARK_W - 2}
            y2={SPARK_H / 2}
            stroke="var(--text-tertiary)"
            strokeWidth="1"
            strokeDasharray="4 3"
            opacity="0.5"
          />
        </svg>
      </span>
    );
  }

  const path = values ? sparklinePath(values, SPARK_W, SPARK_H) : null;
  return (
    <span className="nd-if-spark-wrap" ref={ref}>
      {path ? (
        <svg className="nd-if-spark-svg" viewBox={`0 0 ${SPARK_W} ${SPARK_H}`} preserveAspectRatio="none">
          {/* var() resolves in CSS (inline style), not in an SVG presentation attribute. */}
          <path d={path.area} style={{ fill: SERIES_IN }} opacity="0.12" />
          <path d={path.line} style={{ fill: 'none', stroke: SERIES_IN }} strokeWidth="1.4" />
        </svg>
      ) : (
        <span className="nd-if-spark-empty" />
      )}
    </span>
  );
}

/** Bottom detail dock for the selected interface: throughput (In/Out bps) + errors (In/Out per
 *  second) charts over a selectable window, plus inline In/Out/Err stat tiles. Fetches the series on
 *  a 15s interval (like the rest of the live detail); the range persists while switching rows. */
function InterfaceDock({
  nodeId,
  row,
  onClose,
  resize,
}: {
  nodeId: string;
  row: InterfaceRow;
  onClose: () => void;
  /** Desktop only; `null` on mobile, where the dock is content-sized (see NodeDetail.css). */
  resize: DockResize | null;
}) {
  const { t } = useTranslation('nodes');
  const range = useRangeStore((s) => s.range);
  const setRange = useRangeStore((s) => s.setRange);
  const throughputScale = usePrefsStore((s) => s.throughputScale);
  const toggleThroughputScale = usePrefsStore((s) => s.toggleThroughputScale);
  const rateUnit = usePrefsStore((s) => s.rateUnit);
  const toggleRateUnit = usePrefsStore((s) => s.toggleRateUnit);
  const isPps = rateUnit === 'pps';
  // How the charts are sized, as ONE object used by both so the pair cannot drift.
  //
  // Desktop: `fill`, tracking the dock's height via MetricChart's own ResizeObserver. That is not a
  // stylistic choice — `height` is part of MetricChart's rebuild key, so a fixed height would
  // destroy and reconstruct both uPlot instances on every frame of a drag, while `fill` just calls
  // `plot.setSize()`. ⚠️ Never pass `height` alongside `fill`: it stays in the effect deps, so a
  // varying one silently brings the rebuild back.
  //
  // Mobile: a fixed height, because there is no drag handle there and the dock is content-sized.
  const chartSizing = resize ? ({ fill: true } as const) : ({ height: 164 } as const);
  const tick = useRefreshTick();
  const [series, setSeries] = useState<InterfaceSeries | null>(null);
  const [win, setWin] = useState<[number, number] | null>(null);

  // Blank the charts when a different interface / range is selected (not on a tick refresh, which
  // would flash the charts empty every 15s).
  useEffect(() => {
    setSeries(null);
  }, [nodeId, row.ifindex, range]);

  // Fetch the series on selection/range change and on every shared refresh tick (S24 — the whole
  // node detail shares one clock instead of a per-card setInterval).
  useEffect(() => {
    let cancelled = false;
    const { from, to } = resolveRange(range);
    api
      .getInterfaceSeries(nodeId, row.ifindex, { from, to })
      .then((s) => {
        if (cancelled) return;
        setSeries(s);
        setWin([from, to]);
      })
      .catch(() => undefined);
    return () => {
      cancelled = true;
    };
  }, [nodeId, row.ifindex, range, tick]);

  const ts = series?.timestamps ?? [];
  const hasData = ts.length > 0;
  const errRate = latestErrorRate(series);
  const discRate = latestDiscardRate(series);
  // Configured-bandwidth overlay for the throughput chart (red line + optional capacity Y-range).
  // ⚠️ Memoized because it returns a FRESH `yRange` array every call, and MetricChart compares that
  // prop by reference (its own doc asks for a stable one). Harmless while the dock only re-rendered
  // on a 15s tick; during a drag it would be a `setData` — a scale reset plus a redraw — per frame.
  const bw = useMemo(
    () => throughputBandwidthOverlay(row.if_speed_bps, throughputScale, rateUnit),
    [row.if_speed_bps, throughputScale, rateUnit],
  );
  // `false` in pps mode by construction (the overlay returns nothing there), which is what removes
  // the bandwidth line, the fit/capacity toggle and the legend's bandwidth entry in one move —
  // rather than three call sites each remembering the unit.
  const hasBandwidth = bw.referenceLine != null;
  // Same reason: a fresh array each render is a fresh `series` prop.
  const throughputSeries = useMemo(() => {
    if (!series) return [];
    const [inArr, outArr] = throughputPair(series, rateUnit);
    return [
      { label: t('interfaces.in'), values: inArr, color: SERIES_IN },
      { label: t('interfaces.out'), values: outArr, color: SERIES_OUT },
    ];
  }, [series, rateUnit, t]);
  const errorSeries = useMemo(
    () =>
      series
        ? [
            { label: t('interfaces.in'), values: series.in_errors, color: SERIES_IN },
            { label: t('interfaces.out'), values: series.out_errors, color: SERIES_OUT },
          ]
        : [],
    [series, t],
  );
  // Same In/Out colours as the other two charts on purpose: all three read "colour 1 = in,
  // colour 2 = out", so `ChartLegend` is reused unchanged and no new series colour is introduced.
  const discardSeries = useMemo(
    () =>
      series
        ? [
            { label: t('interfaces.in'), values: series.in_discards, color: SERIES_IN },
            { label: t('interfaces.out'), values: series.out_discards, color: SERIES_OUT },
          ]
        : [],
    [series, t],
  );

  return (
    <div className="nd-if-dock" style={resize ? { height: `${resize.height}px` } : undefined}>
      {resize && (
        /* The dock's top edge, as a real control: focusable, announced, and arrow-key operable.
           How much of the screen the charts get is an operator decision (issue #65), not chrome.
           `role="slider"` with explicit bounds — the Geo map's equivalent omits valuemin/valuemax
           and should not be copied on that point. */
        <div
          className="nd-if-resize"
          role="slider"
          tabIndex={0}
          aria-label={t('interfaces.resizeDock')}
          aria-orientation="vertical"
          aria-valuenow={resize.height}
          aria-valuemin={resize.min}
          aria-valuemax={resize.max}
          title={t('interfaces.resizeDock')}
          onDoubleClick={resize.onReset}
          {...resize.handleProps}
        >
          <span className="nd-if-resize-grip" aria-hidden="true" />
        </div>
      )}
      <div className="nd-if-dock-head">
        <StatusDot state={operState(row.oper_status ?? null)} withLabel={false} />
        <span className="mono nd-if-dock-name">{row.if_name ?? `if${row.ifindex}`}</span>
        {row.if_alias && <span className="nd-muted nd-if-dock-alias">{row.if_alias}</span>}
        <div className="nd-if-dock-ctl">
          <span className="nd-if-dock-stats">
            <span>
              <span className="nd-muted">{t('interfaces.in')}</span> {formatBps(row.in_bps ?? null)}
            </span>
            <span>
              <span className="nd-muted">{t('interfaces.out')}</span> {formatBps(row.out_bps ?? null)}
            </span>
            {errRate != null && errRate > 0 && (
              <span>
                <span className="nd-muted">{t('interfaces.err')}</span>{' '}
                <span className="nd-if-dock-err">{formatPps(errRate)}</span>
              </span>
            )}
            {/* Shown only when non-zero, like the error tile: a permanent `Disc 0.0/s` on every
                healthy interface is noise, and the chart below already says "zero" for free. */}
            {discRate != null && discRate > 0 && (
              <span>
                <span className="nd-muted">{t('interfaces.disc')}</span>{' '}
                <span className="nd-if-dock-err">{formatPps(discRate)}</span>
              </span>
            )}
          </span>
          <RangeControl value={range} onChange={setRange} />
        </div>
        <button
          type="button"
          className="nd-if-dock-close"
          aria-label={t('interfaces.closeDetail')}
          title={t('interfaces.closeDetailTitle')}
          onClick={onClose}
        >
          <svg viewBox="0 0 14 14" fill="none" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round">
            <path d="M3 3l8 8M11 3l-8 8" />
          </svg>
        </button>
      </div>

      <div className="nd-if-dock-charts">
        <div className="nd-if-chart">
          <div className="nd-if-chart-t">
            <span>
              {t('interfaces.throughput')}{' '}
              <span className="nd-unit">
                {isPps ? t('interfaces.throughputUnitPps') : t('interfaces.throughputUnit')}
              </span>
            </span>
            <span className="nd-if-chart-ctl">
              {/* Only this chart carries a unit toggle. Errors and discards below are packets/sec
                  with no other form (IF-MIB counts errored and discarded frames, never their
                  octets), so a dock-level control would promise more than it can do — ADR-060. */}
              <button
                type="button"
                className="nd-if-scale-toggle"
                onClick={toggleRateUnit}
                title={
                  isPps
                    ? t('interfaces.rateUnitToggleTitlePps')
                    : t('interfaces.rateUnitToggleTitleBps')
                }
              >
                {isPps ? t('interfaces.rateUnitPps') : t('interfaces.rateUnitBps')}
              </button>
              {hasBandwidth && (
                <button
                  type="button"
                  className="nd-if-scale-toggle"
                  onClick={toggleThroughputScale}
                  title={
                    throughputScale === 'capacity'
                      ? t('interfaces.scaleToggleTitleCapacity')
                      : t('interfaces.scaleToggleTitleAuto')
                  }
                >
                  {throughputScale === 'capacity'
                    ? t('interfaces.bandwidth')
                    : t('interfaces.auto')}
                </button>
              )}
              <ChartLegend bandwidth={hasBandwidth} />
            </span>
          </div>
          {hasData ? (
            <MetricChart
              title=""
              {...chartSizing}
              timestamps={ts}
              yFormat={formatSi}
              legendFormat={isPps ? formatPps : formatBps}
              yRange={bw.yRange}
              xRange={win ?? undefined}
              referenceLine={bw.referenceLine}
              series={throughputSeries}
            />
          ) : (
            <div className="nd-if-chart-empty">{t('interfaces.noData')}</div>
          )}
        </div>

        <div className="nd-if-chart">
          <div className="nd-if-chart-t">
            <span>
              {t('interfaces.errors')}{' '}
              <span className="nd-unit">{t('interfaces.errorsUnit')}</span>
            </span>
            <ChartLegend />
          </div>
          {hasData ? (
            <MetricChart
              title=""
              {...chartSizing}
              timestamps={ts}
              yFormat={formatSi}
              legendFormat={formatPps}
              xRange={win ?? undefined}
              series={errorSeries}
            />
          ) : (
            <div className="nd-if-chart-empty">{t('interfaces.noData')}</div>
          )}
        </div>

        {/* Discards get their own chart rather than extra series on the errors one (ADR-046
            Inc.4): the two have different causes — damage vs congestion — and in practice differ
            by two or three orders of magnitude, so sharing an axis flattens whichever is smaller.
            No CSS change is needed; `.nd-if-dock-charts` is an auto-fit grid. */}
        <div className="nd-if-chart">
          <div className="nd-if-chart-t">
            <span>
              {t('interfaces.discards')}{' '}
              <span className="nd-unit">{t('interfaces.discardsUnit')}</span>
            </span>
            <ChartLegend />
          </div>
          {hasData ? (
            <MetricChart
              title=""
              {...chartSizing}
              timestamps={ts}
              yFormat={formatSi}
              legendFormat={formatPps}
              xRange={win ?? undefined}
              series={discardSeries}
            />
          ) : (
            <div className="nd-if-chart-empty">{t('interfaces.noData')}</div>
          )}
        </div>
      </div>
    </div>
  );
}

/** In/Out colour key shown beside a dock chart title (matches the chart series colours). With
 *  `bandwidth`, also shows the red configured-bandwidth reference-line key. */
function ChartLegend({ bandwidth = false }: { bandwidth?: boolean }) {
  const { t } = useTranslation('nodes');
  return (
    <span className="nd-if-legend">
      <span className="nd-if-legend-k">
        <span className="nd-if-legend-sw" style={{ background: SERIES_IN }} />
        {t('interfaces.in')}
      </span>
      <span className="nd-if-legend-k">
        <span className="nd-if-legend-sw" style={{ background: SERIES_OUT }} />
        {t('interfaces.out')}
      </span>
      {bandwidth && (
        <span className="nd-if-legend-k">
          <span className="nd-if-legend-sw nd-if-legend-bw" />
          {t('interfaces.bandwidth')}
        </span>
      )}
    </span>
  );
}
