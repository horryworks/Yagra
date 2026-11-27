// SPDX-License-Identifier: AGPL-3.0-only
// The judgement behind the maintenance-window dialog's time fields (ADR-261): reading a typed date
// and time, turning them into an instant in a chosen zone, the length presets, the quick starts,
// and the zone search. Pure, so it is tested (a `.tsx` is never run by Vitest).
//
// WHY A DATE IS WRITTEN YEAR FIRST
// `<input type="datetime-local">` draws whatever the browser's locale says - `mm/dd/yyyy` and AM/PM
// on an American setup - and a page cannot change that. Year first reads the same to someone used
// to either order, so the dialog types its own: `2026-11-20`, with `2026/11/20` accepted. A date
// with no year (`11/20`) is refused, because nothing can tell which of the two numbers is the month.
//
// WHY A TIME IN A GAP IS REFUSED
// Core reads a schedule's wall time in a daylight-saving gap with the offset before it (ADR-221).
// A maintenance window is sent as an instant, so the dialog decides, and it refuses: "02:30" on the
// night the clocks skip it is not a time the requester can have meant. An overlap takes its first
// occurrence, and the "your time" line shows which instant that is.

import { wallClock, wallInstants, utcOffsetText, zoneAbbreviation } from './timeZone';
import { isImeComposing, type ImeKeyEvent } from './ime';

export interface DateParts {
  year: number;
  month: number;
  day: number;
}

export interface TimeParts {
  hour: number;
  minute: number;
}

const pad2 = (n: number) => String(n).padStart(2, '0');

/** A typed date: `2026-11-20` or `2026/11/20` (one-digit month and day allowed). `null` for an
 *  empty box, `'invalid'` for anything else - a date with no year included. */
export function parseDateText(text: string): DateParts | null | 'invalid' {
  const s = text.trim();
  if (!s) return null;
  const m = /^(\d{4})[-/](\d{1,2})[-/](\d{1,2})$/.exec(s);
  if (!m) return 'invalid';
  const [year, month, day] = [Number(m[1]), Number(m[2]), Number(m[3])];
  if (month < 1 || month > 12 || day < 1) return 'invalid';
  // The last day of the month, leap years included, from the calendar rather than a table.
  if (day > new Date(Date.UTC(year, month, 0)).getUTCDate()) return 'invalid';
  return { year, month, day };
}

/** A typed 24-hour time: `22:00`, `9:05`, `2200`, `905`. `null` for an empty box, `'invalid'`
 *  for anything else - `10pm` included, because the dialog is 24-hour throughout. */
export function parseTimeText(text: string): TimeParts | null | 'invalid' {
  const s = text.trim();
  if (!s) return null;
  const m = /^(\d{1,2}):(\d{2})$/.exec(s) ?? /^(\d{1,2})(\d{2})$/.exec(s);
  if (!m) return 'invalid';
  const hour = Number(m[1]);
  const minute = Number(m[2]);
  if (hour > 23 || minute > 59) return 'invalid';
  return { hour, minute };
}

export function dateText(d: DateParts): string {
  return `${d.year}-${pad2(d.month)}-${pad2(d.day)}`;
}

export function timeText(t: TimeParts): string {
  return `${pad2(t.hour)}:${pad2(t.minute)}`;
}

// ── The value shapes the browser's own inputs used (ADR-261 Inc.2) ─────────────────────────
// `DateTimeInput` and `TimeOfDayInput` replace `<input type="datetime-local">` and
// `<input type="time">`, and hand their callers the very strings those inputs produced, so no
// caller's reading (`localInputToUnix`, `timeToMinutes`, …) had to change.

/** A typed time as `<input type="time">` gave it: `HH:MM`, or `''` when the box is empty or
 *  cannot be read. */
export function clockValue(text: string): string {
  const t = parseTimeText(text);
  return t && t !== 'invalid' ? timeText(t) : '';
}

/** `YYYY-MM-DDTHH:MM` (a `datetime-local` value) split into the two boxes' text. */
export function splitDateTime(value: string): { date: string; time: string } {
  const m = /^(\d{4}-\d{2}-\d{2})T(\d{2}:\d{2})/.exec(value);
  return m ? { date: m[1], time: m[2] } : { date: '', time: '' };
}

/** The two boxes' text joined back into `YYYY-MM-DDTHH:MM`, or `''` unless both can be read. */
export function joinDateTime(date: string, time: string): string {
  const d = parseDateText(date);
  const t = parseTimeText(time);
  if (!d || d === 'invalid' || !t || t === 'invalid') return '';
  return `${dateText(d)}T${timeText(t)}`;
}

