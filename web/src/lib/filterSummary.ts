// SPDX-License-Identifier: AGPL-3.0-only
// What a column's filter trigger says (ADR-053 decision 1).
//
// Returns a **descriptor, not a string**. The trigger sits in a grid track that can be 90px wide,
// so the wording is a layout decision as much as a copy one, and it needs EN/JA — neither of which
// belongs in a module that has to be unit-testable. The component turns a descriptor into text; the
// judgement about *which* descriptor lives here, where tests run.
//
// The reason a summary exists at all: the filter row replaces a toolbar where every active filter
// was a visible control with a visible value. Collapse that into a popover and the state goes
// invisible unless the closed trigger keeps saying it. A trigger that reads "Kind" when three kinds
// are selected has lost the thing the redesign was for.

import {
  decodeNumberRange,
  decodeRange,
  decodeSet,
  type ColumnFilterSpec,
  type FilterOption,
  type TextMode,
} from './columnFilter';
import { decodeCondition, conditionIsActive } from './filterCondition';

export type FilterSummary =
  /** Nothing selected — the caller renders `allLabel` (enum) or the placeholder (text). */
  | { kind: 'none' }
  /** Exactly one option: show its label. */
  | { kind: 'one'; label: string }
  /** Several: show the first and how many more, so the trigger's width does not depend on the
   *  selection. "syslog +2", never "syslog, trap, webhook". */
  | { kind: 'many'; label: string; more: number }
  /** A text condition, decomposed so the trigger can mark negation and mode without re-parsing. */
  | { kind: 'text'; term: string; mode: TextMode; not: boolean }
  /** A range preset. */
  | { kind: 'preset'; label: string }
  /** A numeric interval, either end possibly open. The component words it (`≥ 8`, `3 – 5`) because
   *  which of the three readings applies is a copy decision that needs EN/JA. */
  | { kind: 'number'; min: number | null; max: number | null };

function labelOf(options: readonly FilterOption[], value: string): string {
  return options.find((o) => o.value === value)?.label ?? value;
}

/** Summarize one column's current filter value. */
export function summarize<T>(spec: ColumnFilterSpec<T>, raw: string): FilterSummary {
  if (spec.kind === 'enum') {
    const sel = decodeSet(raw);
    if (sel.length === 0) return { kind: 'none' };
    if (sel.length === 1) return { kind: 'one', label: labelOf(spec.options, sel[0]) };
    return { kind: 'many', label: labelOf(spec.options, sel[0]), more: sel.length - 1 };
  }
  if (spec.kind === 'text') {
    const c = decodeCondition(raw);
    if (!conditionIsActive(c)) return { kind: 'none' };
    return { kind: 'text', term: c.term, mode: c.mode, not: c.not };
  }
  if (spec.kind === 'values') {
    // Same descriptors as `enum` — the trigger reads "443 +1" either way, and the operator has no
    // reason to care that one set came from a list and the other from typing. `format` is what
    // turns a stored token back into what they saw (`6` → `TCP`).
    const sel = decodeSet(raw);
    const label = (v: string) => spec.format?.(v) ?? v;
    if (sel.length === 0) return { kind: 'none' };
    if (sel.length === 1) return { kind: 'one', label: label(sel[0]) };
    return { kind: 'many', label: label(sel[0]), more: sel.length - 1 };
  }
  if (spec.kind === 'number') {
    const { min, max } = decodeNumberRange(raw);
    if (min === null && max === null) return { kind: 'none' };
    return { kind: 'number', min, max };
  }
  // A range always shows its window, even at the default. The default is 24h, which *narrows* —
  // a trigger reading "All time" or nothing at all while a day-long window is hiding rows is the
  // same lie the empty state had to be fixed for.
  //
  // Decoded rather than read raw: a custom window's value carries its two instants, so matching the
  // preset list against the raw string would fall through to `?? value` and render
  // `custom:2026-08-01T00:00|…` inside a 90px trigger.
  return { kind: 'preset', label: labelOf(spec.presets, decodeRange(raw, spec).preset) };
}

/** Whether the trigger should render in its "active" style (accent border + clear button). Note
 *  this is NOT `summary.kind !== 'none'`: a range at its default summarizes to a preset label but
 *  is not something the operator set, so it must not offer a clear button that does nothing. */
export function summaryIsActive<T>(spec: ColumnFilterSpec<T>, raw: string): boolean {
  if (spec.kind === 'range') return raw !== '' && raw !== spec.defaultPreset;
  return summarize(spec, raw).kind !== 'none';
}
