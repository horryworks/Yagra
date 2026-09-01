// SPDX-License-Identifier: AGPL-3.0-only
// Event Flap report body (passive monitoring, ADR-024).
//
// Entity: a **(rule, node) pair** — the only report with a two-part entity, which is what shapes it:
//
//  1. A `Segmented` group-by toggle. "By finding" answers "which node is flapping"; "by rule" rolls
//     the same findings up across nodes and answers "which RULE is thrashing fleet-wide" — a
//     different, often more actionable question that a flat per-node list structurally cannot show.
//  2. A fire/clear balance bar per row. An imbalance means "raised and never cleared", a different
//     fault from clean up/down thrash, and a cycle count alone hides it.
//
// `detail.rule_id` is a grouping key only and is never rendered (no raw UUIDs); the rule *name*
// comes from `metric`, which the backend encodes as `event:{rule_name}`.

import { useMemo, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { Card } from '../../../components/ui/Card';
import { RankedBars, type RankedRow } from '../../../dashboard/primitives/RankedBars';
import { Segmented } from '../../../components/ui/Segmented';
import {
  BalanceBar,
  Chips,
  EmptyList,
  FindingRow,
  MonoLine,
  NodeRef,
  ReportToolbar,
  RightRail,
} from '../kit';
import { eventRuleName, groupByRule, sevOf, sortByDetail, sortCommon } from '../format';
import {
  eventFlapClears as clearsOf,
  eventFlapCycles as cyclesOf,
  eventFlapFires as firesOf,
  perHour as rateOf,
} from '../findingFacts';
import { scoreTone as sevFor } from '../findingTone';
import type { ReportBodyProps } from '../types';
import type { AnalysisFinding } from '../../../types/api';

function FlapRow({ finding }: { finding: AnalysisFinding }) {
  const { t } = useTranslation('troubleshoot');
  return (
    <FindingRow
      finding={finding}
      identity={
        <>
          <NodeRef finding={finding} />
          <MonoLine>{eventRuleName(finding.metric)}</MonoLine>
          <span className="ts-anom-kind">
            {t('report.event_flap.fireClear', {
              fires: firesOf(finding),
              clears: clearsOf(finding),
            })}
          </span>
        </>
      }
      viz={
        <BalanceBar fires={firesOf(finding)} clears={clearsOf(finding)} severity={sevOf(finding)} />
      }
      right={
        <RightRail
          when={t('report.event_flap.cycles', { count: cyclesOf(finding) })}
          detail={t('report.event_flap.perHour', { rate: rateOf(finding).toFixed(1) })}
        />
      }
    />
  );
}

export function EventFlapBody({ findings }: ReportBodyProps) {
  const { t } = useTranslation('troubleshoot');
  const [groupBy, setGroupBy] = useState<'finding' | 'rule'>('finding');
  const [filter, setFilter] = useState<'all' | 'cycles5' | 'rate1'>('all');
  const [sort, setSort] = useState<'cycles' | 'rate' | 'rule' | 'node'>('cycles');

  const groups = useMemo(() => groupByRule(findings), [findings]);

  const top = useMemo<RankedRow[]>(
    () =>
      groups.slice(0, 10).map((g, i) => ({
        label: `${i + 1}. ${g.ruleName}`,
        value: g.cycles,
        valueText: t('report.event_flap.cycles', { count: g.cycles }),
        color:
          sevFor(g.score) === 'crit'
            ? 'var(--status-critical)'
            : sevFor(g.score) === 'warn'
              ? 'var(--status-warning)'
              : 'var(--series-5)',
      })),
    [groups, t],
  );

  const list = useMemo(() => {
    const base = findings.filter((f) => {
      if (filter === 'cycles5') return cyclesOf(f) >= 5;
      if (filter === 'rate1') return rateOf(f) >= 1;
      return true;
    });
    if (sort === 'cycles') return sortByDetail(base, 'cycles');
    if (sort === 'rate') return sortByDetail(base, 'per_hour');
    if (sort === 'rule')
      return base.slice().sort((a, b) => eventRuleName(a.metric).localeCompare(eventRuleName(b.metric)));
    return sortCommon(base, 'node');
  }, [findings, filter, sort]);

  return (
    <>
      {top.length > 0 && (
        <Card title={t('report.event_flap.worstRules')}>
          <RankedBars rows={top} />
        </Card>
      )}
      <ReportToolbar
        id="tsr-event-flap"
        filters={
          <Chips
            value={filter}
            onChange={setFilter}
            options={[
              { value: 'all', label: t('report.common.filters.all') },
              { value: 'cycles5', label: t('report.event_flap.filters.cycles5') },
              { value: 'rate1', label: t('report.event_flap.filters.rate1') },
            ]}
          />
        }
        extra={
          <Segmented
            ariaLabel={t('report.event_flap.groupBy')}
            value={groupBy}
            onChange={(v) => setGroupBy(v as 'finding' | 'rule')}
            options={[
              { value: 'finding', label: t('report.event_flap.byFinding') },
              { value: 'rule', label: t('report.event_flap.byRule') },
            ]}
          />
        }
        sort={sort}
        onSort={(v) => setSort(v as 'cycles' | 'rate' | 'rule' | 'node')}
        sortOptions={[
          { value: 'cycles', label: t('report.event_flap.sort.byCycles') },
          { value: 'rate', label: t('report.event_flap.sort.byRate') },
          { value: 'rule', label: t('report.event_flap.sort.byRule') },
          { value: 'node', label: t('report.common.sort.byNode') },
        ]}
      />

      {groupBy === 'rule' ? (
        <div className="ts-anoms">
          {groups.length ? (
            groups.map((g) => (
              <div key={g.key} className={`ts-anom sev-${sevFor(g.score)}`}>
                <div className="ts-anom-score">
                  <span className="ts-anom-score-num">{Math.round(g.score)}</span>
                  <span className="ts-anom-score-cap">{t('report.common.score')}</span>
                </div>
                <div className="ts-anom-id">
                  <span className="ts-anom-node mono">{g.ruleName}</span>
                  <span className="ts-anom-metric">
                    {t('report.event_flap.acrossNodes', { count: g.nodes })}
                  </span>
                </div>
                <div className="ts-anom-chart">
                  <BalanceBar fires={g.fires} clears={g.clears} severity={sevFor(g.score)} />
                </div>
                <div className="ts-anom-right">
                  <span className="ts-anom-when">
                    {t('report.event_flap.cycles', { count: g.cycles })}
                  </span>
                  <span className="ts-anom-dur">
                    {t('report.event_flap.worstNode', { rate: g.worstPerHour.toFixed(1) })}
                  </span>
                </div>
              </div>
            ))
          ) : (
            <EmptyList total={findings.length} />
          )}
        </div>
      ) : (
        <div className="ts-anoms">
          {list.length ? (
            list.map((f) => <FlapRow key={f.id} finding={f} />)
          ) : (
            <EmptyList total={findings.length} />
          )}
        </div>
      )}
    </>
  );
}