/** The instant a date and time name in `tz`: `'gap'` when the clocks skip that time there, else
 *  the earliest instant (an overlap's first occurrence). */
export function wallToInstant(d: DateParts, t: TimeParts, tz: string): number | 'gap' {
  const found = wallInstants(d.year, d.month, d.day, t.hour, t.minute, tz);
  return found.length > 0 ? found[0] : 'gap';
}

/** The date and time a clock in `tz` reads at `ms`, to the minute. */
export function instantToWall(ms: number, tz: string): { date: DateParts; time: TimeParts } {
  const w = wallClock(ms, tz);
  return {
    date: { year: w.year, month: w.month, day: w.day },
    time: { hour: w.hour, minute: w.minute },
  };
}

/** The time fields of the dialog for a window that already exists (ADR-261 Inc.5): its start and
 *  its end as clocks in `tz` read them. The end is always a picked end rather than a length, so an
 *  edit shows the end the window has instead of rounding it to a button. A bound that does not
 *  parse leaves both of its fields empty, which the dialog reports as a missing value. */
export function windowFields(
  startsAt: string,
  endsAt: string,
  tz: string,
): { startDate: string; startTime: string; endDate: string; endTime: string } {
  const read = (iso: string) => {
    const ms = Date.parse(iso);
    if (!Number.isFinite(ms)) return { date: '', time: '' };
    const w = instantToWall(ms, tz);
    return { date: dateText(w.date), time: timeText(w.time) };
  };
  const start = read(startsAt);
  const end = read(endsAt);
  return { startDate: start.date, startTime: start.time, endDate: end.date, endTime: end.time };
}

/** The shortcuts under the end (ADR-261 decision 33, which replaced decision 5's length
 *  buttons): each fills the end with the start plus its length. */
export const WINDOW_LENGTHS = [
  { key: '30m', minutes: 30 },
  { key: '1h', minutes: 60 },
  { key: '2h', minutes: 120 },
  { key: '4h', minutes: 240 },
  { key: '8h', minutes: 480 },
] as const;
export type WindowLengthKey = (typeof WINDOW_LENGTHS)[number]['key'];

/** The hour the "Tonight" button starts at, in the chosen zone. Fixed, not remembered (decision 5). */
export const TONIGHT_HOUR = 22;

/** "Tonight 22:00" in `tz`: today's 22:00 there, or tomorrow's once today's has begun. A gap at
 *  22:00 does not exist in any zone, so the instant always resolves. */
export function tonightStart(nowMs: number, tz: string): { date: DateParts; time: TimeParts } {
  const today = instantToWall(nowMs, tz).date;
  const time = { hour: TONIGHT_HOUR, minute: 0 };
  const at = wallToInstant(today, time, tz);
  if (at !== 'gap' && at > nowMs) return { date: today, time };
  // Noon UTC of the next calendar day is that day everywhere a person lives.
  const next = new Date(Date.UTC(today.year, today.month - 1, today.day + 1, 12));
  return {
    date: { year: next.getUTCFullYear(), month: next.getUTCMonth() + 1, day: next.getUTCDate() },
    time,
  };
}

/** What the dialog's time fields add up to. */
export type WindowTimes =
  | { kind: 'incomplete' }
  | { kind: 'invalid'; field: 'startDate' | 'startTime' | 'endDate' | 'endTime' }
  | { kind: 'gap'; field: 'start' | 'end' }
  | { kind: 'backwards' }
  | { kind: 'ok'; startMs: number; endMs: number };

export interface WindowTimeInput {
  startDate: string;
  startTime: string;
  endDate: string;
  endTime: string;
  zone: string;
}

/** Read the dialog's fields into a start and an end instant, or say which field is wrong. */
export function windowTimes(input: WindowTimeInput): WindowTimes {
  const sd = parseDateText(input.startDate);
  const st = parseTimeText(input.startTime);
  if (sd === 'invalid') return { kind: 'invalid', field: 'startDate' };
  if (st === 'invalid') return { kind: 'invalid', field: 'startTime' };
  if (sd === null || st === null) return { kind: 'incomplete' };
  const start = wallToInstant(sd, st, input.zone);
  if (start === 'gap') return { kind: 'gap', field: 'start' };
  const ed = parseDateText(input.endDate);
  const et = parseTimeText(input.endTime);
  if (ed === 'invalid') return { kind: 'invalid', field: 'endDate' };
  if (et === 'invalid') return { kind: 'invalid', field: 'endTime' };
  if (ed === null || et === null) return { kind: 'incomplete' };
  const end = wallToInstant(ed, et, input.zone);
  if (end === 'gap') return { kind: 'gap', field: 'end' };
  if (end <= start) return { kind: 'backwards' };
  return { kind: 'ok', startMs: start, endMs: end };
}

