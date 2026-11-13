// SPDX-License-Identifier: AGPL-3.0-only
// The "select all" box over a table's rows (ADR-256): empty, a dash for some, a tick for all.
// A press selects every row that can be seen and ticked, or clears everything when all of them
// already are — so one box does both "select all" and "unselect all". The judgement is
// `lib/selectAll.ts`; this file only draws it.

import { useEffect, useRef } from 'react';
import type { ReactNode } from 'react';
import { useTranslation } from 'react-i18next';
import { nextSelection, selectAllState } from '../../lib/selectAll';

interface Props {
  /** Keys of the rows that are on screen and can be ticked. */
  visibleKeys: readonly string[];
  selected: ReadonlySet<string>;
  onChange: (next: Set<string>) => void;
  disabled?: boolean;
  /** Text drawn beside the box (the phone's row above the cards). Without it the box carries
   *  its name for assistive technology only. */
  children?: ReactNode;
  className?: string;
}

export function SelectAllBox({ visibleKeys, selected, onChange, disabled, children, className }: Props) {
  const { t } = useTranslation('common');
  const ref = useRef<HTMLInputElement>(null);
  const state = selectAllState(visibleKeys, selected);
  // `indeterminate` exists only as a DOM property; there is no attribute React could render.
  useEffect(() => {
    if (ref.current) ref.current.indeterminate = state === 'some';
  }, [state]);
  const box = (
    <input
      ref={ref}
      type="checkbox"
      className="select-all-box"
      checked={state === 'all'}
      disabled={disabled || visibleKeys.length === 0}
      aria-label={children ? undefined : t('select.all')}
      aria-checked={state === 'some' ? 'mixed' : state === 'all'}
      onChange={() => onChange(nextSelection(visibleKeys, selected))}
      onClick={(e) => e.stopPropagation()}
    />
  );
  if (!children) return box;
  return (
    <label className={className}>
      {box}
      {children}
    </label>
  );
}
