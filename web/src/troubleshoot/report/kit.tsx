// SPDX-License-Identifier: AGPL-3.0-only
// Shared widgets the report bodies compose from. These carry NO tool semantics — they are the
// grid/chrome only, with every content slot passed in, so each body stays free to say something
// different (the 1:1 requirement) without re-implementing the layout.
//
// Reuses the existing `.ts-anom*` / `.ts-chip*` / `.ts-res-*` CSS from troubleshoot.css unchanged;
// only genuinely new classes live in report.css.

import type { ReactNode } from 'react';
import { useTranslation } from 'react-i18next';
import { Select } from '../../components/ui/Field';
import { EntityName } from '../../components/ui/EntityName';
import type { AnalysisFinding } from '../../types/api';
import { sevOf } from './format';

/**
 * One finding row on the shared `.ts-anom` grid: score · identity · a viz slot · a right rail.
 * Bodies fill the slots; the grid, severity border and mobile collapse come free.
 */
export function FindingRow({
  finding,
  identity,
  viz,
  right,
}: {
  finding: AnalysisFinding;
  identity: ReactNode;
  viz?: ReactNode;
  right?: ReactNode;
}) {
  const { t } = useTranslation('troubleshoot');
  return (
    <div className={`ts-anom sev-${sevOf(finding)}`}>
      <div className="ts-anom-score">
        <span className="ts-anom-score-num">{Math.round(finding.score)}</span>
        <span className="ts-anom-score-cap">{t('report.common.score')}</span>
      </div>
      <div className="ts-anom-id">{identity}</div>
      <div className="ts-anom-chart">{viz ?? null}</div>
      <div className="ts-anom-right">{right ?? null}</div>
    </div>
  );
}

/**
 * The node line of a finding's identity. Always routed through `EntityName` so a `node_name` that
 * the backend could not resolve (it falls back to the raw uuid) renders as a mono handle with a
 * copy affordance rather than as prose — the no-raw-UUIDs rule.
 */
export function NodeRef({ finding }: { finding: AnalysisFinding }) {
  return (
    <span className="ts-anom-node">
      <EntityName name={finding.node_name} id={finding.node_id ?? undefined} />
    </span>
  );
}

/** A mono secondary line (IPs, signatures, metric names — never a proportional font). */
export function MonoLine({ children, title }: { children: ReactNode; title?: string }) {
  return (
    <span className="ts-anom-metric mono" title={title}>
      {children}
    </span>
  );
}

/** A coloured kind/category dot + label (categorical — carries no status meaning). */
export function KindTag({ color, label }: { color: string; label: string }) {
  return (
    <span className="ts-anom-kind">
      <span className="ts-anom-kind-dot" style={{ background: color }} />
      {label}
    </span>
  );
}

/** The right rail: a primary line plus a muted secondary. */
export function RightRail({ when, detail }: { when?: ReactNode; detail?: ReactNode }) {
  return (
    <>
      {when != null && <span className="ts-anom-when">{when}</span>}
      {detail != null && <span className="ts-anom-dur">{detail}</span>}
    </>
  );
}

/**
 * A baseline-vs-peak meter: the learned baseline as a muted segment, the overshoot beyond it in the
 * severity colour, labelled with the multiple (`×12.4`). Shared by `event_storm` and
 * `traffic_anomaly` — their details are the same shape modulo units, so `format` swaps the value
 * formatter rather than the widget. `null` baseline (or 0) means "no baseline to beat", so the whole
 * track reads as overshoot rather than dividing by zero.
 */
export function RatioMeter({
  peak,
  baseline,
  severity,
  format,
}: {
  peak: number;
  baseline: number;
  severity: 'crit' | 'warn' | 'info';
  format: (v: number) => string;
}) {
  const ratio = baseline > 0 ? peak / baseline : Infinity;
  // Baseline share of the peak — the rest of the track is the excess that made this a finding.
  const basePct = baseline > 0 && peak > 0 ? Math.min(100, (baseline / peak) * 100) : 0;
  const label = Number.isFinite(ratio) ? `×${ratio.toFixed(1)}` : format(peak);
  return (
    <div className="tsr-ratio" title={`${format(peak)} / ${format(baseline)}`}>
      <div className="tsr-meter">
        <div className={`tsr-meter-fill tsr-ratio-excess ${severity}`} style={{ width: '100%' }} />
        <div className="tsr-ratio-base" style={{ width: `${basePct}%` }} />
      </div>
      <span className="tsr-ratio-val mono">{label}</span>
    </div>
  );
}

