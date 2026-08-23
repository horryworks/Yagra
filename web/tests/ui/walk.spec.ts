// SPDX-License-Identifier: AGPL-3.0-only
// The route walk (ADR-052 Inc.1). One test per screen, generated from the nav.
//
// What each screen must satisfy, and why each check is here rather than assumed:
//
//   1. **It renders its data.** Not "an element exists" — a `ymock-` string from the generated
//      response is visible, so the page is showing what the API gave it. Existence checks pass on
//      an empty table.
//   2. **The URL did not move.** React Router falls back to `<Navigate to="/dashboard">` for an
//      unmatched path, and the dashboard is full of markers — so check 1 alone would pass while
//      the screen was never reached. This is the ADR-031 failure exactly ("the button renders,
//      clicking it bounces to Overview").
//   3. **Nothing threw.** There is no ErrorBoundary, so a render exception blanks the page
//      silently. `pageerror` is the only channel it appears on.
//   4. **No `console.error`.**
//   5. **No request the OpenAPI document does not describe.** A hand-rolled `fetch` that escaped
//      the typed client, or a stale committed document, shows up here.
//   6. **Its filter controls sit under their columns** (ADR-053) — see the note at the assertion.
//   7. **It explains itself in one line, on screen** (ADR-055 R2). `NOTE_EXEMPT` in `screens.ts`
//      holds the two screens that argue out of it, each with its reason.
//   8. **Nothing is laid out off the page, and no text is cut off with no way to read it**
//      (ADR-088) — see the note at the assertion.
//   9. **Its row actions appear when the row is hovered** (ADR-088) — see `rowActions.ts`.

import { expect, test } from '../support/app';
import { MOCK_PREFIX } from '../support/openapi';
import { inspectFilterSurface, MUST_FILTER } from './filterSurface';
import { inspectRowActions } from './rowActions';
import { inspectScreenGeometry, MIN_TEXT_ELEMENTS } from './screenGeometry';
import { ALL_SCREENS, NOTE_EXEMPT, SCREEN_EXPECT, type Expect } from './screens';

/** How long a screen gets to show its data. Generous: the settings group lazy-loads a chunk. */
const RENDER_TIMEOUT = 15_000;

/** Is a generated string actually on screen?
 *
 *  Two things this must get right, both learned by getting them wrong:
 *
 *  - **Visible, not merely present.** `getByText(…).first()` takes the first match in DOM order
 *    whether or not it is displayed. On the Nodes page that was `<option value="ymock-name">`
 *    inside a closed `<select>` — so the assertion failed while the tree beside it was rendering
 *    the very same string. `innerText` is the rendered text, which is the question being asked.
 *  - **Form fields hold their text in `value`, where no text query can see it.** Half the Settings
 *    screens are forms; ignoring input values would have meant marking them all "unassertable" and
 *    losing the check on the screens that change most often. */
async function markerVisible(page: import('@playwright/test').Page): Promise<boolean> {
  return page.evaluate((prefix) => {
    if ((document.body.innerText || '').includes(prefix)) return true;
    const fields = document.querySelectorAll('input, textarea, select');
    for (const el of Array.from(fields)) {
      const value = (el as HTMLInputElement).value;
      if (typeof value !== 'string' || !value.includes(prefix)) continue;
      const box = el.getBoundingClientRect();
      if (box.width > 0 && box.height > 0) return true;
    }
    return false;
  }, MOCK_PREFIX);
}

async function assertRendered(
  page: import('@playwright/test').Page,
  expectation: Expect,
): Promise<void> {
  switch (expectation.kind) {
    case 'marker':
      await expect
        .poll(() => markerVisible(page), {
          timeout: RENDER_TIMEOUT,
          message: `no visible "${MOCK_PREFIX}…" string — the screen did not render what the mock gave it`,
        })
        .toBe(true);
      return;
    case 'text':
      await expect(page.getByText(expectation.text).first()).toBeVisible({
        timeout: RENDER_TIMEOUT,
      });
      return;
    case 'locator':
      await expect(page.locator(expectation.sel).first()).toBeVisible({
        timeout: RENDER_TIMEOUT,
      });
      return;
    case 'none':
      // Still wait for the app to finish booting, or checks 2-5 would run against a loading screen
      // and pass for the wrong reason.
      await expect(page.locator('.app-loading')).toHaveCount(0, {
        timeout: RENDER_TIMEOUT,
      });
      return;
  }
}

