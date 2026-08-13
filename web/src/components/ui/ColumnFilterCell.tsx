// SPDX-License-Identifier: AGPL-3.0-only
// One cell of the column filter row: a trigger that says what is filtered, and a popover that
// changes it (ADR-053, design-system §4.1).
//
// The trigger's job is the whole point of moving filters out of the toolbar. A toolbar showed every
// active filter as a control with a visible value; collapse that into a popover and the state goes
// invisible unless the closed trigger keeps saying it. So the label is `lib/filterSummary.ts`'s
// descriptor rendered — never the column name.
//
// `FilterBody` is exported because the mobile sheet stacks the SAME bodies inline (a card list has
// no header row to hang a filter row on). Two implementations would drift, and mobile is where
// nobody would notice for a release or two.

import { useCallback, useRef, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { AnchoredPopover, focusPopoverTrigger } from './AnchoredPopover';
import { MultiSelectList } from './MultiSelectList';
import { TextConditionEditor } from './TextConditionEditor';
import {
  CUSTOM_RANGE,
  decodeRange,
  decodeSet,
  defaultValue,
  encodeRange,
  toggleSetValue,
  type ColumnFilterSpec,
  type RangeFilterSpec,
} from '../../lib/columnFilter';
import { decodeCondition, encodeCondition } from '../../lib/filterCondition';
import { summarize, summaryIsActive } from '../../lib/filterSummary';
import './ColumnFilterCell.css';

interface BodyProps<T> {
  spec: ColumnFilterSpec<T>;
  value: string;
  onChange: (next: string) => void;
  /** Facet counts for an enum column. Omitted when this list has none to offer. */
  counts?: Record<string, number>;
  label: string;
  autoFocus?: boolean;
}

/** The editing surface for one column's filter — shared by the desktop popover and the mobile
 *  sheet. Exactly one `switch` over `spec.kind` exists in the UI, and it is this one. */
export function FilterBody<T>({ spec, value, onChange, counts, label, autoFocus }: BodyProps<T>) {
  if (spec.kind === 'enum') {
    const order = spec.options.map((o) => o.value);
    return (
      <MultiSelectList
        options={spec.options}
        selected={decodeSet(value)}
        counts={counts}
        // A `single` column replaces rather than adds, and re-picking the current value clears it —
        // the same behaviour the range kind has always had, for the same reason: the thing behind
        // the control accepts one value. See `EnumFilterSpec.single`.
        onToggle={(v) =>
          onChange(spec.single ? (v === value ? '' : v) : toggleSetValue(value, v, order))
        }
        onClear={() => onChange('')}
        label={label}
      />
    );
  }
  if (spec.kind === 'text') {
    return (
      <TextConditionEditor
        value={decodeCondition(value)}
        onChange={(c) => onChange(encodeCondition(c))}
        modes={spec.modes}
        allowNot={spec.not}
        placeholder={spec.placeholder}
        containsSemantics={spec.containsSemantics}
        autoFocus={autoFocus}
      />
    );
  }
  return <RangeBody spec={spec} value={value} onChange={onChange} label={label} />;
}

/** The range kind's body: the presets, plus the two instants when the custom one is chosen.
 *
 *  Its own component because it needs `useTranslation` for the instant labels, and `FilterBody` is
 *  a plain `switch` that must stay callable from both the popover and the mobile sheet. */
function RangeBody<T>({
  spec,
  value,
  onChange,
  label,
}: {
  spec: RangeFilterSpec<T>;
  value: string;
  onChange: (next: string) => void;
  label: string;
}) {
  const { t } = useTranslation('common');
  const current = decodeRange(value, spec);
  return (
    <div className="dt-f-range">
      <MultiSelectList
        options={spec.presets}
        selected={[current.preset]}
        // A range is single-valued: picking one replaces rather than adds. Re-using the list keeps
        // the popover's look and keyboard behaviour identical across all three kinds.
        onToggle={(v) => onChange(encodeRange({ preset: v, from: '', to: '' }))}
        onClear={() => onChange(spec.defaultPreset)}
        label={label}
      />
      {/* Mounted only under the custom preset, never disabled. A From/To pair on screen but ignored
          by the active preset is the control that makes an operator distrust the whole filter. */}
      {spec.custom && current.preset === CUSTOM_RANGE && (
        <div className="dt-f-instants">
          <input
            className="field dt-f-instant"
            type="datetime-local"
            value={current.from}
            aria-label={t('filter.from')}
            title={t('filter.from')}
            onChange={(e) => onChange(encodeRange({ ...current, from: e.target.value }))}
          />
          <span className="dt-f-instant-sep" aria-hidden="true">
            –
          </span>
          <input
            className="field dt-f-instant"
            type="datetime-local"
            value={current.to}
            aria-label={t('filter.to')}
            title={t('filter.to')}
            onChange={(e) => onChange(encodeRange({ ...current, to: e.target.value }))}
          />
        </div>
      )}
    </div>
  );
}

interface Props<T> {
  spec: ColumnFilterSpec<T>;
  value: string;
  onChange: (next: string) => void;
  counts?: Record<string, number>;
  /** Plain-text column name, for the trigger's accessible name and the clear button's. */
  label: string;
  /** Called when the popover opens. A server-side list fetches its facet counts here and NOT on
   *  load: ADR-023 puts UI load third behind polling and alert evaluation, and five aggregate
   *  queries per page load to decorate checkboxes nobody clicked is how the poller starves. */
  onOpen?: () => void;
  align?: 'right';
}

export function ColumnFilterCell<T>({
  spec,
  value,
  onChange,
  counts,
  label,
  onOpen,
  align,
}: Props<T>) {
  const { t } = useTranslation('common');
  const [open, setOpen] = useState(false);
  const wrapRef = useRef<HTMLDivElement>(null);

  const dismiss = useCallback((restoreFocus: boolean) => {
    setOpen(false);
    if (restoreFocus) focusPopoverTrigger(wrapRef.current, 'dialog');
  }, []);

  const summary = summarize(spec, value);
  const active = summaryIsActive(spec, value);

  /** What the closed trigger reads. `none` is the only case that has to ask the spec — the other
   *  four are fully described by the summary, which is the point of it being a descriptor. */
  const emptyLabel = () => {
    if (spec.kind === 'enum') return spec.allLabel;
    if (spec.kind === 'text') return spec.placeholder ?? t('filter.termPlaceholder');
    return t('filter.termPlaceholder');
  };

  const triggerText = () => {
    switch (summary.kind) {
      case 'none':
        return emptyLabel();
      case 'one':
      case 'preset':
        return summary.label;
      case 'many':
        return `${summary.label} ${t('filter.more', { count: summary.more })}`;
      case 'text':
        return summary.term;
    }
  };

  return (
    <div className={align === 'right' ? 'dt-f right' : 'dt-f'} ref={wrapRef}>
      <button
        type="button"
        className={active ? 'dt-f-trigger on' : 'dt-f-trigger'}
        aria-haspopup="dialog"
        aria-expanded={open}
        aria-label={t('filter.open', { column: label })}
        onClick={() => {
          const next = !open;
          setOpen(next);
          if (next) onOpen?.();
        }}
      >
        {summary.kind === 'text' && summary.not && (
          <span className="dt-f-mark not" aria-hidden="true">
            ≠
          </span>
        )}
        {summary.kind === 'text' && summary.mode === 'regex' && (
          <span className="dt-f-mark" aria-hidden="true">
            .*
          </span>
        )}
        <span className="dt-f-text">{triggerText()}</span>
        <span className="dt-f-caret" aria-hidden="true">
          ▾
        </span>
      </button>
      {/* Outside the trigger, not nested in it: a button inside a button is invalid markup and the
          inner one's click does not reliably reach its own handler. */}
      {active && (
        <button
          type="button"
          className="dt-f-clear"
          aria-label={t('filter.clearAria', { column: label })}
          onClick={() => onChange(defaultValue(spec))}
        >
          ✕
        </button>
      )}
      <AnchoredPopover
        open={open}
        anchorRef={wrapRef}
        role="dialog"
        label={t('filter.open', { column: label })}
        align="start"
        onDismiss={dismiss}
      >
        <FilterBody
          spec={spec}
          value={value}
          onChange={onChange}
          counts={counts}
          label={label}
          autoFocus
        />
      </AnchoredPopover>
    </div>
  );
}
