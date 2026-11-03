// SPDX-License-Identifier: AGPL-3.0-only
// "View only" beside a screen's title (ADR-235).
//
// Write controls a caller may not use are not drawn (ADR-056), so a Viewer could not tell a
// missing feature from a withheld one. The screen's title now says so, and pressing it names the
// privilege and the caller's role.
//
// Both directions, deliberately: a chip that never appears for anybody passes "an admin sees no
// chip" just as well, and the walk signs in as admin.

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

// The badge draws a `▾` after its word, and that is part of the accessible name.
const CHIP = { name: /^view only/i };

test('an admin is not told the screen is view only', async ({ page, mock }) => {
  await page.goto('/alerts/maintenance');
  await expect(page.getByRole('heading', { level: 1 })).toBeVisible();
  // A settled absence: the chip is drawn only after the role matrix arrives, so wait for it.
  await expect.poll(() => mock.requests.some((r) => r.pathname === '/api/v1/roles')).toBe(true);
  await page.waitForTimeout(250);
  await expect(page.getByRole('button', CHIP)).toHaveCount(0);
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

  test('the title says view only, and pressing it names the privilege and the role', async ({
    page,
  }) => {
    await page.goto('/alerts/maintenance');
    const chip = page.locator('.pageheader').getByRole('button', CHIP);
    await expect(chip).toBeVisible();
    await chip.click();
    const pop = page.getByRole('dialog');
    await expect(pop).toContainText('manage_maintenance');
    await expect(pop).toContainText('viewer');
  });
});