/** The weekday names the date line uses. English codes, like the zone abbreviation beside them. */
const WEEKDAYS = ['Sun', 'Mon', 'Tue', 'Wed', 'Thu', 'Fri', 'Sat'] as const;

/** `2026-11-21 Sat 00:00 EST` - an instant on a clock in `tz`. */
export function wallLine(ms: number, tz: string): string {
  const w = wallClock(ms, tz);
  const date = dateText({ year: w.year, month: w.month, day: w.day });
  return `${date} ${WEEKDAYS[w.weekday]} ${timeText(w)} ${zoneAbbreviation(tz, ms)}`;
}

/** The weekday of a typed date, shown beside the box (`Fri`), or `''` for an unreadable one. */
export function weekdayOf(text: string): string {
  const d = parseDateText(text);
  if (d === null || d === 'invalid') return '';
  return WEEKDAYS[new Date(Date.UTC(d.year, d.month - 1, d.day)).getUTCDay()];
}

/** `11-21 Sat 12:00 – 14:00 JST`, or with both dates when the range crosses a day in `tz`. */
export function rangeLine(startMs: number, endMs: number, tz: string): string {
  const a = wallClock(startMs, tz);
  const b = wallClock(endMs, tz);
  const day = (w: typeof a) => `${pad2(w.month)}-${pad2(w.day)} ${WEEKDAYS[w.weekday]}`;
  const sameDay = a.year === b.year && a.month === b.month && a.day === b.day;
  const end = sameDay ? timeText(b) : `${day(b)} ${timeText(b)}`;
  return `${day(a)} ${timeText(a)} – ${end} ${zoneAbbreviation(tz, startMs)}`;
}

/** The clock a window was asked for in, beside the viewer's own times in a list (ADR-261 decision
 *  10): `22:00–00:00 EST`. `null` when the window recorded no zone, or the viewer's zone is it. */
export function requesterClock(
  startsAt: string,
  endsAt: string,
  zone: string | null | undefined,
  viewer: string,
): string | null {
  if (!zone || zone === viewer) return null;
  const start = Date.parse(startsAt);
  const end = Date.parse(endsAt);
  if (!Number.isFinite(start) || !Number.isFinite(end)) return null;
  return `${timeText(wallClock(start, zone))}–${timeText(wallClock(end, zone))} ${zoneAbbreviation(zone, start)}`;
}

// ── Zones ──────────────────────────────────────────────────────────────────────────────────

/** How many zones "Recent" remembers. */
export const RECENT_ZONES_MAX = 5;

/** `zone` moved to the front of `recent`, capped. */
export function pushRecentZone(recent: readonly string[], zone: string): string[] {
  return [zone, ...recent.filter((z) => z !== zone)].slice(0, RECENT_ZONES_MAX);
}

/** A typed offset: `utc+5`, `UTC-03:30`, `gmt+9`, `+0900`, `-5`. Returns `+HHMM` as
 *  `utcOffsetText` writes it, or `null` when the query is not an offset. */
export function offsetQuery(query: string): string | null {
  const m = /^(?:utc|gmt)?\s*([+-])(\d{1,2})(?::?(\d{2}))?$/i.exec(query.trim());
  if (!m) return null;
  const hours = Number(m[2]);
  const minutes = Number(m[3] ?? 0);
  if (hours > 14 || minutes > 59) return null;
  return `${m[1]}${pad2(hours)}${pad2(minutes)}`;
}

/** The zones a search names, in list order: by offset when the query is one (`utc+5`), else every
 *  zone whose name contains each word (`new york` finds `America/New_York`). An empty query is
 *  every zone. */
export function matchZones(query: string, zones: readonly string[], nowMs: number): string[] {
  const q = query.trim();
  if (!q) return [...zones];
  const offset = offsetQuery(q);
  if (offset) return zones.filter((z) => utcOffsetText(nowMs, z) === offset);
  const words = q.toLowerCase().split(/[\s/_]+/).filter(Boolean);
  return zones.filter((z) => {
    const name = z.toLowerCase().replace(/_/g, ' ');
    return words.every((w) => name.includes(w));
  });
}

/** The zone the window's target keeps time in (decision 6), or `null` when it has none or when
 *  its nodes sit in folders that disagree. `zoneOfGroup` answers a folder's effective zone. */
