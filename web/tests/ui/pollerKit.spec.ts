// SPDX-License-Identifier: AGPL-3.0-only
// A site that has never connected can still be given its kit, from this screen (ADR-065 Inc.9).
//
// 🚨 What this protects is a circle, not a button. `docker-compose.poller.yml` travels only inside
// the kit archive (ADR-065 decision 5); before Inc.9 the archive was reachable only from a row in
// the fleet table; and a row appears only once that poller has heartbeated — which needs the
// composition. Measured on a fresh v0.3.4 deployment (2026-09-01): a FIRST remote site could not be
// stood up from the WebUI at all, and the published installation guide described the order as if it
// could. Take this control away and the product silently returns to that state.
//
// Vitest cannot be the reader: the modal is a `.tsx`, which Vitest does not run (testing.md). The
// route walk opens /settings/pollers but never opens this dialog.

import { expect, test } from '../support/app';

test('the register dialog offers the kit, for an id no poller has ever reported', async ({
  page,
}) => {
  await page.goto('/settings/pollers');
  await page.getByRole('button', { name: 'Register poller' }).click();

  const modal = page.locator('.modal').last();
  await expect(modal).toBeVisible();

  // The id is deliberately one the mocked fleet does not contain: the whole point is that the
  // control does not depend on a row existing. If a fixture ever starts shipping this id, the test
  // still asserts the right thing — but pick one that reads as new.
  await modal.getByRole('textbox').first().fill('site-never-seen-1');
  await modal.getByRole('textbox').nth(1).fill('tokyo');

  // Read the button by role and name rather than by position: the manual half of this dialog also
  // has buttons ("Copy"), and a positional locator would keep passing while pointing at one.
  const issue = modal.getByRole('button', { name: /Issue token & download/ });
  await expect(issue).toHaveCount(1);
  await expect(issue).toBeEnabled();
});

test('the kit button is refused an id the server would reject, before any request', async ({
  page,
}) => {
  await page.goto('/settings/pollers');
  await page.getByRole('button', { name: 'Register poller' }).click();
  const modal = page.locator('.modal').last();

  // `issue_poller_token` validates the id at the edge because it becomes a NATS username and a
  // subject component. The dialog must not send one it already knows is invalid — otherwise the
  // first thing a new operator sees is a 400 from a screen that could have said so itself.
  await modal.getByRole('textbox').first().fill('bad id!');
  await modal.getByRole('textbox').nth(1).fill('tokyo');
  await expect(modal.getByRole('button', { name: /Issue token & download/ })).toBeDisabled();

  // And the accepting case, in the same test: a check that only ever refuses passes when the
  // control is refused unconditionally.
  await modal.getByRole('textbox').first().fill('site-never-seen-1');
  await expect(modal.getByRole('button', { name: /Issue token & download/ })).toBeEnabled();
});
