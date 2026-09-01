// SPDX-License-Identifier: AGPL-3.0-only
// Link Saturation report body (cross-store: flow + TSDB).
//
// Entity: a **conversation** on a node. The finding is concentration — one src→dst pair accounting
// for most of a node's traffic — so the primary reading is a share meter (this conversation against
// the node's total), and the endpoints are shown as a pair rather than as a single label.
//
// When the backend could attach the node's live interface throughput (`detail.interface_bps`, read
// from the TSDB at analysis time), a Gauge shows the concentration as percent-of-capacity — the one
// place in these reports where "% of a known whole" is literally the semantic Gauge exists for.

import { useMemo, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { Card } from '../../../components/ui/Card';
import { RankedBars, type RankedRow } from '../../../dashboard/primitives/RankedBars';
import { formatBps, formatBytes, formatUtil } from '../../../lib/format';
import { Chips, EmptyList, FindingRow, MonoLine, NodeRef, ReportToolbar, RightRail } from '../kit';
import { detailStr, sevOf, shareBucket, sortByDetail, sortCommon } from '../format';
import {
  saturationConversationBytes as convBytes,
  saturationInterfaceBps as ifaceBps,
  saturationNodeBytes as nodeBytes,
  saturationRatio as ratioOf,
} from '../findingFacts';
import type { ReportBodyProps } from '../types';
import type { AnalysisFinding } from '../../../types/api';

function SaturationRow({ finding }: { finding: AnalysisFinding }) {
  const { t } = useTranslation('troubleshoot');
  const sev = sevOf(finding);
  const ratio = ratioOf(finding);
  const bps = ifaceBps(finding);
  const src = detailStr(finding, 'src');
  const dst = detailStr(finding, 'dst');
  return (
    <FindingRow
      finding={finding}
      identity={
        <>
          <NodeRef finding={finding} />
          {/* The endpoints are a pair, so they get their own line rather than being concatenated. */}
          <MonoLine title={`${src} → ${dst}`}>
            {src} → {dst}
          </MonoLine>
          <span className="ts-anom-kind">
            {t('report.saturation.ofNode', { bytes: formatBytes(nodeBytes(finding)) })}
          </span>
        </>
      }
      viz={
        <div className="tsr-share" title={`${formatBytes(convBytes(finding))} / ${formatBytes(nodeBytes(finding))}`}>
          <div className="tsr-meter">
            <div
              className={`tsr-meter-fill tsr-share-fill ${sev}`}
              style={{ width: `${Math.max(0, Math.min(100, ratio * 100))}%` }}
            />
          </div>
          <span className="tsr-share-val mono">{formatUtil(ratio * 100)}</span>
        </div>
      }
      right={
        <RightRail
          when={formatBytes(convBytes(finding))}
          // Only shown when the TSDB side actually resolved — never a fake "0 bps".
          detail={bps !== undefined && bps > 0 ? formatBps(bps) : undefined}
        />
      }
    />
  );
}

export function SaturationBody({ findings }: ReportBodyProps) {
  const { t } = useTranslation('troubleshoot');
  const [filter, setFilter] = useState<'all' | 'high' | 'mid'>('all');
  const [sort, setSort] = useState<'share' | 'bytes' | 'node'>('share');

  const top = useMemo<RankedRow[]>(
    () =>
      findings
        .slice()
        .sort((a, b) => ratioOf(b) - ratioOf(a))
        .slice(0, 10)
        .map((f, i) => ({
          label: `${i + 1}. ${f.node_name} · ${detailStr(f, 'src') ?? ''}→${detailStr(f, 'dst') ?? ''}`,
          value: ratioOf(f) * 100,
          valueText: formatUtil(ratioOf(f) * 100),
          color:
            sevOf(f) === 'crit'
              ? 'var(--status-critical)'
              : sevOf(f) === 'warn'
                ? 'var(--status-warning)'
                : 'var(--series-6)',
        })),
    [findings],
  );

  const list = useMemo(() => {
    const base = filter === 'all' ? findings : findings.filter((f) => shareBucket(ratioOf(f)) === filter);
    if (sort === 'share') return base.slice().sort((a, b) => ratioOf(b) - ratioOf(a));
    if (sort === 'bytes') return sortByDetail(base, 'conversation_bytes');
    return sortCommon(base, 'node');
  }, [findings, filter, sort]);

  return (
    <>
      {top.length > 0 && (
        <Card title={t('report.saturation.mostConcentrated')}>
          {/* max={100}: a share is a percentage of the node's own traffic, not of the top row. */}
          <RankedBars rows={top} max={100} />
        </Card>
      )}
      <ReportToolbar
        id="tsr-saturation"
        filters={
          <Chips
            value={filter}
            onChange={setFilter}
            options={[
              { value: 'all', label: t('report.common.filters.all') },
              { value: 'high', label: t('report.saturation.filters.high'), color: 'var(--status-critical)' },
              { value: 'mid', label: t('report.saturation.filters.mid'), color: 'var(--status-warning)' },
            ]}
          />
        }
        sort={sort}
        onSort={(v) => setSort(v as 'share' | 'bytes' | 'node')}
        sortOptions={[
          { value: 'share', label: t('report.saturation.sort.byShare') },
          { value: 'bytes', label: t('report.saturation.sort.byBytes') },
          { value: 'node', label: t('report.common.sort.byNode') },
        ]}
      />
      <div className="ts-anoms">
        {list.length ? (
          list.map((f) => <SaturationRow key={f.id} finding={f} />)
        ) : (
          <EmptyList total={findings.length} />
        )}
      </div>
    </>
  );
}
