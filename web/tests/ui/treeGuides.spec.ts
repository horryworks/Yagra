// SPDX-License-Identifier: AGPL-3.0-only
// The inventory tree's branch lines (ADR-171, ADR-052 Tier1).
//
// Where each line goes, and which ones are lit, is decided in `nodeTreeGuides.ts` and unit-tested
// there. What only a browser can say: that the cells reach the rows they were computed for (the
// index is threaded through the virtualizer), that a lit line is actually painted in another
// colour, that "N selected" survives closing the folder, and that the pinned band appears over a
// scrolled tree, takes the operator back, and does not become a second `.sel`.
//
// 🚨 Colour is read with `getComputedStyle`, never `isVisible()` — a guide with no colour is
// "visible" to Playwright and to nobody else.

import type { Locator, Page } from '@playwright/test';
import { expect, test } from '../support/app';
import { BOOTSTRAP_OVERRIDES } from '../support/bootstrap';
import { defaultBodyFor, MOCK_PREFIX, type Json } from '../support/openapi';

const R = '00000000-0000-4000-8000-0000000001a1';
const A = '00000000-0000-4000-8000-0000000001a2';
const B = '00000000-0000-4000-8000-0000000001a3';
const A_COUNT = 40;
const aName = (i: number) => `${MOCK_PREFIX}site-a-${String(i).padStart(2, '0')}`;
const bName = (i: number) => `${MOCK_PREFIX}site-b-${i}`;
const nodeId = (g: string, i: number) =>
  `00000000-0000-4000-8000-${g}${String(i).padStart(10, '0')}`;

function groups(): Json {
  const [template] = defaultBodyFor('/api/v1/node-groups') as Record<string, Json>[];
  const g = (id: string, name: string, parent_id: string | null, sort_order: number) => ({
    ...template,
    id,
    name: `${MOCK_PREFIX}${name}`,
    parent_id,
    group_type: 'generic',
    origin: null,
    sort_order,
  });
  return [g(R, 'region', null, 1), g(A, 'site-a', R, 1), g(B, 'site-b', R, 2)] as unknown as Json;
}

function summary(): Json {
  const empty = { critical: 0, maintenance: 0, ok: 0, unknown: 0, unreachable: 0, warning: 0 };
  return {
    groups: { [R]: { ...empty, ok: A_COUNT + 2 }, [A]: { ...empty, ok: A_COUNT }, [B]: { ...empty, ok: 2 } },
  } as unknown as Json;
}

function members(url: URL): Json {
  const body = defaultBodyFor('/api/v1/nodes/by-group') as {
    nodes: Record<string, Json>[];
    answered?: string[];
  };
  const row = (id: string, name: string, group_id: string, sort_order: number) => ({
    ...body.nodes[0],
    id,
    name,
    group_id,
    sort_order,
  });
  const byGroup: Record<string, Record<string, Json>[]> = {
    [A]: Array.from({ length: A_COUNT }, (_, i) => row(nodeId('a', i), aName(i), A, i + 1)),
    [B]: [row(nodeId('b', 0), bName(0), B, 1), row(nodeId('b', 1), bName(1), B, 2)],
  };
  const batch = url.searchParams.get('groups');
  if (batch) {
    const asked = batch.split(',').filter(Boolean);
    return {
      nodes: asked.flatMap((g) => byGroup[g] ?? []),
      truncated: false,
      answered: asked,
    } as unknown as Json;
  }
  delete body.answered;
  return { ...body, nodes: byGroup[url.searchParams.get('group') ?? ''] ?? [] } as unknown as Json;
}

test.use({
  mockConfig: {
    overrides: {
      ...BOOTSTRAP_OVERRIDES,
      '/api/v1/node-groups': groups(),
      '/api/v1/fleet/group-summary': summary(),
      '/api/v1/nodes/by-group': members,
    },
  },
});

const nodeRow = (page: Page, name: string) => page.locator('.ntree-node').filter({ hasText: name });
const groupRow = (page: Page, name: string) =>
  page.locator('.ntree-grow').filter({ has: page.locator('.ntree-grp-name', { hasText: name }) });

/** The resolved colour a guide's part is painted in, and the accent it should be compared with. */
const partColour = (guide: Locator, part: '--up' | '--down' | '--stub') =>
  guide.evaluate((el, p) => getComputedStyle(el).getPropertyValue(p).trim(), part);
const accent = (page: Page) =>
  page.locator('.ntree-body').evaluate((el) => {
    // Resolve the token the way the stylesheet does, through a probe element's colour.
    const probe = document.createElement('span');
    probe.style.color = 'var(--accent-color)';
    el.appendChild(probe);
    const c = getComputedStyle(probe).color;
    probe.remove();
    return c;
  });
