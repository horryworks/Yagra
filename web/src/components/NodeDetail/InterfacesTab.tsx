// Interfaces tab of the unified node detail — Direction C: keep the interface LIST and the selected
// interface's CHARTS on screen together. Each row carries a small last-1h throughput sparkline so
// trends across all interfaces are visible without selecting anything (fast triage of "which port is
// busy / flapping"); selecting a row opens a pinned detail dock at the bottom (throughput + error
// charts, range control, stat tiles). The list scrolls above the dock; the dock stays put. The tab
// body is a flex column — toolbar (none) → list (flex:1, scrolls) → dock/hint (none). The list
// refreshes on an interval (shared with the tab badge); per-interface series are loaded lazily.

import { useCallback, useEffect, useLayoutEffect, useRef, useState } from 'react';
import { useTranslation } from 'react-i18next';
import type { TFunction } from 'i18next';
import { api } from '../../services/api';
import { formatBps, formatSi } from '../../lib/format';
import type { InterfaceRow, InterfaceSeries } from '../../types/api';
import { StatusDot } from '../ui/StatusDot';
import { TextInput } from '../ui/Field';
import { MetricChart, SERIES_IN, SERIES_OUT } from '../MetricChart/MetricChart';
import { operState } from './OverviewTab';
import { RangeControl, resolveRange } from './RangeControl';
import { useRangeStore } from '../../store';
import { usePrefsStore } from '../../prefs';
import { latestErrorRate, sparklinePath, throughputBandwidthOverlay } from './interfaceMetrics';

const STATUS_REFRESH_MS = 15_000;
// In-row sparkline window: last hour at a coarse step (cheap; trend, not precision).
const SPARK_WINDOW_SECS = 3600;
const SPARK_STEP_SECS = 120;
const SPARK_W = 120;
const SPARK_H = 26;
// Sticky list-header height, mirrored in CSS — used to keep the selected row clear of it.
const LIST_HEAD_H = 32;

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

