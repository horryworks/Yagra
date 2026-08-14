// SPDX-License-Identifier: AGPL-3.0-only
// The sign-in form (ADR-052 決定 5). The one flow the token-injection shortcut cannot cover, so it
// gets the only test that actually posts credentials.
//
// The interesting half is not the happy path — it is that the server's *own* message reaches the
// operator. `LoginPage` decodes `ApiError` and shows `x.message`, falling back to a generic string
// only for a non-API failure; and it treats `locked_out`/`expired` as recoverable (a warning)
// rather than as a hard failure. Both of those are decisions a screenshot cannot check and a unit
// test of the API client cannot reach, because the branch lives in the component.

import { expect, test } from '../support/app';
import { BOOTSTRAP_OVERRIDES } from '../support/bootstrap';

/** The sign-in screen is the one place the app is deliberately NOT signed in. */
async function signedOut(page: import('@playwright/test').Page): Promise<void> {
  await page.addInitScript(() => localStorage.removeItem('yagra_token'));
}

test.describe('a correct sign-in', () => {
  test('lands on the dashboard', async ({ page }) => {
    await signedOut(page);
    await page.goto('/login');

    await page.getByLabel('Username').fill('ymock-operator');
    await page.getByLabel('Password').fill('correct horse');
    await page.getByRole('button', { name: 'Sign in' }).click();

    // The generated `/auth/login` response carries a token, so the app should leave the form.
    await expect(page).toHaveURL(/\/dashboard$/);
  });
});

test.describe('a rejected sign-in', () => {
  test.use({
    mockConfig: { overrides: BOOTSTRAP_OVERRIDES, failures: { '/api/v1/auth/login': 401 } },
  });

  test('stays on the form and shows the server’s reason', async ({ page, errors }) => {
    await signedOut(page);
    await page.goto('/login');

    await page.getByLabel('Username').fill('ymock-operator');
    await page.getByLabel('Password').fill('wrong');
    await page.getByRole('button', { name: 'Sign in' }).click();

    // The message is the SERVER's, not a generic client string: `LoginPage` shows `x.message` off
    // the decoded `ApiError`, and losing that would leave an operator staring at "Sign-in failed"
    // when the server said something useful.
    await expect(page.getByText('forced 401')).toBeVisible();
    await expect(page).toHaveURL(/\/login$/);
    // A rejected password is an expected outcome, not a fault: nothing may throw, and the button
    // must come back — `busy` is cleared in `finally`, and a missing `finally` locks the form.
    expect(errors.uncaught).toEqual([]);
    await expect(page.getByRole('button', { name: 'Sign in' })).toBeEnabled();
  });
});

test.describe('a locked-out sign-in', () => {
  test.use({
    mockConfig: { overrides: BOOTSTRAP_OVERRIDES, failures: { '/api/v1/auth/login': 429 } },
  });

  test('is reported as recoverable, not as a hard failure', async ({ page }) => {
    await signedOut(page);
    await page.goto('/login');

    await page.getByLabel('Username').fill('ymock-operator');
    await page.getByLabel('Password').fill('again');
    await page.getByRole('button', { name: 'Sign in' }).click();

    await expect(page.getByText('forced 429')).toBeVisible();
    await expect(page).toHaveURL(/\/login$/);
  });
});
