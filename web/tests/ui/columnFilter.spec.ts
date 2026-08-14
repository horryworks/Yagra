// SPDX-License-Identifier: AGPL-3.0-only
// The column filter row (ADR-053), exercised in a browser.
//
// WHY THIS SURFACE, AND WHY NOW. ADR-053 shipped with 1,966 unit tests behind it, and **six
// defects were still found by eye after it went out** — because what broke was never the
// predicate. It was the filter row sharing one grid template with the header and every data row, a
// clear-all whose two URL writes cancelled each other, and a count that described the wrong set.
// None of those are reachable from a `.ts` test of the pure functions.
//
// The MIB repository is the subject because its filter is **server-side**: typing has to reach the
// request. That is a shipped regression in its own right (`fix(web): the History filters never
// reached the request` — the screen narrowed its own state and never told the server), and it is
// the one thing a client-side screen cannot demonstrate.

import { expect, test } from '../support/app';
import { BOOTSTRAP_OVERRIDES } from '../support/bootstrap';
import { defaultBodyFor, MOCK_PREFIX, type Json } from '../support/openapi';

const CATALOG = '/api/v1/mib-catalog';
const ROW_COUNT = 12;
/** Rows 0-3 carry this in their metric name; 4-11 do not. */
const NEEDLE = 'router';
const MATCHING = 4;

/** The catalog, answered the way the server answers it: `?q=` narrows it here, not in the browser.
 *  Anything the screen fails to send simply does not narrow — which is the point. */
function catalog(url: URL): Json {
  const [template] = defaultBodyFor(CATALOG) as Record<string, Json>[];
  const all = Array.from({ length: ROW_COUNT }, (_, i) => ({
    ...template,
    id: `00000000-0000-4000-8000-${String(i).padStart(12, '0')}`,
    metric: `${MOCK_PREFIX}${i < MATCHING ? NEEDLE : 'switch'}_${String(i).padStart(2, '0')}`,
  }));
  const q = url.searchParams.get('q');
  return (q ? all.filter((r) => r.metric.includes(q)) : all) as unknown as Json;
}

test.use({ mockConfig: { overrides: { ...BOOTSTRAP_OVERRIDES, [CATALOG]: catalog } } });

const rows = (page: import('@playwright/test').Page) => page.locator('.dt-row');

test('the filter row sits under its own column header', async ({ page }) => {
  await page.goto('/nodes/mib');
  await expect(rows(page).first()).toBeVisible();

  const filterRow = page.getByRole('group', { name: 'Column filters' });
  await expect(filterRow).toBeVisible();

  // ⚠️ The alignment bug `ui-conventions.md` warns about is invisible to any DOM assertion: the
  // filter row, the header and every data row share ONE grid template, so a stray extra track
  // slides the controls out from under the headers they belong to and *no test can see it*.
  // Geometry can. This is the kind of assertion that only exists because a browser is running.
  const control = filterRow.getByRole('button', { name: /Filter by/ }).first();
  const header = page.locator('.dt-head > *').first();
  const [cBox, hBox] = [await control.boundingBox(), await header.boundingBox()];
  expect(cBox).not.toBeNull();
  expect(hBox).not.toBeNull();
  if (!cBox || !hBox) return;
  expect(cBox.x, 'the filter control sits left of its header').toBeGreaterThanOrEqual(hBox.x - 2);
  expect(cBox.x, 'the filter control has slid past its header').toBeLessThan(hBox.x + hBox.width);
  expect(cBox.y, 'the filter row is not below the header row').toBeGreaterThan(hBox.y);
});

test('typing a term reaches the server, and clearing it comes back', async ({ page, mock }) => {
  await page.goto('/nodes/mib');
  await expect(rows(page)).toHaveCount(ROW_COUNT);

  await page.getByRole('button', { name: /Filter by/ }).first().click();
  const panel = page.getByRole('dialog');
  await expect(panel).toBeVisible();
  await panel.getByRole('searchbox').first().fill(NEEDLE);

  await expect(rows(page), `only the ${MATCHING} matching entries should remain`).toHaveCount(
    MATCHING,
  );

  // The load-bearing assertion: the narrowing happened because the SERVER was asked to narrow.
  // A screen that filtered its own copy would show the same four rows and be wrong at any scale
  // past one page.
  const asked = mock.requests.filter(
    (r) => r.pathname === CATALOG && r.search.includes(`q=${NEEDLE}`),
  );
  expect(asked.length, 'the filter never reached the request').toBeGreaterThan(0);

  // ⚠️ **This screen deliberately does NOT put its filter in the URL.** The first version of this
  // test asserted it did — an expectation taken from a habit rather than from anything the repo
  // declares, which is the failure 決定 7 names outright. `MibRepositoryPage` says so at the call
  // site: "the filter row is a *view* of `query`, not a second copy of it". The URL contract is
  // declared for the Events screens (ADR-053 Inc.2, "Events の絞り込みは全部 URL") and is asserted
  // there, below.
  //
  // 🚨 The clear-all regression: two `setSearchParams` calls in one handler, the second undoing
  // the first, and the button doing nothing at all.
  await page.keyboard.press('Escape');
  await page.getByRole('button', { name: /Clear all filters/ }).click();
  await expect(rows(page), 'clear-all did not restore the full list').toHaveCount(ROW_COUNT);
});

// Events is where the URL contract is declared (ADR-053 Inc.2: the Events filtering is *all* URL),
// so it is where the round-trip is asserted. Two independent failures hide here — a codec that
// encodes one way and decodes another, and a clear-all whose second URL write undoes its first.
test.describe('the Events filter row', () => {
  test.use({ mockConfig: { overrides: BOOTSTRAP_OVERRIDES } });

  test('writes its condition to the URL and reads it back', async ({ page, mock }) => {
    await page.goto('/alerts/events');
    await expect(page.getByRole('group', { name: 'Column filters' })).toBeVisible();

    await page.getByRole('button', { name: /Filter by Message/ }).click();
    await page.getByRole('dialog').getByRole('searchbox').first().fill(NEEDLE);

    await expect(page, 'the condition never reached the URL').toHaveURL(new RegExp(NEEDLE));
    // …and reached the server, which for a fleet-scaled list is the only place it can be applied.
    await expect
      .poll(() =>
        mock.requests.filter(
          (r) => r.pathname === '/api/v1/events' && r.search.includes(NEEDLE),
        ).length,
      )
      .toBeGreaterThan(0);

    // Reload from the URL the screen itself wrote: whatever it encodes, it must be able to decode.
    const written = page.url();
    await page.goto(written);
    await expect(page.getByRole('dialog')).toHaveCount(0);
    await expect(page, 'the filter did not survive a reload').toHaveURL(new RegExp(NEEDLE));

    await page.getByRole('button', { name: /Clear all filters/ }).click();
    await expect(page, 'clear-all left the condition in the URL').not.toHaveURL(new RegExp(NEEDLE));
  });
});