export function targetZone(
  groupIds: readonly (string | null | undefined)[],
  zoneOfGroup: (groupId: string) => string | null | undefined,
): string | null {
  let found: string | null = null;
  for (const id of groupIds) {
    const z = (id && zoneOfGroup(id)) || null;
    if (!z) return null;
    if (found && found !== z) return null;
    found = z;
  }
  return found;
}

/** The name a window is saved under: what the operator typed, or the default when they typed
 *  nothing (ADR-261 decision 2). */
export function windowName(typed: string, fallback: string): string {
  return typed.trim() || fallback;
}

// ── Stepping with ▲▼ (ADR-261 Inc.6) ───────────────────────────────────────────────────────
// What one press of a box's ▲ (or ↑) and ▼ (or ↓) does. Every step is wall-clock arithmetic:
// `23:00` and one hour later is `00:00` on the next day, whatever the zone does that night - a
// time the clocks skip is then refused by `windowTimes`, as a typed one is.

export type StepDir = 1 | -1;
export type TimeUnit = 'hour' | 'minute';
export type StepUnit = 'day' | TimeUnit;
export interface WallText {
  date: string;
  time: string;
}
export interface Wall {
  date: DateParts;
  time: TimeParts;
}

/** How far a minute press moves: to the next mark of this many minutes in its direction. */
export const MINUTE_STEP = 15;
const DAY_MINUTES = 24 * 60;

const readable = <T>(v: T | null | 'invalid'): v is T => v !== null && v !== 'invalid';

/** A wall date and time moved by `minutes`, the date following across midnight. */
export function addWallMinutes(w: Wall, minutes: number): Wall {
  const x = new Date(
    Date.UTC(w.date.year, w.date.month - 1, w.date.day, w.time.hour, w.time.minute + minutes),
  );
  return {
    date: { year: x.getUTCFullYear(), month: x.getUTCMonth() + 1, day: x.getUTCDate() },
    time: { hour: x.getUTCHours(), minute: x.getUTCMinutes() },
  };
}

/** The minutes one press moves `time` by: an hour keeps the minutes, a minute press lands on the
 *  next `MINUTE_STEP` mark in its direction (`22:10` up is `22:15`, down is `22:00`). */
export function stepMinutes(time: TimeParts, unit: TimeUnit, dir: StepDir): number {
  if (unit === 'hour') return dir * 60;
  const m = time.hour * 60 + time.minute;
  const mark =
    dir > 0
      ? (Math.floor(m / MINUTE_STEP) + 1) * MINUTE_STEP
      : (Math.ceil(m / MINUTE_STEP) - 1) * MINUTE_STEP;
  return mark - m;
}

/** A date box alone: a day later or earlier. A box that cannot be read takes `base` and does not
 *  move (decision 37). */
export function stepDateText(text: string, dir: StepDir, base: DateParts): string {
  const d = parseDateText(text);
  if (!readable(d)) return dateText(base);
  return dateText(addWallMinutes({ date: d, time: { hour: 0, minute: 0 } }, dir * DAY_MINUTES).date);
}

/** A time box with no date beside it (`TimeOfDayInput`): it goes round within its day, so `23:00`
 *  and one hour later is `00:00` (decision 36). An unreadable box takes `base`. */
export function stepTimeOfDayText(
  text: string,
  unit: TimeUnit,
  dir: StepDir,
  base: TimeParts,
): string {
  const t = parseTimeText(text);
  if (!readable(t)) return timeText(base);
  const raw = t.hour * 60 + t.minute + stepMinutes(t, unit, dir);
  const m = ((raw % DAY_MINUTES) + DAY_MINUTES) % DAY_MINUTES;
  return timeText({ hour: Math.floor(m / 60), minute: m % 60 });
}

/** A date box and its time box, stepped as one: a time stepped past midnight carries the date
 *  (decision 36). An unreadable box takes its half of `base` and does not move; a time with no
 *  readable date beside it steps from `base`'s date. */
export function stepWallText(fields: WallText, unit: StepUnit, dir: StepDir, base: Wall): WallText {
  if (unit === 'day') return { date: stepDateText(fields.date, dir, base.date), time: fields.time };
  const d = parseDateText(fields.date);
  const t = parseTimeText(fields.time);
  if (!readable(t)) {
    return { date: readable(d) ? dateText(d) : dateText(base.date), time: timeText(base.time) };
  }
  const from = { date: readable(d) ? d : base.date, time: t };
  const moved = addWallMinutes(from, stepMinutes(t, unit, dir));
  return { date: dateText(moved.date), time: timeText(moved.time) };
}

/** The next whole hour on the clock in `tz` - what an empty start box becomes on its first press
 *  (decision 37). */
