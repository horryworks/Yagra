// SPDX-License-Identifier: AGPL-3.0-only
// The editor of one column's filter (ADR-053, design-system §4.1): what opens from a header's
// funnel (`HeaderFilter`) and what the mobile sheet and the toolbar funnel stack (`FilterSections`).
//
// Until ADR-228 this file also held the filter row's cell — a trigger that said what was filtered.
// That job moved to the toolbar's chips; the editor stayed here, one implementation for every
// surface, because two would drift and mobile is where nobody would notice for a release or two.

import { useEffect, useLayoutEffect, useRef, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { MultiSelectList } from './MultiSelectList';
import { SearchField } from './SearchField';
import { TextConditionEditor } from './TextConditionEditor';
import { useDebouncedValue } from '../../lib/useDebouncedValue';
import {
  CUSTOM_RANGE,
  decodeNumberRange,
  decodeRange,
  decodeSet,
  encodeNumberRange,
  encodeRange,
  normalizeValues,
  toggleSetValue,
  type ColumnFilterSpec,
  type NumberFilterSpec,
  type RangeFilterSpec,
  type ValuesFilterSpec,
} from '../../lib/columnFilter';
import { decodeCondition, encodeCondition } from '../../lib/filterCondition';
import './ColumnFilterCell.css';

interface BodyProps<T> {
  spec: ColumnFilterSpec<T>;
  value: string;
  onChange: (next: string) => void;
  /** Facet counts for an enum column. Omitted when this list has none to offer. */
  counts?: Record<string, number>;
  label: string;
  /** Move focus into the body's text entry, if it has one.
   *
   *  ⚠️ **Not React's `autoFocus`, and it must not become it again.** That attribute fires when the
   *  element mounts, which here is while `AnchoredPopover` still has the panel `visibility: hidden`
   *  so it can measure it — and a hidden element cannot take focus, so the call was a silent no-op
   *  for three increments and every filter needed a second click before it would accept a keystroke.
   *  This is a *signal* instead: false on mount, flipped true by `onPlacedChange` once the panel is
   *  placed and visible, which each body watches. The mobile sheet passes nothing — it stacks every
   *  column, so no single box has a claim on the caret, and taking it would raise the on-screen
   *  keyboard over the list the operator just opened. */
  takeFocus?: boolean;
}

/** The editing surface for one column's filter — shared by the desktop popover and the mobile
 *  sheet. Exactly one `switch` over `spec.kind` exists in the UI, and it is this one.
 *
 *  **Where the caret lands, per kind: the body's text entry if it has one, otherwise nowhere.**
 *  `range` has only presets and a short `enum` has only options, so on those the trigger keeps
 *  focus — picking an option for the operator would be a choice, not a convenience.
 *
 *  `spec.hint` renders here rather than at either call site, so the popover and the sheet cannot
 *  say different things (ADR-055 R6). */
export function FilterBody<T>(props: BodyProps<T>) {
  if (!props.spec.hint) return <FilterEditor {...props} />;
  return (
    <>
      <p className="dt-f-hint">{props.spec.hint}</p>
      <FilterEditor {...props} />
    </>
  );
}

function FilterEditor<T>({ spec, value, onChange, counts, label, takeFocus }: BodyProps<T>) {
  if (spec.kind === 'enum') {
    const order = spec.options.map((o) => o.value);
    return (
      <MultiSelectList
        options={spec.options}
        selected={decodeSet(value)}
        counts={counts}
        // Always a set. The `spec.single` branch that used to sit here (pick replaces rather than
        // adds) is gone with the flag — every endpoint behind an enum column now accepts a list.
        onToggle={(v) => onChange(toggleSetValue(value, v, order))}
        onClear={() => onChange('')}
        label={label}
        takeFocus={takeFocus}
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
        takeFocus={takeFocus}
      />
    );
  }
  if (spec.kind === 'values') {
    return <ValuesBody spec={spec} value={value} onChange={onChange} takeFocus={takeFocus} />;
  }
  if (spec.kind === 'number') {
    return <NumberBody spec={spec} value={value} onChange={onChange} takeFocus={takeFocus} />;
  }
  return <RangeBody spec={spec} value={value} onChange={onChange} label={label} />;
}

/** The `values` kind's body: one box, comma-separated, normalized on settle.
 *
 *  **Debounced, unlike `NumberBody`** — and for the reason that comment gives for *not* debouncing
 *  there. A partial value here is a *different, valid, more expensive* query: typing `10.0.0.1`
 *  passes through `10`, `10.0`, `10.0.0` — each a well-formed set that would re-run six ClickHouse
 *  aggregations. 350ms is what the tab's hand-rolled inputs already used for exactly this.
 *
 *  ⚠️ The effect depends on `draft` **only**. `value` comes from state that this component's own
 *  commit changes, and `onChange` is an inline closure with a new identity every render; either in
 *  the dependency list makes the input never settle (`TextConditionEditor` carries the same note,
 *  and the bug it describes shipped once). */
function ValuesBody<T>({
  spec,
  value,
  onChange,
  takeFocus,
}: {
  spec: ValuesFilterSpec<T>;
  value: string;
  onChange: (next: string) => void;
  takeFocus?: boolean;
}) {
  const { t } = useTranslation('common');
  const [draft, setDraft] = useState(value);
  const settled = useDebouncedValue(draft, 350);
  const commit = useRef(onChange);
  commit.current = onChange;
  useEffect(() => {
    commit.current(normalizeValues(settled, spec.parse, spec.max));
    // eslint-disable-next-line react-hooks/exhaustive-deps -- see the note above: only `draft`.
  }, [settled]);
  const inputRef = useRef<HTMLInputElement>(null);
  useLayoutEffect(() => {
    if (takeFocus) inputRef.current?.focus({ preventScroll: true });
  }, [takeFocus]);
  return (
    <SearchField
      inputRef={inputRef}
      className="dt-f-values"
      value={draft}
      placeholder={spec.placeholder ?? t('filter.valuesPlaceholder')}
      aria-label={spec.placeholder ?? t('filter.valuesPlaceholder')}
      onChange={(e) => setDraft(e.target.value)}
      // Past the debounce on purpose: the set is empty the moment the ✕ is pressed, and waiting
      // 350ms to say so would leave the list narrowed by a value no longer on screen.
      onClear={() => {
        setDraft('');
        commit.current('');
      }}
    />
  );
}

/** The number kind's body: two bounds, either of which may be left blank.
 *
 *  No debounce, unlike `TextConditionEditor`. A number field commits whole values — there is no
 *  intermediate keystroke that is a *different, valid, more expensive* query the way `1` is on the
 *  way to `10` in a text search — and the one consumer today re-queries the server, so debouncing
 *  would only delay a query the operator has finished typing. If a future column proves otherwise,
 *  debounce there, not here. */
function NumberBody<T>({
  spec,
  value,
  onChange,
  takeFocus,
}: {
  spec: NumberFilterSpec<T>;
  value: string;
  onChange: (next: string) => void;
  takeFocus?: boolean;
}) {
  const { t } = useTranslation('common');
  const current = decodeNumberRange(value);
  // The lower bound, because an interval is read left to right — and it is the one an operator
  // types alone ("score 8 or worse" is one bound, not a window).
  const minRef = useRef<HTMLInputElement>(null);
  useLayoutEffect(() => {
    if (takeFocus) minRef.current?.focus({ preventScroll: true });
  }, [takeFocus]);
  // `''` and `0` must stay distinguishable: `Number('')` is 0, so a blank field read through the
  // usual `Number(e.target.value) || null` would silently become a bound of zero.
  const set = (side: 'min' | 'max') => (raw: string) => {
    const n = raw.trim() === '' ? null : Number(raw);
    onChange(encodeNumberRange({ ...current, [side]: Number.isFinite(n) ? n : null }));
  };
  const field = (side: 'min' | 'max', v: number | null, aria: string) => (
    <span className="dt-f-numfield">
      <input
        ref={side === 'min' ? minRef : undefined}
        className="field dt-f-num"
        type="number"
        inputMode="decimal"
        value={v ?? ''}
        min={spec.min}
        max={spec.max}
        step={spec.step}
        aria-label={aria}
        title={aria}
        onChange={(e) => set(side)(e.target.value)}
      />
      {spec.unit && <span className="dt-f-numunit">{spec.unit}</span>}
    </span>
  );
  return (
    <div className="dt-f-numrange">
      {field('min', current.min, t('filter.atLeast'))}
      <span className="dt-f-instant-sep" aria-hidden="true">
        –
      </span>
      {field('max', current.max, t('filter.atMost'))}
    </div>
  );
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
