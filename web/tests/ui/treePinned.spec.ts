// SPDX-License-Identifier: AGPL-3.0-only
// Pinned only on the inventory tree (ADR-146).
//
// Why Tier1: which rows Pinned only keeps is decided in `lib/pins.ts` and `flattenTree`, both
// unit-tested. What only a browser proves is the wiring around them, none of which Vitest runs: the
// button beside Filter, the tree reading the switch, the pinned rows from `GET /pins` reaching a tree
// whose folders are loaded lazily, and the row menu offering the pin.
//
// Its own tree rather than `twoLevelTree`: "nothing beside the pin is shown" needs an unpinned
// sibling in the pinned node's folder and an unrelated folder elsewhere, and that fixture has neither.

import { expect, test } from '../support/app';
import { BOOTSTRAP_OVERRIDES } from '../support/bootstrap';
import { defaultBodyFor, MOCK_PREFIX, type Json } from '../support/openapi';

const REGION = '00000000-0000-4000-8000-0000000000d1';
const SITE = '00000000-0000-4000-8000-0000000000d2';
const ELSEWHERE = '00000000-0000-4000-8000-0000000000d3';

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
    folder(ELSEWHERE, 'elsewhere', null, 2),
  ] as unknown as Json;
}

function member(id: string, name: string, group: string, order: number): Record<string, Json> {
  const body = defaultBodyFor('/api/v1/nodes/by-group') as { nodes: Record<string, Json>[] };
  return { ...body.nodes[0], id, name: label(name), group_id: group, sort_order: order, state: 'ok' };
}

const PINNED = member('00000000-0000-4000-8000-0000000000d4', 'pinned', SITE, 1);
const MEMBERS: Record<string, Record<string, Json>[]> = {
  [SITE]: [PINNED, member('00000000-0000-4000-8000-0000000000d5', 'sibling', SITE, 2)],
  [ELSEWHERE]: [member('00000000-0000-4000-8000-0000000000d6', 'far', ELSEWHERE, 1)],
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

/** The rollup, so the tree asks for the folders' members at all (`twoLevelTree.groupSummary`). */
function summary(): Json {
  const empty = { critical: 0, maintenance: 0, ok: 0, unknown: 0, unreachable: 0, warning: 0 };
  return {
    groups: { [REGION]: empty, [SITE]: { ...empty, ok: 2 }, [ELSEWHERE]: { ...empty, ok: 1 } },
  } as unknown as Json;
}

const overrides = (pins: { group_ids: string[]; nodes: Record<string, Json>[] }) => ({
  mockConfig: {
    overrides: {
      ...BOOTSTRAP_OVERRIDES,
      '/api/v1/node-groups': folders(),
      '/api/v1/nodes/by-group': byGroup,
      '/api/v1/fleet/group-summary': summary(),
      '/api/v1/pins': pins as unknown as Json,
    },
  },
});

type Page = import('@playwright/test').Page;

const row = (page: Page, name: string) =>
  page.locator('.ntree-body').getByText(label(name), { exact: true });
const pinnedOnly = (page: Page) => page.getByRole('button', { name: 'Pinned only' });

test.describe('a pinned node', () => {
  test.use(overrides({ group_ids: [], nodes: [PINNED] }));

  test('Pinned only keeps the pinned node and the folders above it, and nothing else', async ({
    page,
  }) => {
    await page.goto('/nodes');
    // Browse first, so a later "gone" can only be the switch's doing.
    await expect(row(page, 'sibling')).toHaveCount(1);
    await expect(row(page, 'far')).toHaveCount(1);
    await expect(page.locator('.ntree-body .ntree-pin'), 'the mark is on the pinned row only').toHaveCount(1);

    await pinnedOnly(page).click();
    await expect(pinnedOnly(page)).toHaveAttribute('aria-pressed', 'true');
    for (const kept of ['region', 'site', 'pinned']) await expect(row(page, kept)).toHaveCount(1);
    await expect(row(page, 'sibling'), 'an unpinned node beside the pin is still shown').toHaveCount(0);
    await expect(row(page, 'elsewhere')).toHaveCount(0);
    await expect(row(page, 'far')).toHaveCount(0);

    await pinnedOnly(page).click();
    await expect(pinnedOnly(page)).toHaveAttribute('aria-pressed', 'false');
    await expect(row(page, 'far')).toHaveCount(1);
  });

  test('the row menu offers Unpin on the pinned node and Pin on another', async ({ page }) => {
    await page.goto('/nodes');
    await row(page, 'pinned').click({ button: 'right' });
    await expect(page.getByRole('menu').getByRole('button', { name: 'Unpin', exact: true })).toHaveCount(1);
    await page.keyboard.press('Escape');
    await row(page, 'sibling').click({ button: 'right' });
    await expect(page.getByRole('menu').getByRole('button', { name: 'Pin', exact: true })).toHaveCount(1);
  });
});

test.describe('a pinned folder', () => {
  test.use(overrides({ group_ids: [ELSEWHERE], nodes: [] }));

  test('Pinned only keeps the folder with everything in it', async ({ page }) => {
    await page.goto('/nodes');
    await expect(row(page, 'pinned')).toHaveCount(1);

    await pinnedOnly(page).click();
    await expect(row(page, 'elsewhere')).toHaveCount(1);
    await expect(row(page, 'far')).toHaveCount(1);
    await expect(row(page, 'region')).toHaveCount(0);
    await expect(row(page, 'pinned')).toHaveCount(0);
  });
});

test.describe('nothing pinned', () => {
  test.use(overrides({ group_ids: [], nodes: [] }));

  test('Pinned only says how to pin instead of drawing an empty tree', async ({ page }) => {
    await page.goto('/nodes');
    await expect(row(page, 'far')).toHaveCount(1);
    await pinnedOnly(page).click();
    await expect(page.locator('.ntree-body').getByText('Nothing is pinned yet', { exact: false })).toHaveCount(1);
  });
});
