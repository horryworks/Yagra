// SPDX-License-Identifier: AGPL-3.0-only
// Table toolbar — the **action row** above every list. The pieces are small composables so each
// screen arranges the slots it needs; styles live with the table standard (styles/table.css).
//
// **This row no longer holds the filter controls (ADR-053, ADR-228).** A list is narrowed from the
// funnel beside each column header, or from one funnel here when the list has no headers. What is
// left here is what acts on the list: that funnel or the mobile filter button, the chips naming each
// set filter, "clear all filters", a sort control, a spacer, the result count and the primary action —
// in that left-to-right order.
//
// Two things were deleted from this file in Inc.7 rather than kept "just in case":
//
//  - **The search box moved out, and is now `SearchField.tsx`** (ADR-132 folded the picker-only
//    `SearchInput` into the one box every surface uses). Five pickers use it and none of them is a
//    list; leaving it here would have kept the toolbar advertising a search slot that no list may
//    use. Its own header says what it is for now.
//  - **`FilterSelect` is gone entirely.** It was the single-valued toolbar dropdown, and its last
//    three call sites went with Inc.6. Keeping an unused single-select on the shared surface is an
//    invitation to reach for it instead of widening the next endpoint to a set — the exact trade
//    `EnumFilterSpec.single`'s deletion already refused (see `lib/columnFilter.ts`).

import type { ReactNode } from 'react';
import { Trans, useTranslation } from 'react-i18next';

/** The toolbar row. Lay children left → right; drop a <TableSpacer/> before the count/action. */
export function TableToolbar({ children }: { children: ReactNode }) {
  return <div className="table-toolbar">{children}</div>;
}

/** Flexible gap that pushes the result count + primary action to the right. */
export function TableSpacer() {
  return <div className="table-spacer" />;
}

/** Result count: "N of M <noun>" (omit `total` for "N <noun>"), with `shown` emphasized. The
 *  word order comes from the translation key so a language like Japanese can reorder it; callers
 *  pass `noun` already localized for their context. */
export function ResultCount({
  shown,
  total,
  noun,
}: {
  shown: number;
  total?: number;
  noun: string;
}) {
  const { t } = useTranslation('common');
  return (
    <span className="table-count">
      <Trans
        t={t}
        i18nKey={total != null ? 'resultCount' : 'resultCountNoTotal'}
        values={{ shown, total, noun }}
        components={{ b: <strong /> }}
      />
    </span>
  );
}
