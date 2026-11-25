// SPDX-License-Identifier: AGPL-3.0-only
// A routing rule's hours (ADR-262 decision 3): the judgement behind the rule dialog's "When" and
// the rule list's "When" cell. Kept out of the `.tsx` so Vitest can reach it.

import type { TFunction } from 'i18next';
import type { RuleWhen, WhenClosed, WhenMode } from '../types/api';
import { daysText, hoursText, minutesToTime, timeToMinutes, WEEKDAYS, WORKDAYS } from './deliveryHours';

/** The three answers to "when is this rule open", in the order the switch shows them. */
export const WHEN_MODES = ['always', 'during', 'outside'] as const satisfies readonly WhenMode[];

/** What the dialog edits. Times are strings because that is what the inputs hold. */
export interface WhenDraft {
  mode: WhenMode;
  days: number[];
  start: string;
  end: string;
}

/** A rule's hours as the dialog starts them: its own, or office hours on weekdays for a rule that
 *  had none, so switching to "During" offers something sensible rather than an empty week. */
export function whenDraftFrom(when?: RuleWhen | null): WhenDraft {
  const has = when && when.mode !== 'always' && (when.days ?? []).length > 0;
  return {
    mode: when?.mode ?? 'always',
    days: has ? [...(when.days ?? [])] : [...WORKDAYS],
    start: minutesToTime(has ? (when.start_minute ?? 0) : 8 * 60 + 30),
    end: minutesToTime(has ? (when.end_minute ?? 0) : 17 * 60 + 20),
  };
}

/** Why a draft cannot be saved; each is a key under `routing.when.err.`. */
export const WHEN_ERRORS = ['days', 'time'] as const;
export type WhenError = (typeof WHEN_ERRORS)[number];

/** The body the API takes, or the first thing wrong with the draft - the checks the server makes,
 *  so the dialog can say what is wrong before sending. "Always" sends no window. */
export function whenBodyFrom(d: WhenDraft): { when: RuleWhen } | { error: WhenError } {
  if (d.mode === 'always') return { when: { mode: 'always' } };
  if (d.days.length === 0) return { error: 'days' };
  const start = timeToMinutes(d.start);
  const end = timeToMinutes(d.end);
  if (start === null || end === null) return { error: 'time' };
  return {
    when: {
      mode: d.mode,
      days: WEEKDAYS.filter((x) => d.days.includes(x)),
      start_minute: start,
      end_minute: end,
    },
  };
}

/** The list's "When" cell: "Always", "Mon–Fri 08:30–17:20", or the same after "Outside" - and,
 *  for a rule that holds while closed, "· other times held" after it (ADR-266). */
export function whenText(
  t: TFunction,
  when: RuleWhen | null | undefined,
  whenClosed?: WhenClosed | null,
): string {
  if (!when || when.mode === 'always' || (when.days ?? []).length === 0) {
    return t('alertsConfig:routing.when.always');
  }
  const span = `${daysText(t, when.days ?? [])} ${hoursText({
    start_minute: when.start_minute ?? 0,
    end_minute: when.end_minute ?? 0,
  })}`;
  const open = when.mode === 'during' ? span : t('alertsConfig:routing.when.outsideSpan', { span });
  return whenClosed === 'hold' ? `${open} · ${t('alertsConfig:routing.when.closedHeld')}` : open;
}
