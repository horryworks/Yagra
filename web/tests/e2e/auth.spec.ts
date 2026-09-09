// SPDX-License-Identifier: AGPL-3.0-only
// Tier2a: the authentication seam, which is the one thing a mocked run can only ever pretend to
// have. Tier1's login spec proves the *form* behaves — that the server's message reaches the
// operator, that 429 is treated as recoverable — against a mock that answers however the test says.
// It cannot prove that a real password is accepted by a real core through a real nginx, or that a
// deep link is closed to someone who has not signed in.
//
// This is also the one spec allowed to send a non-GET (ADR-052 決定 9): `allowLogin` widens the
// read-only guard by exactly one path, and only here.

import { expect, liveEnv, test } from './support/live';

/** The token the fixture seeds is what every other spec rides on; this one has to arrive without
 *  it. Init scripts run in registration order, so removing it here undoes the seed. */
async function signedOut(page: import('@playwright/test').Page): Promise<void> {
  await page.addInitScript(() => localStorage.removeItem('yagra_token'));
}

test.describe('signing in for real', () => {
  test.use({ allowLogin: true });

  test('accepts the credentials and lands on the dashboard', async ({ page }) => {
    const { user, password } = liveEnv();
    await signedOut(page);
    await page.goto('/login');

    await page.getByLabel('Username').fill(user);
    await page.getByLabel('Password').fill(password);
    await page.getByRole('button', { name: 'Sign in' }).click();

    // Everything between the form and the session store is under test here: nginx proxying a POST,
    // core checking the hash, the token surviving into `localStorage`, and `authed` flipping.
    await expect(page).toHaveURL(/\/dashboard$/);
    await expect(page.getByRole('navigation').first()).toBeVisible();
  });
});

test('a deep link is closed to a browser with no session', async ({ page }) => {
  await signedOut(page);
  await page.goto('/settings/users');

  // ⚠️ An earlier version asserted a redirect to `/login` and failed — an expectation taken from
  // habit rather than from anything this repo declares, which is the failure 決定 7 names. The
  // declaration is `App.tsx`: the requested screen is *replaced*, with the URL untouched. So the
  // property to assert is the replacement, not that the address bar moved.
  //
  // ⚠️ This assertion used to branch on `public_dashboard`, because a public deployment answered a
  // deep link with the board. Since ADR-123 Inc.1 it does not: every anonymous path but the board's
  // own is the sign-in form, on both kinds of deployment, so there is one answer to assert.
  await expect(page.getByRole('button', { name: 'Sign in' })).toBeVisible();

  // 🚨 The security half: whatever replaced the screen, it is not the app shell.
  await expect(page.getByRole('navigation'), 'the app shell rendered for a signed-out visitor')
    .toHaveCount(0);
  // Settings ▸ Users is the pick on purpose: a soft failure there — an empty table instead of the
  // gate — would read to an operator as "there are no users".
  await expect(page.getByRole('table')).toHaveCount(0);
});

test('the sign-in screen offers the public board exactly when there is one', async ({ page }) => {
  // ⚠️ The Tier2a shape: assert *agreement with the deployment*, never a value. Whether this box
  // publishes a board is an operator's setting, so both branches are real and both are checked —
  // a test that only ran on the public side would pass on a box where the button never appears.
  const config = await (await page.request.get('/api/v1/config')).json();
  const isPublic = config.public_dashboard === true;

  await signedOut(page);
  await page.goto('/');
  await expect(page.getByRole('button', { name: 'Sign in' })).toBeVisible();

  const toBoard = page.getByRole('link', { name: 'Show public dashboard' });
  if (!isPublic) {
    // A link to a path that would answer with this same screen is an affordance that lies.
    await expect(toBoard).toHaveCount(0);
    return;
  }

  // 🚨 The whole journey, which no unit test can see: the gate reads the path from outside the
  // router, so this only works because the link is a real navigation. A `navigate()` here would
  // change the address bar and leave the sign-in form on screen, and `appGate.test.ts` would still
  // be green — the defect would be the router again, exactly as it was the first time.
  await toBoard.click();
  await expect(page).toHaveURL(/\/dashboard\/public$/);
  // The bare shell: its own way back is a link, and there is no app shell around it.
  await expect(page.getByRole('link', { name: 'Sign in' })).toBeVisible();
  await expect(page.getByRole('navigation')).toHaveCount(0);
});

test('a public deployment still serves the sign-in form at /login', async ({ page }) => {
  // 🚨 The defect Tier2a found on the first real public deployment: `appView` answered every URL
  // with the public board, so there was no route that could reach the form and no way for an
  // operator to sign in at all. It is asserted here rather than only in `appGate.test.ts` because
  // the unit test cannot see the router, and the router is what was wrong.
  await signedOut(page);
  await page.goto('/login');
  await expect(page.getByRole('button', { name: 'Sign in' })).toBeVisible();
  await expect(page.getByRole('navigation')).toHaveCount(0);
});

test('the API edge refuses an unauthenticated read', async ({ page }) => {
  // The browser's request context carries no bearer token, so this is what an unauthenticated
  // client gets from the deployment. A 200 here would mean the edge is open regardless of what
  // the UI does about it — and the UI's behaviour above would then be decoration.
  const res = await page.request.get('/api/v1/nodes');
  expect(res.status(), 'the deployment served the inventory without a session').toBe(401);
});
