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
  /** How many buttons were pressed-tested, over every table on the screen. A floor for callers:
   *  a selector that stopped matching buttons would otherwise report a clean screen. */
  buttons: number;
}

/** Index, among `ROWS_WITH_ACTIONS`, of the first row in each table on the screen. One per table,
 *  not one per screen: Notification delivery has two tables, and the first one alone would leave
 *  the second unchecked (ADR-193). */
async function firstRowOfEachTable(page: Page): Promise<number[]> {
  return page.evaluate((sel) => {
    const seen = new Set<Element>();
    const out: number[] = [];
    document.querySelectorAll(sel).forEach((row, i) => {
      const table = row.closest('.dt, .ytable, .il-list') ?? row.parentElement ?? document.body;
      if (seen.has(table)) return;
      seen.add(table);
      out.push(i);
    });
    return out;
  }, ROWS_WITH_ACTIONS);
}

export async function inspectRowActions(page: Page): Promise<RowActionReport> {
  const rows = page.locator(ROWS_WITH_ACTIONS);
  if ((await rows.count()) === 0) return { findings: [], hovered: false, buttons: 0 };

  const findings: RowActionFinding[] = [];
  let buttons = 0;
  for (const index of await firstRowOfEachTable(page)) {
    const row = rows.nth(index);
    await row.hover();
    const group = row.locator(ACTION_GROUP).first();

    // Polled, not read once: the reveal is a CSS transition, and reading in the frame the pointer
    // lands would catch it mid-way and fail for the wrong reason.
    const read = async () =>
      group.evaluate((e) => {
        const b = e.getBoundingClientRect();
        return {
          opacity: Number(getComputedStyle(e).opacity),
          w: b.width,
          h: b.height,
        };
      });
    let state = await read();
    for (let i = 0; i < 20 && state.opacity < REVEALED; i++) {
      await page.waitForTimeout(50);
      state = await read();
    }

    if (state.opacity < REVEALED) {
      findings.push({
        where: `${ACTION_GROUP} (table ${index})`,
        why: `the hovered row's actions are at opacity ${state.opacity} — they are in the DOM, clickable by a test, and invisible to a person. Check that every row class that can hold them appears in the reveal rules in styles/table.css`,
      });
    }
    if (state.w === 0 || state.h === 0) {
      findings.push({
        where: `${ACTION_GROUP} (table ${index})`,
        why: 'the hovered row’s actions have no box at all — opaque and zero-sized is still unpressable',
      });
    }

    // Opaque is not pressable. A column narrower than its buttons pushes the last ones past the
    // cell, where the table clips them: opacity 1, a real box, and a click lands on something
    // else. That is how "Delete channel" became unreachable (ADR-193), with the opacity check
    // above green. So ask the browser what a click at each button's centre would hit.
    //
    // A table wider than the screen is a different thing: its last column is off to the side, and
    // a person scrolls the table to reach it. So first scroll whatever the operator could scroll —
    // an ancestor with `overflow-x: auto | scroll` — and only then ask. An `overflow: hidden`
    // ancestor is never scrolled: that is the clip that hides a too-narrow column, and scrolling
    // it from a script would make the defect pass (`scrollintoview-scrolls-a-clipping-pane`).
    const hits = await group.evaluate((e) =>
      [...e.querySelectorAll('button')].map((b) => {
        const restore: [Element, number][] = [];
        for (let a = b.parentElement; a; a = a.parentElement) {
          const ox = getComputedStyle(a).overflowX;
          if ((ox !== 'auto' && ox !== 'scroll') || a.scrollWidth <= a.clientWidth) continue;
          const box = a.getBoundingClientRect();
          const r = b.getBoundingClientRect();
          if (r.right > box.right || r.left < box.left) {
            restore.push([a, a.scrollLeft]);
            a.scrollLeft += r.right > box.right ? r.right - box.right + 8 : r.left - box.left - 8;
          }
        }
        const r = b.getBoundingClientRect();
        const hit = document.elementFromPoint(r.left + r.width / 2, r.top + r.height / 2);
        for (const [a, left] of restore.reverse()) a.scrollLeft = left;
        return {
          label: b.getAttribute('title') ?? b.getAttribute('aria-label') ?? '?',
          ok: !!hit && b.contains(hit),
        };
      }),
    );
    buttons += hits.length;
    if (hits.length === 0) {
      findings.push({
        where: `${ACTION_GROUP} (table ${index})`,
        why: 'the action group holds no button — nothing here could be pressed',
      });
    }
    for (const h of hits.filter((h) => !h.ok)) {
      findings.push({
        where: `${ACTION_GROUP} button "${h.label}" (table ${index})`,
        why: 'a click at its centre lands on something else — the actions column is probably narrower than its buttons; size it with rowActionsWidth()',
      });
    }
  }
  return { findings, hovered: true, buttons };
}