export function InterfacesTab({ nodeId, rows, loaded, error }: Props) {
  const { t } = useTranslation('nodes');
  const [filter, setFilter] = useState('');
  const [selected, setSelected] = useState<number | null>(null);
  const listRef = useRef<HTMLDivElement>(null);

  const q = filter.trim().toLowerCase();
  const shown = q
    ? rows.filter(
        (r) =>
          (r.if_name ?? `if${r.ifindex}`).toLowerCase().includes(q) ||
          (r.if_alias ?? '').toLowerCase().includes(q),
      )
    : rows;
  const up = rows.filter((r) => r.oper_status === 1).length;
  const selectedRow = rows.find((r) => r.ifindex === selected) ?? null;

  // Opening the dock shrinks the list, which can push the just-clicked row behind/below the dock —
  // then re-clicking it to close becomes impossible. Keep the selected row scrolled flush into the
  // visible list area (just above the dock) so toggle-to-close is always one click. Computed via
  // getBoundingClientRect + scrollTop (never scrollIntoView, which can disrupt the app); the scale
  // factor keeps it correct under any ancestor transform, and LIST_HEAD_H clears the sticky header.
  const keepSelectedInView = useCallback(() => {
    const list = listRef.current;
    if (!list) return;
    const row = list.querySelector('.nd-if-row.selected');
    if (!row) return;
    const lr = list.getBoundingClientRect();
    const rr = row.getBoundingClientRect();
    const scale = (list.offsetHeight ? lr.height / list.offsetHeight : 1) || 1;
    const headH = LIST_HEAD_H * scale;
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
    const ro = new ResizeObserver(() => keepSelectedInView());
    ro.observe(list);
    return () => ro.disconnect();
  }, [selected, keepSelectedInView]);

  if (loaded && rows.length === 0) {
    return (
      <div className="nd-tabpad">
        {error && <p className="form-error">{error}</p>}
        <p className="nd-muted">{t('interfaces.emptyDiscovered')}</p>
      </div>
    );
  }

  return (
    <div className="nd-if">
      <div className="nd-if-toolbar">
        <span className="nd-if-summary">
          <b>{up}</b> {t('interfaces.ofUp', { total: rows.length })}
          <span className="nd-if-summary-hint">{t('interfaces.sparklineHint')}</span>
        </span>
        <TextInput
          className="nd-if-filter"
          value={filter}
          onChange={(e) => setFilter(e.target.value)}
          placeholder={t('interfaces.filterPlaceholder')}
        />
      </div>
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
                <StatusDot state={operState(r.oper_status)} withLabel={false} />
                {operLabel(r.oper_status, t)}
              </span>
              <span className="nd-if-spark">
                <Sparkline nodeId={nodeId} ifindex={r.ifindex} down={down} />
              </span>
              <span className="nd-if-cell right">
                {r.oper_status === 1
                  ? `${formatBps(r.in_bps)} / ${formatBps(r.out_bps)}`
                  : t('interfaces.operDown')}
              </span>
            </button>
          );
        })}
        {shown.length === 0 && (
          <p className="nd-muted nd-tabpad">{t('interfaces.noMatch', { filter })}</p>
        )}
      </div>

      {selectedRow ? (
        <InterfaceDock nodeId={nodeId} row={selectedRow} onClose={() => setSelected(null)} />
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
          <path d={path.area} fill={SERIES_IN} opacity="0.12" />
          <path d={path.line} fill="none" stroke={SERIES_IN} strokeWidth="1.4" />
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
}: {
  nodeId: string;
  row: InterfaceRow;
  onClose: () => void;
}) {
  const { t } = useTranslation('nodes');
  const range = useRangeStore((s) => s.range);
  const setRange = useRangeStore((s) => s.setRange);
  const throughputScale = usePrefsStore((s) => s.throughputScale);
  const toggleThroughputScale = usePrefsStore((s) => s.toggleThroughputScale);
  const [series, setSeries] = useState<InterfaceSeries | null>(null);
  const [win, setWin] = useState<[number, number] | null>(null);

  useEffect(() => {
    let cancelled = false;
    const load = () => {
      const { from, to } = resolveRange(range);
      api
        .getInterfaceSeries(nodeId, row.ifindex, { from, to })
        .then((s) => {
          if (cancelled) return;
          setSeries(s);
          setWin([from, to]);
        })
        .catch(() => undefined);
    };
    setSeries(null);
    load();
    const id = setInterval(load, STATUS_REFRESH_MS);
    return () => {
      cancelled = true;
      clearInterval(id);
    };
  }, [nodeId, row.ifindex, range]);

  const ts = series?.timestamps ?? [];
  const hasData = ts.length > 0;
  const errRate = latestErrorRate(series);
  // Configured-bandwidth overlay for the throughput chart (red line + optional capacity Y-range).
  const bw = throughputBandwidthOverlay(row.if_speed_bps, throughputScale);
  const hasBandwidth = bw.referenceLine != null;

  return (
    <div className="nd-if-dock">
      <div className="nd-if-dock-head">
        <StatusDot state={operState(row.oper_status)} withLabel={false} />
        <span className="mono nd-if-dock-name">{row.if_name ?? `if${row.ifindex}`}</span>
        {row.if_alias && <span className="nd-muted nd-if-dock-alias">{row.if_alias}</span>}
        <div className="nd-if-dock-ctl">
          <span className="nd-if-dock-stats">
            <span>
              <span className="nd-muted">{t('interfaces.in')}</span> {formatBps(row.in_bps)}
            </span>
            <span>
              <span className="nd-muted">{t('interfaces.out')}</span> {formatBps(row.out_bps)}
            </span>
            {errRate != null && errRate > 0 && (
              <span>
                <span className="nd-muted">{t('interfaces.err')}</span>{' '}
                <span className="nd-if-dock-err">{errRate.toFixed(1)}/s</span>
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
              <span className="nd-unit">{t('interfaces.throughputUnit')}</span>
            </span>
            <span className="nd-if-chart-ctl">
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
              height={132}
              timestamps={ts}
              yFormat={formatSi}
              legendFormat={formatBps}
              yRange={bw.yRange}
              xRange={win ?? undefined}
              referenceLine={bw.referenceLine}
              series={[
                { label: t('interfaces.in'), values: series!.in_bps, color: SERIES_IN },
                { label: t('interfaces.out'), values: series!.out_bps, color: SERIES_OUT },
              ]}
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
              height={132}
              timestamps={ts}
              yFormat={formatSi}
              legendFormat={(v) => `${formatSi(v)}/s`}
              xRange={win ?? undefined}
              series={[
                { label: t('interfaces.in'), values: series!.in_errors, color: SERIES_IN },
                { label: t('interfaces.out'), values: series!.out_errors, color: SERIES_OUT },
              ]}
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