export function nextWholeHour(nowMs: number, tz: string): Wall {
  const w = instantToWall(nowMs, tz);
  return addWallMinutes({ date: w.date, time: { hour: w.time.hour, minute: 0 } }, 60);
}

/** What an empty end box becomes on its first press: an hour after the start, or after the next
 *  whole hour while the start cannot be read. Never the start itself, which is no window. */
export function endBase(start: WallText, nowMs: number, tz: string): Wall {
  const d = parseDateText(start.date);
  const t = parseTimeText(start.time);
  const from = readable(d) && readable(t) ? { date: d, time: t } : nextWholeHour(nowMs, tz);
  return addWallMinutes(from, 60);
}

/** Which way an arrow key steps a box, or `null` for any other key. */
export function arrowStep(key: string): StepDir | null {
  if (key === 'ArrowUp') return 1;
  if (key === 'ArrowDown') return -1;
  return null;
}

/** Which way a key press in a box steps it: an arrow, unless an input method is walking its
 *  candidates with it. */
export function keyStep(e: ImeKeyEvent & { key: string }): StepDir | null {
  return isImeComposing(e) ? null : arrowStep(e.key);
}

// ── The two boxes of a time (ADR-261 decision 35, 38) ──────────────────────────────────────

/** The hour (`0`) or minute (`1`) a time box shows: two digits once the time reads, else what was
 *  typed on that side of the colon. */
export function timePart(text: string, part: 0 | 1): string {
  const t = parseTimeText(text);
  if (readable(t)) return pad2(part === 0 ? t.hour : t.minute);
  return text.split(':')[part] ?? '';
}

/** Typing in one of the two boxes: the text that box now shows, and the whole time to hand on.
 *  `whole` is a paste or a fill of several characters at once - `22:00` or `2200` put in the hour
 *  box fills both. Otherwise the box keeps its last two digits, and an hour typed beside an empty
 *  minute brings `00` with it. */
export function typedTimePart(
  raw: string,
  part: 0 | 1,
  current: string,
  whole: boolean,
): { shown: string; text: string } {
  if (whole) {
    const t = parseTimeText(raw);
    if (readable(t)) return { shown: pad2(part === 0 ? t.hour : t.minute), text: timeText(t) };
  }
  const digits = raw.replace(/\D/g, '').slice(-2);
  const parts: [string, string] = [timePart(current, 0), timePart(current, 1)];
  parts[part] = digits;
  if (part === 0 && digits && !parts[1]) parts[1] = '00';
  return { shown: digits, text: !parts[0] && !parts[1] ? '' : `${parts[0]}:${parts[1]}` };
}

/** Whether a box's own text still says the same as the part of the value it shows (`2` and `02`
 *  do), so a value coming back from the caller does not overwrite what is being typed. */
export function samePart(shown: string, part: string): boolean {
  if (shown === part) return true;
  return /^\d+$/.test(shown) && /^\d+$/.test(part) && Number(shown) === Number(part);
}

// ── The end follows the start (ADR-261 decision 33, 34) ────────────────────────────────────

/** The instant a date and a time name in `tz`, or `null` when either cannot be read or the clocks
 *  skip that time. */
export function fieldsInstant(f: WallText, tz: string): number | null {
  const d = parseDateText(f.date);
  const t = parseTimeText(f.time);
  if (!readable(d) || !readable(t)) return null;
  const ms = wallToInstant(d, t, tz);
  return ms === 'gap' ? null : ms;
}

/** The date and time a clock in `tz` reads at `ms`. */
export function instantFields(ms: number, tz: string): WallText {
  const w = instantToWall(ms, tz);
  return { date: dateText(w.date), time: timeText(w.time) };
}

/** The length a window keeps while its start moves: the end minus the start, when both read and
 *  the end is after the start. */
export function keptLength(start: WallText, end: WallText, tz: string): number | null {
  const s = fieldsInstant(start, tz);
  const e = fieldsInstant(end, tz);
  return s != null && e != null && e > s ? e - s : null;
}

/** The end that keeps `length` after the start moved to `start` (decision 34), or `null` to leave
 *  the end alone - no length is kept, or the start does not read yet. */
export function followStart(start: WallText, length: number | null, tz: string): WallText | null {
  if (length == null) return null;
  const s = fieldsInstant(start, tz);
  return s == null ? null : instantFields(s + length, tz);
}

/** The end a shortcut puts in: the start plus `minutes` as time passes, so `+2 h` is two hours of
 *  window even across a clock change. `null` while the start does not read. */
export function endAfter(start: WallText, minutes: number, tz: string): WallText | null {
  const s = fieldsInstant(start, tz);
  return s == null ? null : instantFields(s + minutes * 60_000, tz);
}
