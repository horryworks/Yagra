// SPDX-License-Identifier: AGPL-3.0-only
// A list the caller may not read says so — instead of its toolbar and its table (ADR-056, ADR-184).
//
// `loadState.test.ts` proves no screen classifies a failed load by hand. It cannot prove what the
// screen then *draws*: the notice has to replace the toolbar and the table, because a notice above
// an empty table keeps "0 credentials" on screen and leaves `+ Add credential` mounted for a caller
// the server just refused. That half is layout, so it is asserted here.
//
// Written before the screens moved onto the shared loader (ADR-184 increment 23), against the code
// as it was, so that the move is checked against what shipped rather than against what it became.
//
// Both directions, per screen. "The notice is shown" alone is satisfied by a screen that shows it
// to everybody; the second test is what keeps the table in front of a caller who may read it.

import { expect, test } from '../support/app';
import { BOOTSTRAP_OVERRIDES } from '../support/bootstrap';

interface Subject {
  path: string;
  /** The list read whose refusal the screen must report. */
  read: string;
  /** Whether the screen draws a `.table-toolbar` when it can read. The integration pages are a
   *  set of cards, not a list with a toolbar. */
  toolbar: boolean;
  /** The refusal, when the screen keeps a sentence of its own rather than the shared one. */
  refused?: RegExp;
  /** False for the two review screens, whose only writes act on a selection, and for the alert
   *  log, which has none: there is no "Add" to lose, so the check that it is gone would pass
   *  vacuously. */
  adds?: boolean;
}

const SUBJECTS: Subject[] = [
  { path: '/events/webhooks', read: '/api/v1/event-sources', toolbar: true },
  { path: '/nodes/credentials', read: '/api/v1/credentials', toolbar: true },
  { path: '/nodes/collection-templates', read: '/api/v1/collection-templates', toolbar: true },
  { path: '/settings/auth', read: '/api/v1/settings/oidc', toolbar: false },
  {
    path: '/settings/users',
    read: '/api/v1/users',
    toolbar: true,
    refused: /Managing users requires an admin account/,
  },
  { path: '/nodes/classification-rules', read: '/api/v1/classification-rules', toolbar: true },
  { path: '/alerts/event-rules', read: '/api/v1/event-rules', toolbar: true },
  { path: '/settings/integrations/netbox', read: '/api/v1/netbox/servers', toolbar: false },
  { path: '/alerts/mutes', read: '/api/v1/mutes', toolbar: true },
  { path: '/alerts/maintenance', read: '/api/v1/maintenance-windows', toolbar: true },
  { path: '/nodes/profiles', read: '/api/v1/profiles', toolbar: true },
  { path: '/alerts/routing', read: '/api/v1/routing-rules', toolbar: true },
  { path: '/settings/integrations/meraki', read: '/api/v1/meraki/orgs', toolbar: false },
  { path: '/nodes/duplicates', read: '/api/v1/nodes/duplicates', toolbar: false, adds: false },
  { path: '/nodes/reclassify', read: '/api/v1/reclassify', toolbar: false, adds: false },
  { path: '/settings/pollers', read: '/api/v1/pollers', toolbar: true },
  { path: '/settings/api-tokens', read: '/api/v1/api-tokens', toolbar: true },
  { path: '/events/forwarding', read: '/api/v1/forwarding/destinations', toolbar: true },
  { path: '/nodes/mib', read: '/api/v1/mib-catalog', toolbar: true },
  { path: '/alerts/rules', read: '/api/v1/thresholds', toolbar: true },
  // A log, not a configuration list: nothing to add. It swallowed every failure until ADR-184
  // increment 27, so a refusal read as an empty history.
  { path: '/alerts/history', read: '/api/v1/alerts/history', toolbar: true, adds: false },
];

/** The sentence both refusal texts end with (`common.loadBlock.*`). */
const REFUSED = /Ask an administrator if you need access/;

/** An "Add …" control — every one of these lists has one for an administrator. */
const ADD = /^\+?\s*(Add|New|Create|Upload)\b/i;

for (const s of SUBJECTS) {
  test.describe(s.path, () => {
    test.describe('refused', () => {
      test.use({
        mockConfig: {
          overrides: BOOTSTRAP_OVERRIDES,
          failures: { [s.read]: { status: 403, code: 'forbidden' } },
        },
      });

      test('says so instead of drawing the list', async ({ page, mock }) => {
        await page.goto(s.path);
        await expect(page.getByText(s.refused ?? REFUSED)).toBeVisible();
        expect(mock.served).toContain(`GET ${s.read}`);
        // Instead of, not above: no toolbar to act from and nothing to add with.
        await expect(page.locator('.table-toolbar')).toHaveCount(0);
        if (s.adds !== false) await expect(page.getByRole('button', { name: ADD })).toHaveCount(0);
      });
    });

    test('draws the list for a caller who may read it', async ({ page, mock }) => {
      await page.goto(s.path);
      await expect.poll(() => mock.served.includes(`GET ${s.read}`)).toBe(true);
      if (s.toolbar) await expect(page.locator('.table-toolbar').first()).toBeVisible();
      // The refused test's "no Add button" means something only if there is one to lose.
      if (s.adds !== false) await expect(page.getByRole('button', { name: ADD }).first()).toBeVisible();
      // A settled absence: the read has been answered and the list drawn from it.
      await expect(page.locator('.app-loading')).toHaveCount(0);
      await expect(page.getByText(s.refused ?? REFUSED)).toHaveCount(0);
    });
  });
}
