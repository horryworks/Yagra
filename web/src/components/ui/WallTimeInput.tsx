// SPDX-License-Identifier: AGPL-3.0-only
// A date box and a time box that read the same whatever the browser's language (ADR-261).
//
// `<input type="datetime-local">` draws the browser's own format - `mm/dd/yyyy` and AM/PM on an
// American setup - and a page cannot change it. These boxes are plain text: the date is year
// first (`2026-11-20`, `2026/11/20` accepted) with its weekday beside it, the time is 24-hour in
// two boxes, hours and minutes. The calendar button opens the browser's own date picker over a
// hidden native input, so picking with the mouse still works; what it picks is written back year
// first. Reading the text is `lib/maintenanceTime.ts`, which is tested.
//
// Every box carries ▲▼ at its right edge (ADR-261 Inc.6): a date moves a day, an hour an hour,
// a minute to the next quarter. A press steps once and a held press keeps stepping; the box's own
// up and down arrow keys do the same, so the buttons are not Tab stops. A caller that pairs a
// date with a time (or keeps its own idea of where an empty box should start) passes `onStep`.

import {
  useEffect,
  useRef,
  useState,
  type ChangeEvent,
  type KeyboardEvent,
  type MouseEvent,
  type ReactNode,
} from 'react';
import { useTranslation } from 'react-i18next';
import { IconButton } from './IconButton';
import { CalendarIcon, ChevronDownIcon, ChevronUpIcon } from './icons';
import { displayTimeZone } from '../../lib/timeZone';
import {
  clockValue,
  dateText,
  joinDateTime,
  keyStep,
  nextWholeHour,
  parseDateText,
  parseTimeText,
  samePart,
  splitDateTime,
  stepDateText,
  stepTimeOfDayText,
  stepWallText,
  timePart,
  timeText,
  typedTimePart,
  weekdayOf,
  type StepDir,
  type TimeUnit,
} from '../../lib/maintenanceTime';
import './WallTimeInput.css';

/** An up or down arrow in a box steps it (the box keeps the key); anything else is the box's. */
const stepByKey = (e: KeyboardEvent<HTMLInputElement>, step: (dir: StepDir) => void) => {
  const dir = keyStep(e);
  if (dir == null) return;
  e.preventDefault();
  step(dir);
};

/** How long a press is held before it repeats, and how often it repeats then. */
const HOLD_DELAY_MS = 400;
const HOLD_EVERY_MS = 90;

/** The ▲▼ at a box's right edge. */
function Stepper({
  onStep,
  upLabel,
  downLabel,
}: {
  onStep: (dir: StepDir) => void;
  upLabel: string;
  downLabel: string;
}) {
  // The repeat calls the newest handler, not the one the press began with: each step re-renders
  // the caller with the value the next step must start from.
  const latest = useRef(onStep);
  useEffect(() => {
    latest.current = onStep;
  });
  const timers = useRef<{ wait?: number; every?: number }>({});
  const stop = () => {
    window.clearTimeout(timers.current.wait);
    window.clearInterval(timers.current.every);
    timers.current = {};
  };
  useEffect(() => stop, []);
  const press = (dir: StepDir) => (e: MouseEvent) => {
    if (e.button !== 0) return;
    // Keep the caret where it is: the box being typed in stays the one being typed in.
    e.preventDefault();
    stop();
    latest.current(dir);
    timers.current.wait = window.setTimeout(() => {
      timers.current.every = window.setInterval(() => latest.current(dir), HOLD_EVERY_MS);
    }, HOLD_DELAY_MS);
  };
  // A click with no mouse press behind it (an assistive technology's) steps once.
  const click = (dir: StepDir) => (e: MouseEvent) => {
    if (e.detail === 0) latest.current(dir);
  };
  return (
    <span className="wt-step">
      {(
        [
          [1, upLabel, <ChevronUpIcon key="u" />],
          [-1, downLabel, <ChevronDownIcon key="d" />],
        ] as const
      ).map(([dir, label, icon]) => (
        <button
          key={dir}
          type="button"
          className="wt-step-btn"
          tabIndex={-1}
          aria-label={label}
          onMouseDown={press(dir)}
          onMouseUp={stop}
          onMouseLeave={stop}
          onClick={click(dir)}
        >
          {icon}
        </button>
      ))}
    </span>
  );
}

interface DateProps {
  id?: string;
  value: string;
  onChange: (text: string) => void;
  invalid?: boolean;
  /** What ▲ (1) and ▼ (-1) do; a day later or earlier when left out. */
  onStep?: (dir: StepDir) => void;
  /** The box's name when no `<label>` points at it. */
  'aria-label'?: string;
}

export function DateInput({ id, value, onChange, invalid, onStep, ...aria }: DateProps) {
  const { t } = useTranslation('common');
  const native = useRef<HTMLInputElement>(null);
  const parsed = parseDateText(value);
  const iso = parsed && parsed !== 'invalid' ? dateText(parsed) : '';
  const step = (dir: StepDir) =>
    onStep ? onStep(dir) : onChange(stepDateText(value, dir, nextWholeHour(Date.now(), displayTimeZone()).date));
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
      <span className="field wt-box">
        <input
          id={id}
          className="wt-in"
          value={value}
          onChange={(e) => onChange(e.target.value)}
          onBlur={() => {
            const d = parseDateText(value);
            if (d && d !== 'invalid') onChange(dateText(d));
          }}
          onKeyDown={(e) => stepByKey(e, step)}
          placeholder="2026-11-20"
          inputMode="numeric"
          autoComplete="off"
          aria-invalid={invalid || undefined}
          {...aria}
        />
        <span className="wt-suffix">{weekdayOf(value)}</span>
        <Stepper
          onStep={step}
          upLabel={t('wallTime.dayLater')}
          downLabel={t('wallTime.dayEarlier')}
        />
      </span>
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
  /** Given to the hour box, which a `<label>` then names. */
  id?: string;
  value: string;
  onChange: (text: string) => void;
  invalid?: boolean;
  className?: string;
  /** What ▲ (1) and ▼ (-1) do on the hour or the minute; round the clock within the day when left
   *  out. */
  onStep?: (unit: TimeUnit, dir: StepDir) => void;
  /** The time's name; each box adds "hour" or "minute" to it. */
  'aria-label'?: string;
}

