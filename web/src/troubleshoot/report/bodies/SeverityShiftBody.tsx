// SPDX-License-Identifier: AGPL-3.0-only
// Severity Shift report body (passive monitoring, ADR-024).
//
// Entity: a node. The finding is a **change in composition**, not a level: the share of syslog at
// error-or-worse rose against the node's own baseline. So the axis everywhere is the delta in
// percentage points — `DeltaBars` at the top (signed magnitude is exactly what it is for; peak-
// relative normalization is right here because pp deltas have no natural ceiling), and a
// before→after twin track per row so the move itself is visible rather than two bare percentages.
//
// Volume is shown alongside, because +40 pp out of 12 events is noise and out of 4,000 is an incident.

import { useMemo, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { Card } from '../../../components/ui/Card';
import { DeltaBars, type DeltaRow } from '../../../dashboard/primitives/DeltaBars';
import { Chips, EmptyList, FindingRow, NodeRef, ReportToolbar, RightRail, TwinTrack } from '../kit';
import { detailNum, fmtCount, ppBucket, sevOf, sortCommon } from '../format';
import {
  shiftBaselineFrac as baseFrac,
  shiftDeltaPp as deltaPp,
  shiftRecentFrac as recentFrac,
  shiftVolume as volumeOf,
} from '../findingFacts';
import type { ReportBodyProps } from '../types';
import type { AnalysisFinding } from '../../../types/api';

function ShiftRow({ finding }: { finding: AnalysisFinding }) {
  const { t } = useTranslation('troubleshoot');
  const rp = recentFrac(finding) * 100;
  const bp = baseFrac(finding) * 100;
  const pp = deltaPp(finding);
  return (
    <FindingRow
      finding={finding}
      identity={
        <>
          <NodeRef finding={finding} />
          <span className="ts-anom-kind">
            {t('report.severity_shift.volume', {
              high: fmtCount(detailNum(finding, 'recent_high') ?? 0),
              total: fmtCount(volumeOf(finding)),
            })}
          </span>
        </>
      }
      viz={
        <TwinTrack
          basePct={bp}
          recentPct={rp}
          severity={sevOf(finding)}
          label={`${bp.toFixed(0)}% → ${rp.toFixed(0)}%`}
        />
      }
      right={
        <RightRail
          when={t('report.severity_shift.delta', { pp: pp.toFixed(0) })}
          detail={t('report.severity_shift.wasAt', { pct: bp.toFixed(0) })}
        />
      }
    />
  );
}

export function SeverityShiftBody({ findings }: ReportBodyProps) {
  const { t } = useTranslation('troubleshoot');
  const [filter, setFilter] = useState<'all' | 'big' | 'mid'>('all');
  const [sort, setSort] = useState<'delta' | 'recent' | 'volume' | 'node'>('delta');

  const top = useMemo<DeltaRow[]>(
    () =>
      findings
        .slice()
        .sort((a, b) => deltaPp(b) - deltaPp(a))
        .slice(0, 10)
        .map((f, i) => ({
          label: `${i + 1}. ${f.node_name}`,
          value: deltaPp(f),
          valueText: t('report.severity_shift.delta', { pp: deltaPp(f).toFixed(0) }),
        })),
    [findings, t],
  );

  const list = useMemo(() => {
    const base = filter === 'all' ? findings : findings.filter((f) => ppBucket(deltaPp(f)) === filter);
    if (sort === 'delta') return base.slice().sort((a, b) => deltaPp(b) - deltaPp(a));
    if (sort === 'recent') return base.slice().sort((a, b) => recentFrac(b) - recentFrac(a));
    if (sort === 'volume') return base.slice().sort((a, b) => volumeOf(b) - volumeOf(a));
    return sortCommon(base, 'node');
  }, [findings, filter, sort]);

  return (
    <>
      {top.length > 0 && (
        <Card title={t('report.severity_shift.biggest')}>
          <DeltaBars rows={top} />
        </Card>
      )}
      <ReportToolbar
        id="tsr-severity-shift"
        filters={
          <Chips
            value={filter}
            onChange={setFilter}
            options={[
              { value: 'all', label: t('report.common.filters.all') },
              { value: 'big', label: t('report.severity_shift.filters.big'), color: 'var(--status-critical)' },
              { value: 'mid', label: t('report.severity_shift.filters.mid'), color: 'var(--status-warning)' },
            ]}
          />
        }
        sort={sort}
        onSort={(v) => setSort(v as 'delta' | 'recent' | 'volume' | 'node')}
        sortOptions={[
          { value: 'delta', label: t('report.severity_shift.sort.byDelta') },
          { value: 'recent', label: t('report.severity_shift.sort.byRecent') },
          { value: 'volume', label: t('report.severity_shift.sort.byVolume') },
          { value: 'node', label: t('report.common.sort.byNode') },
        ]}
      />
      <div className="ts-anoms">
        {list.length ? (
          list.map((f) => <ShiftRow key={f.id} finding={f} />)
        ) : (
          <EmptyList total={findings.length} />
        )}
      </div>
    </>
  );
}
