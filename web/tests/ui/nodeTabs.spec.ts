// SPDX-License-Identifier: AGPL-3.0-only
// Node detail tabs, generated from the declaration that owns them (ADR-052 決定 7, 出典 2).
//
// Nothing here is written down twice. `tabs.ts` says which kinds see which tab; this asks the
// running app the same question and demands the same answer. Add a tab, or change one `kinds`
// list, and these tests change with it — there is no expectation to update, which is the whole
// point of taking expectations from declarations instead of prose.
//
// WHY THIS SCREEN. ADR-031's bug was "the Flow button renders, and clicking it bounces back to
// Overview" — the tab list and the tab *body* were two lists, and only one of them knew about
// Flow. Rendering is therefore not the assertion: **clicking is**. A test that only counted the
// buttons would have passed throughout that bug's life.

import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import { expect, test } from '../support/app';
import { BOOTSTRAP_OVERRIDES } from '../support/bootstrap';
import { defaultBodyFor, type Json } from '../support/openapi';
import { NODE_DETAIL_TABS, visibleNodeDetailTabs } from '../../src/components/NodeDetail/tabs';
import { NODE_KINDS, type NodeKind } from '../../src/types/api';

const NODE_ID = '00000000-0000-4000-8000-0000000000aa';

/** Tab labels read from the EN locale — the same file the app renders from, so a renamed label
 *  cannot make this test disagree with the screen. Read rather than imported: JSON import
 *  attributes differ between the two module systems this repo's tooling uses. */
const TAB_LABELS = JSON.parse(
  readFileSync(join(process.cwd(), 'src/locales/en/nodes.json'), 'utf8'),
).tabs as Record<string, string>;

function nodeOfKind(kind: NodeKind): Json {
  const body = defaultBodyFor(`/api/v1/nodes/${NODE_ID}`) as { kind: NodeKind };
  body.kind = kind;
  return body as unknown as Json;
}

for (const kind of NODE_KINDS) {
  test.describe(`a ${kind} node`, () => {
    test.use({
      mockConfig: {
        overrides: { ...BOOTSTRAP_OVERRIDES, '/api/v1/nodes/{node_id}': () => nodeOfKind(kind) },
      },
    });

    test('shows exactly the tabs its kind declares, in declared order', async ({ page }) => {
      await page.goto(`/nodes/${NODE_ID}`);
      const tabs = page.getByRole('tab');
      await expect(tabs.first()).toBeVisible();

      const expected = visibleNodeDetailTabs(kind).map((t) => TAB_LABELS[t]);
      // Order matters: `visibleNodeDetailTabs` filters NODE_DETAIL_TABS, so the declaration fixes
      // the sequence too. A screen that showed the right set in a different order would be a
      // different screen from the one the declaration describes.
      await expect(tabs).toHaveText(expected.map((label) => new RegExp(`^${label}`)));
    });

    test('opens every tab it offers, and none it does not', async ({ page }) => {
      for (const tab of NODE_DETAIL_TABS) {
        const allowed = visibleNodeDetailTabs(kind).includes(tab);
        await page.goto(`/nodes/${NODE_ID}?tab=${tab}`);
        await expect(page.getByRole('tab').first()).toBeVisible();

        // Deep-linking is the harsher half of the same contract: `resolveNodeDetailTab` must send
        // a URL for a tab this kind cannot show back to Overview, and must leave the others alone.
        const selected = page.getByRole('tab', { selected: true });
        await expect(
          selected,
          allowed
            ? `?tab=${tab} should open ${tab} on a ${kind} node`
            : `?tab=${tab} is not offered to a ${kind} node and must fall back to Overview`,
        ).toHaveText(new RegExp(`^${TAB_LABELS[allowed ? tab : 'overview']}`));
      }
    });
  });
}

test.describe('clicking a tab', () => {
  test.use({
    mockConfig: {
      overrides: { ...BOOTSTRAP_OVERRIDES, '/api/v1/nodes/{node_id}': () => nodeOfKind('device') },
    },
  });

  test('selects it and keeps it selected — the ADR-031 regression', async ({ page, errors }) => {
    await page.goto(`/nodes/${NODE_ID}`);
    for (const tab of visibleNodeDetailTabs('device')) {
      const button = page.getByRole('tab', { name: new RegExp(`^${TAB_LABELS[tab]}`) });
      await button.click();
      // The bug was a bounce, so re-read after the click settles rather than trusting the class
      // that the click itself set.
      await expect(button, `${tab} bounced back after being clicked`).toHaveAttribute(
        'aria-selected',
        'true',
      );
      await expect(page).toHaveURL(new RegExp(`[?&]tab=${tab}\\b`));
    }
    expect(errors.uncaught).toEqual([]);
  });
});
