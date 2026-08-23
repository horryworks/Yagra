// SPDX-License-Identifier: AGPL-3.0-only
// Row actions are on screen when the row is hovered (ADR-088 × ADR-052).
//
// WHY THIS FILE EXISTS. `styles/table.css` holds the edit/delete icons at `opacity: 0` and reveals
// them on row hover. Every reveal rule named `.ytable-row`, and a `DataTable` row is `.dt-row` — so
// on **ten screens at once** the icons were permanently transparent, reachable only under
// `(hover: none)`. It was found by a person looking at a screenshot.
//
// 🚨 **`isVisible()` cannot see this and never could.** Playwright counts an `opacity: 0` element
// as visible — it checks `display`, `visibility` and box size — so a browser test clicks a control
// no human can find, and passes. The computed opacity is the only witness. `table.css` says so at
// the declaration, `thresholdRules.spec.ts` proved it on one screen, and this is that check moved
// to where the defect actually lived: all of them.
//
// It runs inside the route walk, on the visit the walk already makes. A screen with no row actions
// is simply not a subject, so there is no list here of which screens have them — which matters,
// because the defect's whole shape was "ten screens at once from one shared stylesheet". A list
// would have had to name all ten to catch it.

import type { Page } from '@playwright/test';

/** Rows that carry a hover-revealed action group, in all three row flavours the app renders.
 *
 *  ⚠️ **All three, every time.** `.ytable-row` is the hand-rolled tables, `.dt-row` is `DataTable`,
 *  `.il-row` is the interface list — and naming two of the three is precisely the bug above.
 *  `ui-conventions.md` repeats this at the CSS rule; it is repeated here because a selector in a
 *  test drifts from a selector in a stylesheet exactly as easily. */
const ROWS_WITH_ACTIONS =
  '.dt-row:has(.ytable-actions), .ytable-row:has(.ytable-actions), .il-row:has(.il-actions)';

const ACTION_GROUP = '.ytable-actions, .il-actions';

/** Anything below this is not "revealed": the transition is 0.15s and settles well inside the
 *  poll, so a value between the two states means the reveal did not happen rather than that it is
 *  still happening. */
const REVEALED = 0.9;

/** A control that is on the screen but cannot be seen or pressed. */
export interface RowActionFinding {
  where: string;
  why: string;
}

export interface RowActionReport {
  findings: RowActionFinding[];
  /** Whether this screen was a subject at all. `false` means it renders no row actions, which is
   *  not a defect — most settings screens are forms. Reported rather than hidden so a screen that
   *  *stops* rendering them shows up as a change in the walk's own output. */
  hovered: boolean;
}

export async function inspectRowActions(page: Page): Promise<RowActionReport> {
  const rows = page.locator(ROWS_WITH_ACTIONS);
  if ((await rows.count()) === 0) return { findings: [], hovered: false };

  const row = rows.first();
  await row.hover();
  const group = row.locator(ACTION_GROUP).first();

  // Polled, not read once: the reveal is a CSS transition, and reading in the frame the pointer
  // lands would catch it mid-way and fail for the wrong reason.
  const read = async () =>
    group.evaluate((e) => {
      const b = e.getBoundingClientRect();
      return { opacity: Number(getComputedStyle(e).opacity), w: b.width, h: b.height };
    });
  let state = await read();
  for (let i = 0; i < 20 && state.opacity < REVEALED; i++) {
    await page.waitForTimeout(50);
    state = await read();
  }

  const findings: RowActionFinding[] = [];
  if (state.opacity < REVEALED) {
    findings.push({
      where: ACTION_GROUP,
      why: `the hovered row's actions are at opacity ${state.opacity} — they are in the DOM, clickable by a test, and invisible to a person. Check that every row class that can hold them appears in the reveal rules in styles/table.css`,
    });
  }
  if (state.w === 0 || state.h === 0) {
    findings.push({
      where: ACTION_GROUP,
      why: 'the hovered row’s actions have no box at all — opaque and zero-sized is still unpressable',
    });
  }
  return { findings, hovered: true };
}
