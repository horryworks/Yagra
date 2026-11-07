// SPDX-License-Identifier: AGPL-3.0-only
// The node Overview's availability graph (ADR-223): one bar per half hour, four hours or day, each
// stacked by how much of it the node was up, in maintenance, down and not recorded. Down is the
// liveness alert being open, as the server decides. Every judgement — the bars, their shares, the
// headline, the keys — is in `availability.ts`; this file draws.
//
// The strip is one control rather than one button per bar: ninety three-pixel buttons are neither
// a touch target nor a sensible Tab order. A press or hover anywhere picks the bar under it, the
// arrow keys move along, and the line under the strip says what the picked bar was.

import { useEffect, useMemo, useRef, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { api } from '../../services/api';
import type { NodeAvailability } from '../../types/api';
import { useRefreshTick } from '../../lib/refreshTick';
import { dateOnly, formatClockTime, formatDuration } from '../../lib/format';
import { Stack } from '../ui/Page';
import { SectionHead } from '../ui/Section';
import { Segmented } from '../ui/Segmented';
import {
  AVAILABILITY_PERIODS,
  availabilityBars,
  barIndexAt,
  barSegments,
  barsAreDays,
  DEFAULT_AVAILABILITY_PERIOD,
  formatAvailabilityPct,
  nextBarIndex,
  type AvailabilityBar,
  type AvailabilityPeriod,
} from './availability';
import './AvailabilitySection.css';

const SEGMENT_CLASS = {
  up: 'nd-av-up',
  maintenance: 'nd-av-maint',
  down: 'nd-av-down',
  no_record: 'nd-av-none',
} as const;

export function AvailabilitySection({ nodeId }: { nodeId: string }) {
  const { t } = useTranslation('nodes');
  const tick = useRefreshTick();
  const [period, setPeriod] = useState<AvailabilityPeriod>(DEFAULT_AVAILABILITY_PERIOD);
  const [data, setData] = useState<NodeAvailability | null>(null);
  const [failed, setFailed] = useState(false);
  const [picked, setPicked] = useState<number | null>(null);
  const [hovered, setHovered] = useState<number | null>(null);
  const strip = useRef<HTMLDivElement>(null);

  useEffect(() => {
    let cancelled = false;
    api
      .getNodeAvailability(nodeId, period)
      .then((a) => {
        if (cancelled) return;
        setData(a);
        setFailed(false);
      })
      .catch(() => !cancelled && setFailed(true));
    return () => {
      cancelled = true;
    };
  }, [nodeId, period, tick]);

  // A different period is a different set of bars; a picked index would point at another time.
  useEffect(() => setPicked(null), [period, nodeId]);

  const bars = useMemo(
    () => (data && data.period === period ? availabilityBars(data, period) : []),
    [data, period],
  );

  const rangeText = (b: AvailabilityBar) =>
    barsAreDays(period)
      ? dateOnly(new Date(b.fromMs).toISOString())
      : `${dateOnly(new Date(b.fromMs).toISOString())} ${formatClockTime(b.fromMs)}–${formatClockTime(b.toMs)}`;
  const barText = (b: AvailabilityBar) => {
    const parts: string[] = [];
    if (b.downSecs > 0) {
      parts.push(
        t('overview.availabilityGraph.barDown', {
          count: b.downCount,
          duration: formatDuration(b.downSecs, { smallest: 'm' }),
        }),
      );
    }
    if (b.maintenanceSecs > 0) {
      parts.push(
        t('overview.availabilityGraph.barMaintenance', {
          duration: formatDuration(b.maintenanceSecs, { smallest: 'm' }),
        }),
      );
    }
    if (b.noRecordSecs > 0 && b.noRecordSecs >= b.elapsedSecs) {
      parts.push(t('overview.availabilityGraph.barNoRecord'));
    } else if (parts.length === 0) {
      parts.push(t('overview.availabilityGraph.barUp'));
    }
    return `${rangeText(b)} · ${parts.join(' · ')}`;
  };

  const shown = hovered ?? picked;
  const shownBar = shown != null ? bars[shown] : undefined;
  const pct = data ? formatAvailabilityPct(data.availability_pct, data.down_secs) : null;
  const summary = !data
    ? ''
    : data.down_secs > 0
      ? t('overview.availabilityGraph.summaryDown', {
          count: data.outages,
          duration: formatDuration(data.down_secs, { smallest: 'm' }),
        })
      : t('overview.availabilityGraph.summaryClean');
  const line = shownBar ? barText(shownBar) : summary;

  const indexAt = (clientX: number) => {
    const el = strip.current;
    if (!el) return null;
    const r = el.getBoundingClientRect();
    return barIndexAt(clientX - r.left, r.width, bars.length);
  };

  return (
    <Stack as="section" gap={8} className="nd-av">
      <SectionHead
        title={t('overview.availabilityGraph.title')}
        actions={
          <Segmented
            size="sm"
            ariaLabel={t('overview.availabilityGraph.period')}
            options={AVAILABILITY_PERIODS.map((p) => ({ value: p, label: p }))}
            value={period}
            onChange={(v) => setPeriod(v as AvailabilityPeriod)}
          />
        }
      />
      {failed && !data ? (
        <p className="form-error">{t('overview.availabilityGraph.loadFailed')}</p>
      ) : (
        <>
          <div className="nd-av-head">
            <span className="nd-av-pct">{pct ?? '—'}</span>
            <span className="nd-av-line" aria-live="polite">
              {line}
            </span>
          </div>
          <div
            ref={strip}
            className="nd-av-bars"
            role="slider"
            tabIndex={0}
            aria-label={t('overview.availabilityGraph.bars')}
            aria-valuemin={1}
            aria-valuemax={Math.max(1, bars.length)}
            aria-valuenow={(picked ?? bars.length - 1) + 1}
            aria-valuetext={line}
            onPointerMove={(e) => setHovered(indexAt(e.clientX))}
            onPointerLeave={() => setHovered(null)}
            onPointerDown={(e) => setPicked(indexAt(e.clientX))}
            onKeyDown={(e) => {
              const next = nextBarIndex(e.key, picked, bars.length);
              if (next == null) return;
              e.preventDefault();
              // The key is the newer gesture: a pointer resting on another bar must not keep the
              // line on that one.
              setHovered(null);
              setPicked(next);
            }}
            onBlur={() => setPicked(null)}
          >
            {bars.map((b, i) => (
              <div
                key={b.fromMs}
                className={i === shown ? 'nd-av-bar on' : 'nd-av-bar'}
              >
                {barSegments(b).map((s) =>
                  s.kind === 'up' ? (
                    <i key={s.kind} className={SEGMENT_CLASS.up} />
                  ) : (
                    <i
                      key={s.kind}
                      className={SEGMENT_CLASS[s.kind]}
                      style={{ height: `${s.pct}%` }}
                    />
                  ),
                )}
              </div>
            ))}
          </div>
          <div className="nd-av-legend">
            <span>
              <i className="nd-av-up" />
              {t('overview.availabilityGraph.legend.up')}
            </span>
            <span>
              <i className="nd-av-down" />
              {t('overview.availabilityGraph.legend.down')}
            </span>
            <span>
              <i className="nd-av-maint" />
              {t('overview.availabilityGraph.legend.maintenance')}
            </span>
            <span>
              <i className="nd-av-none" />
              {t('overview.availabilityGraph.legend.noRecord')}
            </span>
          </div>
        </>
      )}
    </Stack>
  );
}
