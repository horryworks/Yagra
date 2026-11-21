// SPDX-License-Identifier: AGPL-3.0-only
// Coverage (ADR-262 decision 8), under the routing rules: every node grouped by the rules that
// reach it, and where its alerts go across a week, so a stretch where nobody hears an alert can be
// seen before it happens. The server draws the week; this paints it. The judgement is in
// `coverageWeek.ts`.

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
import { SectionHead } from '../components/ui/Section';
import { Segmented } from '../components/ui/Segmented';
import { Inline, Stack } from '../components/ui/Page';
import { channelColour, coverageGaps, daySlices, paintedChannels, rowRuleNames } from './coverageWeek';
import { WEEKDAYS, weekdayName } from './deliveryHours';

export function RoutingCoverage({
  rules,
  channels,
}: {
  rules: RoutingRule[];
  channels: NotificationChannel[];
}) {
  const { t } = useTranslation('alertsConfig');
  const [severity, setSeverity] = useState<Severity>('critical');
  const load = useLoad<Coverage | null>(() => api.getRoutingCoverage(severity), [severity, rules], {
    initial: null,
  });
  const cov = load.data;
  const ruleName = (id: string) => rules.find((r) => r.id === id)?.name;
  const channelName = (id: string) => channels.find((c) => c.id === id)?.name ?? id;
  const painted = cov ? paintedChannels(cov.rows) : [];
  const colour = (id: string) => channelColour(Math.max(0, painted.indexOf(id)));
  const gaps = cov ? coverageGaps(cov.rows) : { held: 0, nowhere: 0 };

  return (
    <Stack gap={8} className="routing-coverage">
      <SectionHead
        title={t('routing.coverage.title')}
        tools={
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
        actions={
          cov && (gaps.held > 0 || gaps.nowhere > 0) ? (
            <span className="routing-coverage-gaps">
              {t('routing.coverage.gaps', { held: gaps.held, nowhere: gaps.nowhere })}
            </span>
          ) : null
        }
      />
      {cov && (
        <>
          <div className="coverage-row">
            <span />
            <div className="coverage-week coverage-days">
              {WEEKDAYS.map((d) => (
                <span key={d}>{weekdayName(t, d)}</span>
              ))}
            </div>
          </div>
          {cov.rows.map((row) => (
            <div className="coverage-row" key={`${row.rule_ids.join(',')}|${row.zone ?? ''}`}>
              <span className="coverage-label">
                <span className="yt-name-txt">
                  {rowRuleNames(row, ruleName) ?? t('routing.coverage.noRule')}
                </span>{' '}
                <span className="muted">
                  {t('routing.coverage.nodes', { count: row.node_count })}
                  {row.zone ? ` · ${row.zone}` : ''}
                </span>
              </span>
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
        </>
      )}
    </Stack>
  );
}
