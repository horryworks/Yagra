// SPDX-License-Identifier: AGPL-3.0-only
// TEMPORARY (ADR-184 increments 23–33): what every list toolbar holds, recorded before the toolbars
// move onto the shared `ListToolbar`, and compared after each batch.
//
// A toolbar that loses its `+ Add` button during a mechanical move still renders, still filters and
// still passes every other check — the screen simply stops offering the action. Nothing else in the
// suite looks at *which* controls a toolbar holds, so this does, for the length of the move, and is
// deleted with the last batch (increment 33). It is not a design lock: a deliberate change to a
// toolbar re-records with `UPDATE_TOOLBAR_INVENTORY=1` and the diff is the review.
//
// What is recorded per control: its tag and class, and for a button its text with digits removed
// (so `Filter (2)` and a result count do not make it move). The result count's text is not recorded
// at all — increment 33 changes its wording on purpose (T1, T2).

import { readFileSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';
import { expect, test } from '../support/app';
import { BOOTSTRAP_OVERRIDES, deviceNode } from '../support/bootstrap';
import { MERAKI_ORG_SCREEN } from './screens';

const NODE_ID = '00000000-0000-4000-8000-0000000000aa';
const FILE = join(process.cwd(), 'tests/ui/toolbarInventory.json');
const UPDATE = !!process.env.UPDATE_TOOLBAR_INVENTORY;

/** Every surface whose toolbar the move touches that a URL can reach. The three Troubleshoot report
 *  bodies are left out: each needs a finished run of its own tool with findings of its own shape,
 *  and none of them has an action button to lose. */
const SUBJECTS = [
  '/events/webhooks',
  '/alerts/event-rules',
  '/nodes/classification-rules',
  '/nodes/credentials',
  '/alerts/mutes',
  '/alerts/maintenance',
  '/settings/pollers',
  '/settings/api-tokens',
  '/events/forwarding',
  '/troubleshoot/scheduled',
  '/topology/dependency',
  '/nodes/collection-templates',
  '/nodes/mib',
  '/alerts/routing',
  '/dashboard/reports',
  MERAKI_ORG_SCREEN,
  '/nodes/profiles',
  '/alerts/rules',
  '/settings/audit',
  '/alerts/history',
  '/events',
  '/troubleshoot/findings',
  '/settings/users',
  '/alerts',
  '/troubleshoot/runs',
  '/nodes/discovery',
  `/nodes/${NODE_ID}?tab=neighbors`,
  `/nodes/${NODE_ID}?tab=events`,
  `/nodes/${NODE_ID}?tab=interfaces`,
  `/nodes/${NODE_ID}?tab=collection`,
  `/nodes/${NODE_ID}?tab=flow`,
];

/** The toolbars, including the three node-detail tabs that keep their own container. */
const TOOLBAR = '.table-toolbar, .nd-if-toolbar, .nd-flow-toolbar-actions, .nd-section-head';

test.use({
  mockConfig: {
    overrides: { ...BOOTSTRAP_OVERRIDES, '/api/v1/nodes/{node_id}': () => deviceNode(NODE_ID) },
  },
});

const recorded: Record<string, string[][]> = (() => {
  try {
    return JSON.parse(readFileSync(FILE, 'utf8')) as Record<string, string[][]>;
  } catch {
    return {};
  }
})();
const seen: Record<string, string[][]> = {};

test.describe.configure({ mode: 'serial' });

for (const path of SUBJECTS) {
  test(`${path} holds the controls it held`, async ({ page, mock }) => {
    await page.goto(path);
    await expect(page.locator(TOOLBAR).first()).toBeVisible({ timeout: 15_000 });
    // Controls gated on the role matrix appear a moment after the toolbar: wait until it is drawn
    // from, or an admin's `+ Add` would be recorded as absent.
    await expect.poll(() => mock.served.includes('GET /api/v1/roles')).toBe(true);
    await page.waitForTimeout(300);

    const inventory = await page.locator(TOOLBAR).evaluateAll((bars) =>
      bars.map((bar) =>
        [...bar.children].map((el) => {
          const cls = [...el.classList].sort().join('.');
          const tag = el.tagName.toLowerCase();
          const name = `${tag}${cls ? `.${cls}` : ''}`;
          if (el.classList.contains('table-count')) return name;
          const text = (el as HTMLElement).innerText?.replace(/[0-9]/g, '').replace(/\s+/g, ' ').trim();
          return tag === 'button' || el.querySelector('button') ? `${name} "${text}"` : name;
        }),
      ),
    );
    seen[path] = inventory;
    if (!UPDATE) {
      expect(recorded[path], `${path} was never recorded — run with UPDATE_TOOLBAR_INVENTORY=1`).toBeDefined();
      expect(inventory).toEqual(recorded[path]);
    }
  });
}

test.afterAll(() => {
  if (UPDATE) writeFileSync(FILE, `${JSON.stringify({ ...recorded, ...seen }, null, 2)}\n`);
});