/**
 * A before→after pair on one shared 0–100% scale: the baseline track above (muted) and the recent
 * one below (severity-coloured). Reading them as two bars on the same scale is what makes a shift
 * legible — two percentages side by side do not show it.
 */
export function TwinTrack({
  basePct,
  recentPct,
  severity,
  label,
}: {
  basePct: number;
  recentPct: number;
  severity: 'crit' | 'warn' | 'info';
  label: string;
}) {
  const clamp = (v: number) => Math.max(0, Math.min(100, v));
  return (
    <div className="tsr-twin" title={label}>
      <div className="tsr-twin-track">
        <div className="tsr-twin-fill base" style={{ width: `${clamp(basePct)}%` }} />
      </div>
      <div className="tsr-twin-track">
        <div className={`tsr-twin-fill ${severity}`} style={{ width: `${clamp(recentPct)}%` }} />
      </div>
      <span className="tsr-twin-val mono">{label}</span>
    </div>
  );
}

export interface ChipOption<T extends string> {
  value: T;
  label: string;
  /** Optional categorical dot. */
  color?: string;
}

/** A filter chip row. */
export function Chips<T extends string>({
  options,
  value,
  onChange,
}: {
  options: ChipOption<T>[];
  value: T;
  onChange: (v: T) => void;
}) {
  return (
    <div className="ts-chip-row">
      {options.map((o) => (
        <button
          key={o.value}
          type="button"
          className={value === o.value ? 'ts-chip on' : 'ts-chip'}
          aria-pressed={value === o.value}
          onClick={() => onChange(o.value)}
        >
          {o.color && <span className="ts-chip-dot" style={{ background: o.color }} />}
          {o.label}
        </button>
      ))}
    </div>
  );
}

/** The results toolbar: a filter area on the left, a sort control on the right. */
export function ReportToolbar({
  id,
  filters,
  sortOptions,
  sort,
  onSort,
  extra,
}: {
  id: string;
  filters?: ReactNode;
  sortOptions: { value: string; label: string }[];
  sort: string;
  onSort: (v: string) => void;
  /** Anything between the filters and the sort control (a search box, a group-by toggle). */
  extra?: ReactNode;
}) {
  const { t } = useTranslation('troubleshoot');
  return (
    <div className="ts-res-toolbar">
      {filters}
      {extra}
      <div className="ts-cfgbar-spacer" />
      <label className="ts-sort-label" htmlFor={`${id}-sort`}>
        {t('report.common.sort.label')}
      </label>
      <Select id={`${id}-sort`} value={sort} onChange={(e) => onSort(e.target.value)}>
        {sortOptions.map((o) => (
          <option key={o.value} value={o.value}>
            {o.label}
          </option>
        ))}
      </Select>
    </div>
  );
}

/**
 * A tier-unavailable notice. The backend emits a synthetic `kind: 'info'` finding (`node_id: null`,
 * `node_name: "—"`, empty labels, `detail: {note}`) whenever a flow analysis runs with the flow tier
 * off. The shell splits those out and renders them here, so no report shows a broken row or counts
 * one in its totals.
 */
export function NoticeRow({ notices }: { notices: AnalysisFinding[] }) {
  const { t } = useTranslation('troubleshoot');
  if (!notices.length) return null;
  const note = (notices[0].detail as { note?: string } | null | undefined)?.note;
  return (
    <div className="tsr-notice">
      <span className="tsr-notice-mark" aria-hidden="true">
        !
      </span>
      <span>{note ? t('report.common.notice', { reason: note }) : t('report.common.noticePlain')}</span>
    </div>
  );
}

/** The list-level empty note: distinguishes "nothing found" from "your filter hid everything". */
export function EmptyList({ total }: { total: number }) {
  const { t } = useTranslation('troubleshoot');
  return (
    <div className="ts-empty-note">
      {total ? t('report.common.list.emptyFiltered') : t('report.common.list.emptyAll')}
    </div>
  );
}