/** A CSS colour string resolved to `rgb(...)`, so a hex and an rgb spelling compare equal. */
const asRgb = (page: Page, colour: string) =>
  page.evaluate((c) => {
    const probe = document.createElement('span');
    probe.style.color = c;
    document.body.appendChild(probe);
    const out = getComputedStyle(probe).color;
    probe.remove();
    return out;
  }, colour);

/** The tree is virtualized: a row below the fold is not in the DOM until the pane scrolls to it. */
const scrollTree = (page: Page, where: 'top' | 'bottom') =>
  page.locator('.ntree-body').evaluate((el, w) => {
    el.scrollTop = w === 'top' ? 0 : el.scrollHeight;
  }, where);

async function open(page: Page) {
  await page.goto('/nodes');
  await expect(nodeRow(page, aName(0))).toBeVisible();
}

test('each row carries its own branch: a tee while siblings follow, an elbow on the last', async ({
  page,
}) => {
  await open(page);
  // site-b-0 hangs in site-b with a sibling below it; site-b is the region's last child, so the
  // region's column carries nothing on its rows.
  await scrollTree(page, 'bottom');
  await expect(nodeRow(page, bName(0)).locator('.ntree-guide')).toHaveCount(1);
  await expect(nodeRow(page, bName(0)).locator('.ntree-guide-tee')).toHaveCount(1);
  await expect(nodeRow(page, bName(1)).locator('.ntree-guide-elbow')).toHaveCount(1);
  // site-a is followed by site-b, so every row inside site-a carries the region's line too.
  await scrollTree(page, 'top');
  await expect(nodeRow(page, aName(0)).locator('.ntree-guide-pipe')).toHaveCount(1);
  await expect(groupRow(page, 'site-a').locator('.ntree-guide-tee')).toHaveCount(1);
  // The region is at the top level and joins nothing.
  await expect(groupRow(page, 'region').locator('.ntree-guide')).toHaveCount(0);
});

test('the selected row’s branch is painted in the accent, and only that branch', async ({ page }) => {
  await open(page);
  const target = nodeRow(page, bName(0));
  await scrollTree(page, 'bottom');
  await target.click();
  await expect(page.locator('.ntree-row.sel')).toHaveCount(1);

  const want = await accent(page);
  const own = target.locator('.ntree-guide').first();
  expect(await asRgb(page, await partColour(own, '--up'))).toBe(want);
  expect(await asRgb(page, await partColour(own, '--stub'))).toBe(want);
  // Its sibling below is not on the path: the line past the selected row stays plain.
  expect(await asRgb(page, await partColour(own, '--down'))).not.toBe(want);
  // site-b's own connector into the region is on the path.
  const parent = groupRow(page, 'site-b').locator('.ntree-guide').first();
  expect(await asRgb(page, await partColour(parent, '--stub'))).toBe(want);
  // A lit line is drawn wider, so it reads as a path and not as a recoloured hairline.
  expect(await own.evaluate((el) => getComputedStyle(el, '::before').width)).toBe('2px');
  // …and site-b-1, below the selection, is not lit at all.
  const other = nodeRow(page, bName(1)).locator('.ntree-guide').first();
  expect(await asRgb(page, await partColour(other, '--up'))).not.toBe(want);
});

test('Ctrl-picked nodes in two folders light faintly and count on every folder above them', async ({
  page,
}) => {
  await open(page);
  await nodeRow(page, aName(0)).click({ modifiers: ['Control'] });
  await scrollTree(page, 'bottom');
  await nodeRow(page, bName(1)).click({ modifiers: ['Control'] });
  // site-a-00 is scrolled out of the virtualized DOM by now, so count the pick that is on screen;
  // the region's count below is what proves both were taken.
  await expect(nodeRow(page, bName(1))).toHaveClass(/checked/);

  const want = await accent(page);
  const soft = nodeRow(page, bName(1)).locator('.ntree-guide').first();
  const softUp = await asRgb(page, await partColour(soft, '--up'));
  // site-a's last row joins site-a with an elbow no picked branch passes through (site-a-00's branch
  // stops at the top of the folder): the plain colour. Not site-b-0 — the line down to site-b-1 runs
  // through it, and is lit there on purpose.
  const plain = await asRgb(
    page,
    await partColour(nodeRow(page, aName(A_COUNT - 1)).locator('.ntree-guide-elbow'), '--up'),
  );
  expect(softUp, 'a picked node’s branch is not lit').not.toBe(plain);
  expect(softUp, 'a picked node’s branch looks like the selected one').not.toBe(want);

  // Every folder above a picked node says how many, the region both.
  await expect(groupRow(page, 'site-b').locator('.ntree-pick')).toContainText('1');
  await scrollTree(page, 'top');
  await expect(groupRow(page, 'region').locator('.ntree-pick')).toContainText('2');

  // Closing site-a hides its rows, and with them the branch — the count is what is left.
  await groupRow(page, 'site-a').locator('.ntree-twisty').click();
  await expect(nodeRow(page, aName(0))).toHaveCount(0);
  await expect(groupRow(page, 'site-a').locator('.ntree-pick')).toContainText('1');
});

