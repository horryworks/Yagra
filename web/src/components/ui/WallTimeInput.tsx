// SPDX-License-Identifier: AGPL-3.0-only
// A date box and a time box that read the same whatever the browser's language (ADR-261).
//
// `<input type="datetime-local">` draws the browser's own format - `mm/dd/yyyy` and AM/PM on an
// American setup - and a page cannot change it. These boxes are plain text: the date is year
// first (`2026-11-20`, `2026/11/20` accepted) with its weekday beside it, the time is 24-hour
// (`2200` becomes `22:00` when the box is left). The calendar button opens the browser's own date
// picker over a hidden native input, so picking with the mouse still works; what it picks is
// written back year first. Reading the text is `lib/maintenanceTime.ts`, which is tested.

import { useRef } from 'react';
import { useTranslation } from 'react-i18next';
import { TextInput } from './Field';
import { IconButton } from './IconButton';
import { CalendarIcon } from './icons';
import {
  dateText,
  parseDateText,
  parseTimeText,
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
  'aria-label'?: string;
}

export function TimeInput({ id, value, onChange, invalid, ...aria }: TimeProps) {
  return (
    <TextInput
      id={id}
      className="wt-time"
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
