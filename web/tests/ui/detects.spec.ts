// SPDX-License-Identifier: AGPL-3.0-only
// Does the walk actually detect anything?
//
// `walk.spec.ts` is fifty assertions that a marker is visible. If the marker check were vacuous —
// if `ymock-` leaked in from the shell, or the poll returned true for the wrong reason — every one
// of them would pass on a completely broken build and nobody would know. A suite made only of
// "it works" cases cannot tell you it is working; the repo has paid for that before (a regex that
// rejected every input passed all its tests, because every test asserted rejection).
//
// So: one screen, one endpoint broken, and the same check must come back false.

import { expect, test } from '../support/app';
import { BOOTSTRAP_OVERRIDES } from '../support/bootstrap';
import { MOCK_PREFIX } from '../support/openapi';

test.use({
  mockConfig: {
    overrides: BOOTSTRAP_OVERRIDES,
    // The Users screen is the cleanest subject: one list endpoint, and nothing else on the page
    // (the shell shows an avatar initial, not a name) carries a generated string.
    failures: { '/api/v1/users': 500 },
  },
});

test('a screen whose data failed to load does NOT report itself as rendered', async ({
  page,
  errors,
}) => {
  await page.goto('/settings/users');
  await expect(page.locator('.app-loading')).toHaveCount(0, { timeout: 15_000 });
  // Give the screen as long as a passing screen gets, then some. If a marker were going to
  // appear, it has had every chance.
  await page.waitForTimeout(2_000);

  const visible = await page.evaluate(
    (prefix) => (document.body.innerText || '').includes(prefix),
    MOCK_PREFIX,
  );
  expect(visible, 'the walk would have called a screen with a 500 "rendered"').toBe(false);

  // And the failure must be a handled one: a 500 the UI reports, not an exception that blanks it.
  expect(errors.uncaught, 'a 500 threw instead of being surfaced').toEqual([]);
});