test('scrolled into a long folder, its parents are pinned on top and take you back', async ({
  page,
}) => {
  await open(page);
  const scroller = page.locator('.ntree-body');
  const room = await scroller.evaluate((el) => el.scrollHeight - el.clientHeight);
  expect(room, 'the tree does not scroll — this fixture cannot see the band').toBeGreaterThan(300);

  // At the top nothing has scrolled away, so nothing is pinned.
  await expect(page.locator('.ntree-sticky-row')).toHaveCount(0);

  await scroller.evaluate((el) => {
    el.scrollTop = 20 * 30;
  });
  const band = page.locator('.ntree-sticky-row');
  await expect(band).toHaveCount(2);
  await expect(band.nth(0)).toContainText('region');
  await expect(band.nth(1)).toContainText('site-a');
  // The band hangs over the rows: its top is the scroller's top.
  const [bandTop, bodyTop] = await Promise.all([
    band.nth(0).evaluate((el) => el.getBoundingClientRect().top),
    scroller.evaluate((el) => el.getBoundingClientRect().top),
  ]);
  expect(Math.abs(bandTop - bodyTop)).toBeLessThan(2);

  await band.nth(1).click();
  await expect.poll(() => new URL(page.url()).searchParams.get('sel')).toBe(`group:${A}`);
  // The band is a lookalike, never a second selected row (`treeDeselect.spec.ts` counts this).
  await expect(page.locator('.ntree-row.sel')).toHaveCount(1);
  await expect(groupRow(page, 'site-a')).toBeInViewport();
});

/**
 * The scroller's position on each of the next `frames` animation frames, starting `afterMs` after
 * the call. A tree that has come to rest gives one value; the defect below gave two, alternating.
 */
const scrollTrace = (page: Page, afterMs: number, frames: number) =>
  page.locator('.ntree-body').evaluate(
    (el, [wait, n]) =>
      new Promise<number[]>((resolve) => {
        const out: number[] = [];
        const step = () => {
          out.push(el.scrollTop);
          if (out.length < n) requestAnimationFrame(step);
          else resolve(out);
        };
        setTimeout(() => requestAnimationFrame(step), wait);
      }),
    [afterMs, frames] as const,
  );

// 🚨 ADR-171 Inc.2. The scroll the tree makes itself used to aim at "the row, below the band as it
// is HERE" — and moving there changed the band, which changed the aim, which moved it back. The
// library re-aims every frame for five seconds, so near a folder boundary the pane shook between two
// positions. The test above stayed green throughout: the row is in the viewport at both of them.
test('pressing a pinned folder scrolls once and stays put', async ({ page }) => {
  await open(page);
  const scroller = page.locator('.ntree-body');
  await scroller.evaluate((el) => {
    el.scrollTop = 20 * 30;
  });
  await expect(page.locator('.ntree-sticky-row')).toHaveCount(2);
  await page.locator('.ntree-sticky-row').nth(1).click();
  const trace = await scrollTrace(page, 300, 40);
  expect(new Set(trace).size, `the pane kept moving: ${trace.join(' ')}`).toBe(1);
  await expect(groupRow(page, 'site-a')).toBeInViewport();
});

test('an arrow key onto a row under the band scrolls once and stays put', async ({ page }) => {
  await open(page);
  const scroller = page.locator('.ntree-body');
  // Two rows down: site-a-00 and site-a-01 sit under the two-row band, site-a-02 is the first in
  // the clear.
  await scroller.evaluate((el) => {
    el.scrollTop = 2 * 30;
  });
  await expect(page.locator('.ntree-sticky-row')).toHaveCount(2);
  await nodeRow(page, aName(2)).click();
  await expect(nodeRow(page, aName(2))).toHaveClass(/sel/);
  await page.keyboard.press('ArrowUp');
  const trace = await scrollTrace(page, 300, 40);
  expect(new Set(trace).size, `the pane kept moving: ${trace.join(' ')}`).toBe(1);
  // …and where it stopped, the row the cursor moved to is not hidden under the band.
  const rowTop = await nodeRow(page, aName(1)).evaluate((el) => el.getBoundingClientRect().top);
  const bandBottom = await page.evaluate(() => {
    const band = [...document.querySelectorAll('.ntree-sticky-row')];
    const last = band[band.length - 1];
    return last ? last.getBoundingClientRect().bottom : -Infinity;
  });
  expect(rowTop).toBeGreaterThanOrEqual(bandBottom - 1);
});
