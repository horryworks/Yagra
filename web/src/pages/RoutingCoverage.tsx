// SPDX-License-Identifier: AGPL-3.0-only
// Coverage (ADR-262 decisions 8 and 12): a card of its own under the routing rules. Its title says
// what it draws — who gets an alert of one severity, across a week — and every node is grouped by
// the rules that reach it, so a stretch where nobody hears an alert can be seen before it happens.
// When some nodes reach nobody (or are held), one line above the week says how many and offers the
// catch-all. The server draws the week; this paints it. The judgement is in `coverageWeek.ts`.

import { useState } from 'react';
import { useTranslation } from 'react-i18next';
import { api } from '../services/api';
import {
  SEVERITIES,
  type NotificationChannel,
  type RoutingCoverage as Coverage,
  type RoutingRule,
  type Severity,
} from '../types/api';
import { useLoad } from '../lib/useLoad';
import { severityLabel } from '../lib/format';
import { Card } from '../components/ui/Card';
import { Callout, SectionHead } from '../components/ui/Section';
import { Button } from '../components/ui/Button';
import { Segmented } from '../components/ui/Segmented';
import { Inline, Stack } from '../components/ui/Page';
import {
  channelColour,
  coverageGaps,
  coverageNodes,
  daySlices,
  paintedChannels,
  rowRuleNames,
} from './coverageWeek';
import { WEEKDAYS, weekdayName } from './deliveryHours';

export function RoutingCoverage({
  rules,
  channels,
  catchAll,
  onEditCatchAll,
}: {
  rules: RoutingRule[];
  channels: NotificationChannel[];
  catchAll: string[];
  /** Opens the catch-all's channels; absent for a caller who may not change them. */
  onEditCatchAll?: () => void;
}) {
  const { t } = useTranslation('alertsConfig');
  const [severity, setSeverity] = useState<Severity>('critical');
  const load = useLoad<Coverage | null>(
    () => api.getRoutingCoverage(severity),
    [severity, rules, catchAll.join(',')],
    { initial: null },
  );
  const cov = load.data;
  const ruleName = (id: string) => rules.find((r) => r.id === id)?.name;
  const channelName = (id: string) => channels.find((c) => c.id === id)?.name ?? id;
  const painted = cov ? paintedChannels(cov.rows) : [];
  const colour = (id: string) => channelColour(Math.max(0, painted.indexOf(id)));
  const gaps = cov ? coverageGaps(cov.rows) : { held: 0, nowhere: 0 };
  const total = cov ? coverageNodes(cov.rows) : 0;
  const fix = onEditCatchAll && (
    <Button variant="outline" onClick={onEditCatchAll}>
      {t('routing.coverage.setCatchAll')}
    </Button>
  );

  return (
    <Stack as="section" gap={12} className="routing-coverage">
      <SectionHead
        level={2}
        title={t('routing.coverage.titleFor', { severity: severityLabel(severity) })}
        actions={
          <Segmented
            size="sm"
            ariaLabel={t('routing.coverage.severity')}
            value={severity}
            onChange={(v) => {
              const s = SEVERITIES.find((x) => x === v);
              if (s) setSeverity(s);
            }}
            options={SEVERITIES.map((s) => ({ value: s, label: severityLabel(s) }))}
          />
        }
      />
      <Card>
      {cov && (
        <Stack gap={8}>
          {gaps.nowhere > 0 ? (
            <Callout tone="critical" status className="routing-coverage-gaps">
              <Inline gap={12} justify="between">
                <span>{t('routing.coverage.nowhere', { count: gaps.nowhere, total })}</span>
                {fix}
              </Inline>
            </Callout>
          ) : gaps.held > 0 ? (
            <Callout tone="warning" status className="routing-coverage-gaps">
              <Inline gap={12} justify="between">
                <span>{t('routing.coverage.held', { count: gaps.held, total })}</span>
                {fix}
              </Inline>
            </Callout>
          ) : null}
          <div className="coverage-row">
            <span className="muted">{t('routing.coverage.rowsHead')}</span>
            <div className="coverage-week coverage-days">
              {WEEKDAYS.map((d) => (
                <span key={d}>{weekdayName(t, d)}</span>
              ))}
            </div>
          </div>
          {cov.rows.map((row) => (
            <div className="coverage-row" key={`${row.rule_ids.join(',')}|${row.zone ?? ''}`}>
              <Stack gap={2}>
                <span className="yt-name-txt">
                  {t('routing.coverage.nodes', { count: row.node_count })}
                </span>
                <span className="muted">
                  {rowRuleNames(row, ruleName) ?? t('routing.coverage.noRule')}
                  {row.zone ? ` · ${row.zone}` : ''}
                </span>
              </Stack>
              <div className="coverage-week">
                {WEEKDAYS.map((d) => (
                  <div className="coverage-day" key={d}>
                    {daySlices(row.segments, d - 1).map((s) => (
                      <div
                        key={s.start}
                        className={`coverage-seg coverage-${s.kind}`}
                        style={{ flexGrow: s.end - s.start }}
                        title={
                          s.channel_ids.length > 0
                            ? s.channel_ids.map(channelName).join(', ')
                            : t(`routing.coverage.kind.${s.kind}`)
                        }
                      >
                        {s.channel_ids.map((c) => (
                          <span key={c} style={{ background: colour(c) }} />
                        ))}
                      </div>
                    ))}
                  </div>
                ))}
              </div>
            </div>
          ))}
          <Inline gap={12} className="coverage-legend">
            {painted.map((c) => (
              <span key={c}>
                <i style={{ background: colour(c) }} />
                {channelName(c)}
                {cov.catch_all.includes(c) ? ` (${t('routing.catchAll.title')})` : ''}
              </span>
            ))}
            <span>
              <i className="coverage-held" />
              {t('routing.coverage.kind.held')}
            </span>
            <span>
              <i className="coverage-nowhere" />
              {t('routing.coverage.kind.nowhere')}
            </span>
          </Inline>
        </Stack>
      )}
      </Card>
    </Stack>
  );
}
