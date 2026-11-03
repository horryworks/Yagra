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

test('each filterable column carries its funnel inside its own header, and no row is added', async ({
  page,
}) => {
  await page.goto('/nodes/collection/mib');
  await expect(rows(page).first()).toBeVisible();

  // ADR-228: the controls moved from a row under the header to a funnel beside each label. The
  // alignment question is the same one the row had — a funnel laid over the next column reads as
  // that column's — and only geometry can answer it.
  const funnel = page.locator('.dt-head .hf-trigger').first();
  await expect(funnel).toBeVisible();
  const header = funnel.locator('xpath=ancestor::*[contains(@class, "hcell")][1]');
  const [fBox, hBox] = [await funnel.boundingBox(), await header.boundingBox()];
  expect(fBox).not.toBeNull();
  expect(hBox).not.toBeNull();
  if (!fBox || !hBox) return;
  expect(fBox.x, 'the funnel sits left of its header').toBeGreaterThanOrEqual(hBox.x - 2);
  expect(fBox.x + fBox.width, 'the funnel runs past its header').toBeLessThanOrEqual(
    hBox.x + hBox.width + 2,
  );
  // The whole point: the table is the header and the rows, nothing between them.
  const head = await page.locator('.dt-head').boundingBox();
  const first = await rows(page).first().boundingBox();
  expect(first!.y - (head!.y + head!.height), 'something sits between the header and the rows').toBeLessThan(2);
});

test('typing a term reaches the server, and clearing it comes back', async ({ page, mock }) => {
  await page.goto('/nodes/collection/mib');
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

  // Since ADR-153 the term is also in the URL, as `?q=` — the API's own parameter name. The filter
  // row is still a *view* of that one value (`MibRepositoryPage` says so at the call site); what
  // changed is where the value lives, so a reload keeps it.
  await expect(page, 'the term never reached the URL').toHaveURL(new RegExp(`[?&]q=${NEEDLE}`));
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
    await page.goto('/events');
    await expect(page.locator('.dt-head .hf-trigger').first()).toBeVisible();

    await page.getByRole('button', { name: /Filter by Message/ }).click();
    await page.getByRole('dialog').getByRole('searchbox').first().fill(NEEDLE);

    await expect(page, 'the condition never reached the URL').toHaveURL(new RegExp(NEEDLE));
    // The set filter is named in the toolbar, where the closed row trigger used to say it.
    await expect(page.locator('.filter-chip', { hasText: NEEDLE })).toBeVisible();
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
