// SPDX-License-Identifier: AGPL-3.0-only
// A date box and a time box that read the same whatever the browser's language (ADR-261).
//
// `<input type="datetime-local">` draws the browser's own format - `mm/dd/yyyy` and AM/PM on an
// American setup - and a page cannot change it. These boxes are plain text: the date is year
// first (`2026-11-20`, `2026/11/20` accepted) with its weekday beside it, the time is 24-hour
// (`2200` becomes `22:00` when the box is left). The calendar button opens the browser's own date
// picker over a hidden native input, so picking with the mouse still works; what it picks is
// written back year first. Reading the text is `lib/maintenanceTime.ts`, which is tested.

import { useRef, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { TextInput } from './Field';
import { IconButton } from './IconButton';
import { CalendarIcon } from './icons';
import {
  clockValue,
  dateText,
  joinDateTime,
  parseDateText,
  parseTimeText,
  splitDateTime,
  timeText,
  weekdayOf,
} from '../../lib/maintenanceTime';
import './WallTimeInput.css';

interface DateProps {
  id?: string;
  value: string;
  onChange: (text: string) => void;
  invalid?: boolean;
  /** The box's name when no `<label>` points at it. */
  'aria-label'?: string;
}

export function DateInput({ id, value, onChange, invalid, ...aria }: DateProps) {
  const { t } = useTranslation('common');
  const native = useRef<HTMLInputElement>(null);
  const parsed = parseDateText(value);
  const iso = parsed && parsed !== 'invalid' ? dateText(parsed) : '';
  const open = () => {
    const el = native.current;
    if (!el) return;
    // `showPicker` is in every browser this WebUI supports; a focus is the fallback where it throws
    // (it refuses outside a user gesture, which a click is).
    try {
      el.showPicker();
    } catch {
      el.focus();
    }
  };
  return (
    <span className="wt-date">
      <TextInput
        id={id}
        value={value}
        onChange={(e) => onChange(e.target.value)}
        onBlur={() => {
          const d = parseDateText(value);
          if (d && d !== 'invalid') onChange(dateText(d));
        }}
        placeholder="2026-11-20"
        inputMode="numeric"
        autoComplete="off"
        aria-invalid={invalid || undefined}
        suffix={weekdayOf(value)}
        {...aria}
      />
      <IconButton title={t('wallTime.pickDate')} onClick={open}>
        <CalendarIcon />
      </IconButton>
      <input
        ref={native}
        type="date"
        className="wt-native"
        tabIndex={-1}
        aria-hidden="true"
        value={iso}
        onChange={(e) => {
          const d = parseDateText(e.target.value);
          if (d && d !== 'invalid') onChange(dateText(d));
        }}
      />
    </span>
  );
}

interface TimeProps {
  id?: string;
  value: string;
  onChange: (text: string) => void;
  invalid?: boolean;
  className?: string;
  'aria-label'?: string;
}

export function TimeInput({ id, value, onChange, invalid, className, ...aria }: TimeProps) {
  return (
    <TextInput
      id={id}
      className={className ? `wt-time ${className}` : 'wt-time'}
      value={value}
      onChange={(e) => onChange(e.target.value)}
      onBlur={() => {
        const tm = parseTimeText(value);
        if (tm && tm !== 'invalid') onChange(timeText(tm));
      }}
      placeholder="22:00"
      inputMode="numeric"
      autoComplete="off"
      aria-invalid={invalid || undefined}
      {...aria}
    />
  );
}

// ── Drop-in replacements for `<input type="time">` and `<input type="datetime-local">` ─────────
// (ADR-261 Inc.2). Each keeps what is typed inside itself and hands its caller the string the
// browser's input used to: `HH:MM` / `YYYY-MM-DDTHH:MM` once the text reads, `''` until then. So a
// caller's own reading of the value is unchanged, and half-typed text is never saved as a time.

interface TimeOfDayProps {
  id?: string;
  /** `HH:MM`, or `''`. */
  value: string;
  onChange: (value: string) => void;
  className?: string;
  'aria-label'?: string;
}

/** A 24-hour time of day, in place of `<input type="time">` (which shows AM/PM on some setups). */
export function TimeOfDayInput({ id, value, onChange, className, ...aria }: TimeOfDayProps) {
  const [draft, setDraft] = useState(value);
  const [seen, setSeen] = useState(value);
  // A new value from the caller replaces the text only when the text does not already say it -
  // so the value this box just handed out comes back without overwriting what is being typed,
  // and a caller that ignores `''` (it keeps its last good time) leaves the half-typed text alone.
  if (value !== seen) {
    setSeen(value);
    if (clockValue(draft) !== value) setDraft(value);
  }
  const emit = (text: string) => {
    setDraft(text);
    const v = clockValue(text);
    if (v !== value) onChange(v);
  };
  return (
    <TimeInput
      id={id}
      value={draft}
      onChange={emit}
      invalid={draft.trim() !== '' && clockValue(draft) === ''}
      className={className}
      {...aria}
    />
  );
}

interface DateTimeProps {
  id?: string;
  /** `YYYY-MM-DDTHH:MM`, or `''`. */
  value: string;
  onChange: (value: string) => void;
  /** The name the date box is read out by; the time box adds "time" to it. */
  label: string;
}

/** A year-first date and a 24-hour time, in place of `<input type="datetime-local">`. */
export function DateTimeInput({ id, value, onChange, label }: DateTimeProps) {
  const { t } = useTranslation('common');
  const [draft, setDraft] = useState(() => splitDateTime(value));
  const [seen, setSeen] = useState(value);
  if (value !== seen) {
    setSeen(value);
    if (joinDateTime(draft.date, draft.time) !== value) setDraft(splitDateTime(value));
  }
  const emit = (next: { date: string; time: string }) => {
    setDraft(next);
    const v = joinDateTime(next.date, next.time);
    if (v !== value) onChange(v);
  };
  const dateBad = draft.date.trim() !== '' && parseDateText(draft.date) === 'invalid';
  const timeBad = draft.time.trim() !== '' && parseTimeText(draft.time) === 'invalid';
  return (
    <span className="wt-datetime">
      <DateInput
        id={id}
        value={draft.date}
        onChange={(date) => emit({ ...draft, date })}
        invalid={dateBad}
        aria-label={label}
      />
      <TimeInput
        value={draft.time}
        onChange={(time) => emit({ ...draft, time })}
        invalid={timeBad}
        aria-label={t('wallTime.time', { label })}
      />
    </span>
  );
}
