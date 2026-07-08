// Reusable chart time-range selector — the canonical duration/range picker for the whole app.
// A joined segmented control of presets (1h 6h 24h 3d 7d) followed by a calendar button that opens
// a custom-range popover (quick chips + From/To). Every chart window reuses THIS component rather
// than re-inventing a button row: NodeDetail Device health, the Interfaces dock, Metric explorer.
//
// The range model generalizes the old "seconds back from now" to an explicit window so custom
// (absolute) ranges work. Relative ranges re-resolve `now` on each `resolveRange` call (so a polling
// interval slides the window); absolute ranges hold fixed bounds.

import { useEffect, useRef, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { CalendarIcon } from '../ui/icons';
import './RangeControl.css';

export type Range =
  | { kind: 'relative'; secs: number } // presets and relative quick-picks
  | { kind: 'absolute'; from: number; to: number }; // custom range (unix seconds)

/** Segmented presets — the single source of truth for the chart time-windows. */
export const RANGES: { label: string; secs: number }[] = [
  { label: '1h', secs: 3600 },
  { label: '6h', secs: 6 * 3600 },
  { label: '24h', secs: 24 * 3600 },
  { label: '3d', secs: 3 * 86400 },
  { label: '7d', secs: 7 * 86400 },
];

/** Popover quick-chips — the presets plus a longer 30d relative pick. */
const QUICK_CHIPS: { label: string; secs: number }[] = [...RANGES, { label: '30d', secs: 30 * 86400 }];

/** Default selection used by every consumer's initial state (the first preset, relative). */
export const DEFAULT_RANGE: Range = { kind: 'relative', secs: RANGES[0].secs };

/** Resolve a Range to concrete unix-second bounds. Relative re-reads `now` each call (so a polling
 *  interval naturally advances the window); absolute returns its fixed bounds unchanged. */
export function resolveRange(range: Range): { from: number; to: number } {
  if (range.kind === 'absolute') return { from: range.from, to: range.to };
  const to = Math.floor(Date.now() / 1000);
  return { from: to - range.secs, to };
}

/** Write a Range into URL search params so a reload restores the same window: relative → `r=<secs>`,
 *  absolute → `from=<unix>&to=<unix>`. Clears the other form's keys so the URL stays unambiguous. */
export function writeRangeParams(params: URLSearchParams, range: Range): void {
  if (range.kind === 'relative') {
    params.set('r', String(range.secs));
    params.delete('from');
    params.delete('to');
  } else {
    params.set('from', String(range.from));
    params.set('to', String(range.to));
    params.delete('r');
  }
}

/** Read a Range back from URL search params, or null if absent/invalid (caller falls back to
 *  DEFAULT_RANGE). A valid absolute window (`from`+`to`, from < to) wins over relative (`r`). */
export function readRangeParam(params: URLSearchParams): Range | null {
  const fromStr = params.get('from');
  const toStr = params.get('to');
  if (fromStr != null && toStr != null) {
    const from = Number(fromStr);
    const to = Number(toStr);
    if (Number.isFinite(from) && Number.isFinite(to) && from < to) {
      return { kind: 'absolute', from, to };
    }
  }
  const rStr = params.get('r');
  if (rStr != null) {
    const secs = Number(rStr);
    if (Number.isFinite(secs) && secs > 0) return { kind: 'relative', secs };
  }
  return null;
}

const pad2 = (n: number) => String(n).padStart(2, '0');

/** Compact range readout for the calendar button, e.g. "6/12 12:00–6/17 09:30" (local time). */
// TODO(i18n): M/D HH:MM ordering is hardcoded; route through a formatting layer when localization lands.
export function formatCompactRange(from: number, to: number): string {
  const fmt = (s: number) => {
    const d = new Date(s * 1000);
    return `${d.getMonth() + 1}/${d.getDate()} ${pad2(d.getHours())}:${pad2(d.getMinutes())}`;
  };
  return `${fmt(from)}–${fmt(to)}`;
}

/** unix seconds → 'YYYY-MM-DDTHH:MM' in LOCAL time for an <input type="datetime-local">. Built from
 *  local getters (not toISOString, which is UTC and would shift the value by the timezone offset). */
export function unixToLocalInput(s: number): string {
  const d = new Date(s * 1000);
  return `${d.getFullYear()}-${pad2(d.getMonth() + 1)}-${pad2(d.getDate())}T${pad2(d.getHours())}:${pad2(d.getMinutes())}`;
}

/** 'YYYY-MM-DDTHH:MM' (local wall-clock) → unix seconds, or null if empty/unparseable. `new Date(v)`
 *  with no zone suffix is interpreted as local time, so this round-trips with unixToLocalInput. */
export function localInputToUnix(v: string): number | null {
  if (!v) return null;
  const ms = new Date(v).getTime();
  return Number.isNaN(ms) ? null : Math.floor(ms / 1000);
}

/** Whether the From/To draft inputs form a valid absolute window (both parse, from < to). */
export function rangeInputsValid(fromStr: string, toStr: string): boolean {
  const from = localInputToUnix(fromStr);
  const to = localInputToUnix(toStr);
  return from != null && to != null && from < to;
}

interface Props {
  value: Range;
  onChange: (range: Range) => void;
}

export function RangeControl({ value, onChange }: Props) {
  const { t } = useTranslation();
  const [open, setOpen] = useState(false);
  const [draftFrom, setDraftFrom] = useState('');
  const [draftTo, setDraftTo] = useState('');
  const ref = useRef<HTMLDivElement>(null);

  // Close on outside-click (mousedown, like UserMenu) and Esc (like Modal) — only while open.
  useEffect(() => {
    if (!open) return;
    const onDown = (e: MouseEvent) => {
      if (ref.current && !ref.current.contains(e.target as Node)) setOpen(false);
    };
    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape') setOpen(false);
    };
    document.addEventListener('mousedown', onDown);
    document.addEventListener('keydown', onKey);
    return () => {
      document.removeEventListener('mousedown', onDown);
      document.removeEventListener('keydown', onKey);
    };
  }, [open]);

  // Picking a preset/chip commits a relative range and closes the popover.
  const selectPreset = (secs: number) => {
    onChange({ kind: 'relative', secs });
    setOpen(false);
  };

  const togglePopover = () => {
    if (open) {
      setOpen(false);
      return;
    }
    // Seed the From/To inputs from the current window so the popover opens on a sensible range.
    const { from, to } = resolveRange(value);
    setDraftFrom(unixToLocalInput(from));
    setDraftTo(unixToLocalInput(to));
    setOpen(true);
  };

  const applyEnabled = rangeInputsValid(draftFrom, draftTo);
  const apply = () => {
    const from = localInputToUnix(draftFrom);
    const to = localInputToUnix(draftTo);
    if (from == null || to == null || from >= to) return; // guarded; Apply is disabled when invalid
    onChange({ kind: 'absolute', from, to });
    setOpen(false);
  };

  return (
    <div className="rc" ref={ref}>
      <div className="rc-seg" role="group" aria-label={t('range.timeRange')}>
        {RANGES.map((r) => {
          const active = value.kind === 'relative' && value.secs === r.secs;
          return (
            <button
              key={r.secs}
              type="button"
              className={`rc-seg-btn${active ? ' active' : ''}`}
              aria-pressed={active}
              onClick={() => selectPreset(r.secs)}
            >
              {r.label}
            </button>
          );
        })}
      </div>

      <button
        type="button"
        className={`rc-cal${value.kind === 'absolute' ? ' active' : ''}`}
        aria-label={t('range.custom')}
        aria-expanded={open}
        title={t('range.custom')}
        onClick={togglePopover}
      >
        <CalendarIcon className="rc-cal-icon" width={14} height={14} />
        {value.kind === 'absolute' && (
          <span className="rc-cal-readout">{formatCompactRange(value.from, value.to)}</span>
        )}
      </button>

      {open && (
        <div className="rc-pop" role="dialog" aria-label={t('range.custom')}>
          <div className="rc-pop-sec">
            <div className="rc-pop-label">{t('range.quick')}</div>
            <div className="rc-chips">
              {QUICK_CHIPS.map((c) => {
                const active = value.kind === 'relative' && value.secs === c.secs;
                return (
                  <button
                    key={c.secs}
                    type="button"
                    className={`rc-chip${active ? ' active' : ''}`}
                    onClick={() => selectPreset(c.secs)}
                  >
                    {c.label}
                  </button>
                );
              })}
            </div>
          </div>

          <label className="rc-field">
            <span className="rc-field-label">{t('range.from')}</span>
            <input
              className="rc-input"
              type="datetime-local"
              value={draftFrom}
              onChange={(e) => setDraftFrom(e.target.value)}
            />
          </label>
          <label className="rc-field">
            <span className="rc-field-label">{t('range.to')}</span>
            <input
              className="rc-input"
              type="datetime-local"
              value={draftTo}
              onChange={(e) => setDraftTo(e.target.value)}
            />
          </label>

          <div className="rc-pop-foot">
            <button type="button" className="rc-btn" onClick={() => setOpen(false)}>
              {t('actions.cancel')}
            </button>
            <button
              type="button"
              className="rc-btn rc-btn-primary"
              disabled={!applyEnabled}
              onClick={apply}
            >
              {t('actions.apply')}
            </button>
          </div>
        </div>
      )}
    </div>
  );
}
