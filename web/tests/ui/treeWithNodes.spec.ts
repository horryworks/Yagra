// SPDX-License-Identifier: AGPL-3.0-only
// "Folders with nodes only" on the inventory tree (ADR-159).
//
// Why Tier1: which folders the switch keeps is decided in `flattenTree`/`foldersWithNodes`, both
// unit-tested. What only a browser proves is the wiring around them, none of which Vitest runs: the
// button beside Pinned only, the page holding the switch, and the tree reading the per-folder counts
// that arrive from `/fleet/group-summary` rather than from the members it has loaded.
//
// Its own tree rather than `twoLevelTree`: the point needs a folder whose only nodes sit one level
// down (it must stay) beside a folder with nothing anywhere below it (it must go), and that fixture
// has neither.

import { expect, test } from '../support/app';
import { BOOTSTRAP_OVERRIDES } from '../support/bootstrap';
import { defaultBodyFor, MOCK_PREFIX, type Json } from '../support/openapi';

const REGION = '00000000-0000-4000-8000-0000000000e1';
const SITE = '00000000-0000-4000-8000-0000000000e2';
const EMPTY = '00000000-0000-4000-8000-0000000000e3';

const label = (s: string) => `${MOCK_PREFIX}${s}`;

function folders(): Json {
  const [template] = defaultBodyFor('/api/v1/node-groups') as Record<string, Json>[];
  const folder = (id: string, name: string, parent: string | null, order: number) => ({
    ...template,
    id,
    name: label(name),
    parent_id: parent,
    group_type: 'generic',
    sort_order: order,
  });
  return [
    folder(REGION, 'region', null, 1),
    folder(SITE, 'site', REGION, 1),
    folder(EMPTY, 'nowhere', null, 2),
  ] as unknown as Json;
}

function member(id: string, name: string, group: string): Record<string, Json> {
  const body = defaultBodyFor('/api/v1/nodes/by-group') as { nodes: Record<string, Json>[] };
  return { ...body.nodes[0], id, name: label(name), group_id: group, sort_order: 1, state: 'ok' };
}

const MEMBERS: Record<string, Record<string, Json>[]> = {
  [SITE]: [member('00000000-0000-4000-8000-0000000000e4', 'sw1', SITE)],
};

/** Both forms `/nodes/by-group` answers — the batch form must echo what it covered (ADR-125). */
function byGroup(url: URL): Json {
  const batch = url.searchParams.get('groups');
  if (batch) {
    const asked = batch.split(',').filter(Boolean);
    return {
      nodes: asked.flatMap((g) => MEMBERS[g] ?? []),
      truncated: false,
      answered: asked,
    } as unknown as Json;
  }
  const one = url.searchParams.get('group');
  return { nodes: (one && MEMBERS[one]) || [], truncated: false } as unknown as Json;
}

/** The rollup the switch reads. 🚨 An empty folder is ABSENT here, not present with zeros — that is
 *  what the server sends (`fleet.rs::group_summary`), and the fixture has to say the same thing. */
function summary(): Json {
  const empty = { critical: 0, maintenance: 0, ok: 0, unknown: 0, unreachable: 0, warning: 0 };
  return { groups: { [SITE]: { ...empty, ok: 1 } } } as unknown as Json;
}

test.use({
  mockConfig: {
    overrides: {
      ...BOOTSTRAP_OVERRIDES,
      '/api/v1/node-groups': folders(),
      '/api/v1/nodes/by-group': byGroup,
      '/api/v1/fleet/group-summary': summary(),
    },
  },
});

type Page = import('@playwright/test').Page;

const row = (page: Page, name: string) =>
  page.locator('.ntree-body').getByText(label(name), { exact: true });
const withNodesOnly = (page: Page) =>
  page.getByRole('button', { name: 'With nodes' });

test('the switch drops a folder with nothing below it and keeps the one above the node', async ({
  page,
}) => {
  await page.goto('/nodes');
  // Browse first, so a later "gone" can only be the switch's doing.
  for (const name of ['region', 'site', 'nowhere', 'sw1']) {
    await expect(row(page, name)).toHaveCount(1);
  }

  await withNodesOnly(page).click();
  await expect(withNodesOnly(page)).toHaveAttribute('aria-pressed', 'true');
  await expect(row(page, 'nowhere'), 'a folder with no node anywhere below it').toHaveCount(0);
  // The folder above the node has no members of its own and must stay, or the node it holds has
  // nowhere to be drawn.
  await expect(row(page, 'region')).toHaveCount(1);
  await expect(row(page, 'site')).toHaveCount(1);
  await expect(row(page, 'sw1')).toHaveCount(1);

  await withNodesOnly(page).click();
  await expect(withNodesOnly(page)).toHaveAttribute('aria-pressed', 'false');
  await expect(row(page, 'nowhere')).toHaveCount(1);
});

test('"clear all filters" appears with it and switches it off', async ({ page }) => {
  await page.goto('/nodes');
  await withNodesOnly(page).click();
  const clear = page.getByRole('button', { name: /clear all filters/i });
  await expect(clear, 'the tree is narrowed, so the reset has to be on screen').toHaveCount(1);
  await clear.click();
  await expect(withNodesOnly(page)).toHaveAttribute('aria-pressed', 'false');
  await expect(row(page, 'nowhere')).toHaveCount(1);
});
