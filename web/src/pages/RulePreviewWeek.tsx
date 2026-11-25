// SPDX-License-Identifier: AGPL-3.0-only
// The rule dialog's week (ADR-266 decision 5): for the nodes the draft reaches, a bar per weekday
// painted by the rule that decides each stretch, a legend naming each rule and its channels, and a
// line saying where an alert would go now. It shows the result instead of explaining the order
// (ADR-200). The judgement is in `rulePreview.ts`.

import { useEffect, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { api } from '../services/api';
import type { NotificationChannel, RoutingRule, RulePreview } from '../types/api';
import { useLoad } from '../lib/useLoad';
import { useDebouncedValue } from '../lib/useDebouncedValue';
import { FieldGroup } from '../components/ui/FormLayout';
import { Select } from '../components/ui/Field';
import { daySlices } from './coverageWeek';
import { WEEKDAYS, weekdayName } from './deliveryHours';
import {
  dayPercent,
  deciderColour,
  deciderOf,
  legendOf,
  nowOnDay,
  nowStretch,
  otherDeciders,
  previewQuery,
  rowZone,
  weekMinuteText,
  type Decider,
  type PreviewDraft,
  type PreviewQuery,
} from './rulePreview';

/** How long the fields rest before the week is asked again: it reads every node. */
const SETTLE_MS = 400;

/** The axis marks, in hours. */
const AXIS_HOURS = [0, 6, 12, 18, 24] as const;

export function RulePreviewWeek({
  draft,
  name,
  rules,
  channels,
}: {
  draft: PreviewDraft;
  /** The draft's name as typed, for its legend entry. */
  name: string;
  rules: readonly RoutingRule[];
  channels: readonly NotificationChannel[];
}) {
  const { t } = useTranslation('alertsConfig');
  // While the hours cannot be read the dialog says why, and the week keeps the last answer.
  const asked = previewQuery(draft);
  const askedKey = asked ? JSON.stringify(asked) : null;
  const [validKey, setValidKey] = useState(askedKey);
  useEffect(() => {
    if (askedKey) setValidKey(askedKey);
  }, [askedKey]);
  const settled = useDebouncedValue(validKey, SETTLE_MS);
  const { data: preview } = useLoad<RulePreview | null>(
    () =>
      settled
        ? api.previewRoutingRule(JSON.parse(settled) as PreviewQuery)
        : Promise.resolve(null),
    [settled],
    { initial: null },
  );
  const [pick, setPick] = useState(0);
  if (!preview || preview.rows.length === 0) return null;
  const row = preview.rows[Math.min(pick, preview.rows.length - 1)];
  const thisRule = preview.rule_id;
  const others = otherDeciders(row, thisRule);
  const channelNames = (ids: readonly string[]) =>
    ids.map((id) => channels.find((c) => c.id === id)?.name ?? id).join(', ');
  const ruleName = (d: Decider): string => {
    switch (d.kind) {
      case 'this':
        return name.trim()
          ? t('routing.preview.this', { name: name.trim() })
          : t('routing.preview.thisUnnamed');
      case 'rule': {
        const at = rules.findIndex((r) => r.id === d.ruleId);
        return at < 0
          ? d.ruleId
          : t('routing.preview.rule', { name: rules[at].name, n: at + 1 });
      }
      case 'catch_all':
        return t('routing.catchAll.title');
      case 'held':
        return t('routing.preview.held');
      case 'nowhere':
        return t('routing.coverage.kind.nowhere');
    }
  };
  const shortName = (d: Decider): string =>
    d.kind === 'this'
      ? name.trim() || t('routing.preview.thisUnnamed')
      : d.kind === 'rule'
        ? (rules.find((r) => r.id === d.ruleId)?.name ?? d.ruleId)
        : ruleName(d);
  const { zone, isDefault } = rowZone(preview, row);
  const zoneText = isDefault ? `${zone} · ${t('routing.when.zoneDefault')}` : zone;
  const now = nowOnDay(row);
  const at = nowStretch(row);
  const nowWhat = (() => {
    if (!at) return '';
    const d = deciderOf(at, thisRule);
    if (d.kind === 'held') {
      return row.next_change_minute === null || row.next_change_minute === undefined
        ? t('routing.rules.hold')
        : t('routing.preview.heldUntil', { when: weekMinuteText(t, row.next_change_minute) });
    }
    if (d.kind === 'nowhere') return ruleName(d);
    return `${shortName(d)} → ${channelNames(at.channel_ids)}`;
  })();

  return (
    <FieldGroup label={t('routing.preview.title')}>
      {preview.rows.length > 1 && (
        <Select
          aria-label={t('routing.preview.title')}
          value={String(Math.min(pick, preview.rows.length - 1))}
          onChange={(e) => setPick(Number(e.target.value))}
        >
          {preview.rows.map((r, i) => (
            <option key={`${r.rule_ids.join(',')}|${r.zone ?? ''}`} value={String(i)}>
              {`${t('routing.coverage.nodes', { count: r.node_count })} · ${rowZone(preview, r).zone}`}
            </option>
          ))}
        </Select>
      )}
      <div className="rule-week" role="img" aria-label={t('routing.preview.title')}>
        <div className="rule-week-row">
          <span className="rule-week-day" />
          <div className="rule-week-axis">
            {AXIS_HOURS.map((h) => (
              <span
                key={h}
                className={h === 0 ? 'is-first' : h === 24 ? 'is-last' : undefined}
                style={{ left: dayPercent(h * 60) }}
              >
                {`${String(h).padStart(2, '0')}:00`}
              </span>
            ))}
          </div>
        </div>
        {WEEKDAYS.map((d) => (
          <div className="rule-week-row" key={d}>
            <span className={now.day === d - 1 ? 'rule-week-day is-today' : 'rule-week-day'}>
              {weekdayName(t, d)}
            </span>
            <div className="rule-week-bar">
              {daySlices(row.segments, d - 1).map((s) => {
                const decider = deciderOf(s, thisRule);
                const colour = deciderColour(decider, others);
                return (
                  <div
                    key={s.start}
                    className={colour === null ? 'rule-week-seg is-held' : 'rule-week-seg'}
                    style={{
                      left: dayPercent(s.start),
                      width: dayPercent(s.end - s.start),
                      ...(colour === null ? {} : { background: colour }),
                    }}
                  />
                );
              })}
              {now.day === d - 1 && <div className="rule-week-now" style={{ left: now.left }} />}
            </div>
          </div>
        ))}
      </div>
      <ul className="rule-week-legend">
        {legendOf(row, thisRule).map((e) => {
          const colour = deciderColour(e.decider, others);
          const sends = e.decider.kind !== 'held' && e.decider.kind !== 'nowhere';
          return (
            <li key={`${e.decider.kind}|${e.decider.kind === 'rule' ? e.decider.ruleId : ''}`}>
              <i
                className={colour === null ? 'is-held' : undefined}
                style={colour === null ? undefined : { background: colour }}
              />
              {sends ? `${ruleName(e.decider)} → ${channelNames(e.channels)}` : ruleName(e.decider)}
            </li>
          );
        })}
      </ul>
      <p className="form-status">
        {`${t('routing.preview.now', { when: weekMinuteText(t, row.now_minute), zone: zoneText })}: ${nowWhat}`}
      </p>
    </FieldGroup>
  );
}
