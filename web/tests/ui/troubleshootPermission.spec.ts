// SPDX-License-Identifier: AGPL-3.0-only
// Troubleshoot — who is offered "Run" (ADR-052 Tier1, ADR-056).
//
// `POST /analysis/jobs` takes `ack_alerts`; reading past runs takes `view`. Until this was fixed the
// catalogue drew all fifteen Run buttons for a Viewer, and pressing one answered "could not start"
// with no reason — the directory asked `useCan` in exactly one place (Scheduled's `+ Add`).
//
// Both directions, deliberately. "A Viewer sees no Run button" alone is satisfied by a page that
// never draws one for anybody, which is the failure the bootstrap's `/roles` fixture warns about:
// every write control in the app can vanish with every screen still green.

import { expect, test } from '../support/app';
import { BOOTSTRAP_OVERRIDES } from '../support/bootstrap';
import type { Json } from '../support/openapi';

/** The bootstrap's role matrix, with `viewer` cut down to `view` — which is what a Viewer holds. */
function viewerRoles(): Json {
  const roles = BOOTSTRAP_OVERRIDES['/api/v1/roles'] as {
    permissions: Json[];
    roles: { key: string; permissions: string[] }[];
  };
  return {
    ...roles,
    roles: roles.roles.map((r) => (r.key === 'viewer' ? { ...r, permissions: ['view'] } : r)),
  } as unknown as Json;
}

const RUN = { name: /^run$/i };

test('an admin is offered Run on every tool', async ({ page }) => {
  await page.goto('/troubleshoot');
  const runs = page.locator('.ts-tool').getByRole('button', RUN);
  // `count()` does not wait, and `useCan` answers false until the role matrix arrives — so the
  // control appears a moment after the card it sits on. Wait for one before counting them.
  await expect(runs.first()).toBeVisible();
  // A floor, not an exact count: the point is that the control exists at all for this caller.
  expect(await runs.count()).toBeGreaterThanOrEqual(10);
});

test.describe('as a Viewer', () => {
  test.use({
    mockConfig: {
      overrides: {
        ...BOOTSTRAP_OVERRIDES,
        '/api/v1/auth/me': {
          ...(BOOTSTRAP_OVERRIDES['/api/v1/auth/me'] as Record<string, Json>),
          role: 'viewer',
        },
        '/api/v1/roles': viewerRoles(),
      },
    },
  });

  test('no tool offers Run, and the page says which privilege is missing', async ({ page, mock }) => {
    await page.goto('/troubleshoot');
    // The cards still render — this is a page a Viewer may read. (Waited for: `count()` does not.)
    await expect(page.locator('.ts-tool').first()).toBeVisible();
    // 🚨 The zero below has to be a SETTLED zero. `useCan` answers false until the role matrix
    // arrives, so for a moment nobody is offered Run — an admin included — and a count taken then
    // would pass for the wrong reason. Wait until the matrix has been served and drawn from.
    await expect
      .poll(() => mock.requests.some((r) => r.pathname === '/api/v1/roles'))
      .toBe(true);
    await page.waitForTimeout(250);
    expect(await page.locator('.ts-tool').count()).toBeGreaterThanOrEqual(10);
    await expect(page.locator('.ts-tool').getByRole('button', RUN)).toHaveCount(0);
    // Named from the server's catalogue, not a generic "no permission".
    await expect(page.getByText(/ack_alerts/)).toBeVisible();

    // A card is a description now, not a launcher: clicking it must not open the run drawer…
    await page.locator('.ts-tool').first().click();
    await expect(page.getByRole('button', { name: /run analysis/i })).toHaveCount(0);
    // …and nothing was asked of the server that a Viewer may not ask.
    expect(mock.requests.filter((r) => r.method !== 'GET')).toEqual([]);
  });
});