for (const screen of ALL_SCREENS) {
  test(`${screen.path} renders the data it was given`, async ({ page, mock, errors }) => {
    const expectation = SCREEN_EXPECT[screen.path];
    await page.goto(`${screen.path}${screen.query ?? ''}`);

    try {
      await assertRendered(page, expectation);
    } catch (e) {
      // A blank page reads as "the marker never appeared", which is true and useless. If something
      // threw, say that instead — it is the cause, and the marker is the symptom.
      if (errors.uncaught.length > 0) {
        throw new Error(
          `${screen.path}: threw while rendering (no ErrorBoundary ⇒ blank page)\n` +
            errors.uncaught.join('\n'),
        );
      }
      throw e;
    }

    expect(new URL(page.url()).pathname, `${screen.path} redirected instead of rendering`).toBe(
      screen.path,
    );
    expect(errors.uncaught, `${screen.path}: uncaught exception`).toEqual([]);
    expect(errors.logged, `${screen.path}: console.error`).toEqual([]);
    expect(mock.unmatched, `${screen.path}: request outside the OpenAPI document`).toEqual([]);

    // 6. **Its filter controls are where they claim to be** (ADR-053). Free here: the page is
    //    already open and settled, and a screen with no filter surface is simply not a subject —
    //    so this covers every screen that has one without anyone maintaining a list of which do.
    //    `MUST_FILTER` is the other direction, and only for the screens whose *absence* would be
    //    silent: a list that stopped filtering still renders, it just shows everything forever.
    expect(await inspectFilterSurface(page), `${screen.path}: filter surface`).toEqual([]);
    if (MUST_FILTER[screen.path]) {
      expect(
        await page.locator('.dt-f-trigger').count(),
        `${screen.path} has no filter control — ${MUST_FILTER[screen.path]}`,
      ).toBeGreaterThan(0);
    }

    // 7. **It says what it is, without being hovered** (ADR-055 G1 / R2). One line under the title,
    //    on every screen, and `NOTE_EXEMPT` is where a screen argues it should not have one. Free
    //    here for the same reason as check 6 — the page is open and settled — and it catches the
    //    thing nothing else can: a new screen that ships explaining itself nowhere. Four screens
    //    were in that state when this was written, including the product's own landing page.
    if (!NOTE_EXEMPT[screen.path]) {
      const note = page.locator('.pageheader-note');
      await expect(note, `${screen.path} has no one-line description under its title`).toHaveCount(
        1,
      );
      expect(
        (await note.innerText()).trim().length,
        `${screen.path}: its description is empty`,
      ).toBeGreaterThan(0);
    }

    // 8. **It is readable and it fits** (ADR-088). Four of the last ten `fix(web)` commits were
    //    geometry defects that every text assertion passes — the element is there, the string is
    //    there, and a human still cannot read it. Free here for the same reason as checks 6 and 7.
    //
    //    🚨 **The count is asserted, not logged.** A sweep whose traversal stops matching returns
    //    an empty finding list, which is indistinguishable from a healthy screen. This repo has
    //    shipped that exact failure three times; the floor is what makes it a red test instead.
    const geometry = await inspectScreenGeometry(page);
    expect(geometry.findings, `${screen.path}: geometry`).toEqual([]);
    expect(
      geometry.inspected,
      `${screen.path}: the sweep found only ${geometry.inspected} text elements — it stopped matching, so its empty result means nothing`,
    ).toBeGreaterThanOrEqual(MIN_TEXT_ELEMENTS);

    // 9. **Its row actions can be seen when the row is hovered** (ADR-088). One hover, on a screen
    //    that is already open — and no list of which screens have row actions, deliberately: the
    //    defect this catches came from one shared stylesheet and hit ten screens at once, so a list
    //    would have had to be right about all ten. `rowActions.ts` carries the reasoning, including
    //    why `isVisible()` is not allowed to answer this.
    expect((await inspectRowActions(page)).findings, `${screen.path}: row actions`).toEqual([]);
  });
}