/** A 24-hour time in two boxes, hours and minutes (ADR-261 decision 35, 38). `value` and
 *  `onChange` carry the whole time as text (`22:00`, or whatever has been typed so far). */
export function TimeInput({
  id,
  value,
  onChange,
  invalid,
  className,
  onStep,
  'aria-label': label = '',
}: TimeProps) {
  const { t } = useTranslation('common');
  // The box being typed in shows what was typed (`2`), the other one the value (`02`).
  const [typing, setTyping] = useState<{ part: 0 | 1; shown: string } | null>(null);
  const [seen, setSeen] = useState(value);
  if (value !== seen) {
    setSeen(value);
    if (typing && !samePart(typing.shown, timePart(value, typing.part))) {
      setTyping({ part: typing.part, shown: timePart(value, typing.part) });
    }
  }
  const step = (unit: TimeUnit, dir: StepDir) =>
    onStep ? onStep(unit, dir) : onChange(stepTimeOfDayText(value, unit, dir, nextWholeHour(Date.now(), displayTimeZone()).time));
  const typed = (part: 0 | 1) => (e: ChangeEvent<HTMLInputElement>) => {
    const native = e.nativeEvent as InputEvent;
    const whole = native.inputType === 'insertFromPaste' || (native.data?.length ?? 0) > 1;
    const next = typedTimePart(e.target.value, part, value, whole);
    setTyping({ part, shown: next.shown });
    onChange(next.text);
  };
  const box = (part: 0 | 1) => {
    const unit: TimeUnit = part === 0 ? 'hour' : 'minute';
    return (
      <span className="field wt-box wt-part">
        <input
          id={part === 0 ? id : undefined}
          className="wt-in"
          value={typing?.part === part ? typing.shown : timePart(value, part)}
          onChange={typed(part)}
          onFocus={(e) => {
            setTyping({ part, shown: timePart(value, part) });
            e.target.select();
          }}
          onBlur={() => {
            setTyping(null);
            const tm = parseTimeText(value);
            if (tm && tm !== 'invalid' && timeText(tm) !== value) onChange(timeText(tm));
          }}
          onKeyDown={(e) => stepByKey(e, (dir) => step(unit, dir))}
          placeholder={part === 0 ? '22' : '00'}
          inputMode="numeric"
          autoComplete="off"
          aria-invalid={invalid || undefined}
          // With no name of its own, the hour box is named by the `<label>` pointing at its id.
          aria-label={
            part === 0
              ? label
                ? t('wallTime.hour', { label })
                : undefined
              : label
                ? t('wallTime.minute', { label })
                : t('wallTime.minuteAlone')
          }
        />
        <Stepper
          onStep={(dir) => step(unit, dir)}
          upLabel={part === 0 ? t('wallTime.hourLater') : t('wallTime.minutesLater')}
          downLabel={part === 0 ? t('wallTime.hourEarlier') : t('wallTime.minutesEarlier')}
        />
      </span>
    );
  };
  return (
    <span
      className={className ? `wt-time ${className}` : 'wt-time'}
      role="group"
      aria-label={label || undefined}
    >
      {box(0)}
      <span className="wt-colon" aria-hidden="true">
        :
      </span>
      {box(1)}
    </span>
  );
}

/** Start and end rows whose columns line up (ADR-261 decision 39): put `Field`s inside, and in
 *  each the date, the time and what follows them inside a `WallTimeCols`. The date takes what room
 *  is left, and the time and the last column are as wide as the widest of their rows. */
export function WallTimeRows({ children }: { children: ReactNode }) {
  return <div className="wt-rows">{children}</div>;
}

export function WallTimeCols({ children }: { children: ReactNode }) {
  return <div className="wt-cols">{children}</div>;
}

/** A zone's name in the last column, where the row above has the zone's button: its text starts
 *  where the button's does. */
export function WallTimeZone({ children }: { children: ReactNode }) {
  return <span className="wt-zone">{children}</span>;
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
  /** The name the date box is read out by; the time boxes add "time" to it. */
  label: string;
}

/** A year-first date and a 24-hour time, in place of `<input type="datetime-local">`. A time
 *  stepped past midnight carries the date with it (ADR-261 decision 36). */
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
        onStep={(dir) => emit(stepWallText(draft, 'day', dir, nextWholeHour(Date.now(), displayTimeZone())))}
        invalid={dateBad}
        aria-label={label}
      />
      <TimeInput
        value={draft.time}
        onChange={(time) => emit({ ...draft, time })}
        onStep={(unit, dir) => emit(stepWallText(draft, unit, dir, nextWholeHour(Date.now(), displayTimeZone())))}
        invalid={timeBad}
        aria-label={t('wallTime.time', { label })}
      />
    </span>
  );
}
