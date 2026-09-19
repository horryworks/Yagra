// SPDX-License-Identifier: AGPL-3.0-only
// Dragging a node INTO a folder that sits below a node (ADR-162 増分 3, ADR-052 Tier1).
//
// The layout ADR-162 made possible: inside one folder, `node · folder · node`. ADR-162 増分 2 draws
// the destination as a real row — the insertion slot — which pushes everything below it down by
// one row height. "Into this folder" draws no slot (決定 4). So coming down from the node above:
//
//   over the node's lower half  → `after node`  → slot inserted directly ABOVE the folder
//   pointer reaches the folder  → `inside F`    → slot removed → the folder jumps UP by one row
//
// …and the pointer, which did not move, is now over the row BELOW the folder. Letting go there
// writes into the parent, not into the folder that was outlined a moment before.
//
// 🚨 **Why the other drag specs never saw it.** Their helpers re-read `getBoundingClientRect()` of a
// named row immediately before each event, so the event always lands on the row they named — a
// question a browser never asks. This one keeps the pointer at fixed screen coordinates and sends
// each event to whatever `elementFromPoint` finds there, which is what a browser does.

import type { Page } from '@playwright/test';
import { expect, test } from '../support/app';
import { BOOTSTRAP_OVERRIDES } from '../support/bootstrap';
import { defaultBodyFor, MOCK_PREFIX, type Json } from '../support/openapi';

const P = '00000000-0000-4000-8000-0000000000d1';
const F = '00000000-0000-4000-8000-0000000000d2';
const N1 = '00000000-0000-4000-8000-0000000000e1';
const N2 = '00000000-0000-4000-8000-0000000000e2';
const N3 = '00000000-0000-4000-8000-0000000000e3';
const F_NAME = `${MOCK_PREFIX}inner`;

function groups(): Json {
  const [template] = defaultBodyFor('/api/v1/node-groups') as Record<string, Json>[];
  return [
    { ...template, id: P, name: `${MOCK_PREFIX}outer`, parent_id: null, group_type: 'generic', sort_order: 1 },
    // Between the two nodes below: folders and nodes share one ordered list (ADR-162).
    { ...template, id: F, name: F_NAME, parent_id: P, group_type: 'generic', sort_order: 2 },
  ] as unknown as Json;
}

function summary(): Json {
  const empty = { critical: 0, maintenance: 0, ok: 0, unknown: 0, unreachable: 0, warning: 0 };
  return { groups: { [P]: { ...empty, ok: 3 }, [F]: empty } } as unknown as Json;
}

function members(url: URL): Json {
  const body = defaultBodyFor('/api/v1/nodes/by-group') as {
    nodes: Record<string, Json>[];
    answered?: string[];
  };
  const row = (id: string, name: string, sort_order: number) => ({
    ...body.nodes[0],
    id,
    name: `${MOCK_PREFIX}${name}`,
    group_id: P,
    sort_order,
  });
  const inP = [row(N1, 'above', 1), row(N2, 'below', 3), row(N3, 'carried', 4)];
  const batch = url.searchParams.get('groups');
  if (batch) {
    const asked = batch.split(',').filter(Boolean);
    return { nodes: asked.includes(P) ? inP : [], truncated: false, answered: asked } as unknown as Json;
  }
  delete body.answered;
  return { ...body, nodes: url.searchParams.get('group') === P ? inP : [] } as unknown as Json;
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

interface MoveRequest {
  node_ids: string[];
  group_id: string | null;
  before?: string;
  after?: string;
}

async function captureMoves(page: Page): Promise<MoveRequest[]> {
  const seen: MoveRequest[] = [];
  await page.route('**/api/v1/nodes/move', async (route) => {
    const body = route.request().postDataJSON() as MoveRequest;
    seen.push(body);
    await route.fulfill({
      status: 200,
      contentType: 'application/json',
      body: JSON.stringify({ requested: body.node_ids.length, moved: body.node_ids.length }),
    });
  });
  return seen;
}

/** Send one drag event to whatever is under the pointer — the way a browser does. */
async function pointerEvent(page: Page, type: 'dragover' | 'drop', x: number, y: number) {
  return page.evaluate(
    ({ type, x, y }) => {
      const w = window as unknown as { __dt?: DataTransfer };
      const el = document.elementFromPoint(x, y);
      if (!el) throw new Error(`nothing under the pointer at ${x},${y}`);
      el.dispatchEvent(
        new DragEvent(type, { bubbles: true, cancelable: true, dataTransfer: w.__dt, clientX: x, clientY: y }),
      );
      return (el.closest('.ntree-row')?.textContent ?? '').trim().slice(0, 40);
    },
    { type, x, y },
  );
}

const rowByText = (page: Page, text: string) => page.locator('.ntree-row', { hasText: text });

test('a node dragged down onto a folder lands IN that folder, not in the row that slid under the pointer', async ({
  page,
}) => {
  const moves = await captureMoves(page);
  await page.goto('/nodes');
  await expect(rowByText(page, `${MOCK_PREFIX}above`)).toBeVisible();
  await expect(rowByText(page, F_NAME)).toBeVisible();

  // Pick the carried node up.
  await page.evaluate((text: string) => {
    const w = window as unknown as { __dt?: DataTransfer };
    w.__dt = new DataTransfer();
    const src = [...document.querySelectorAll('.ntree-node')].find((r) => r.textContent?.includes(text));
    if (!src) throw new Error('no drag source');
    src.dispatchEvent(new DragEvent('dragstart', { bubbles: true, cancelable: true, dataTransfer: w.__dt }));
  }, `${MOCK_PREFIX}carried`);

  // 1. Over the lower half of the node ABOVE the folder: "after that node". A slot appears.
  const above = await rowByText(page, `${MOCK_PREFIX}above`).boundingBox();
  if (!above) throw new Error('no box for the node above');
  const x = above.x + above.width / 2;
  await pointerEvent(page, 'dragover', x, above.y + above.height * 0.8);
  await expect(page.locator('.ntree-drop-slot')).toHaveCount(1);

  // 2. Carry on down to the folder — wherever it is NOW, with the slot pushing it down.
  const folder = await rowByText(page, F_NAME).boundingBox();
  if (!folder) throw new Error('no box for the folder');
  const y = folder.y + folder.height / 2;
  const over = await pointerEvent(page, 'dragover', x, y);
  expect(over).toContain(F_NAME);
  await expect(rowByText(page, F_NAME)).toHaveClass(/drop-inside/);

  // 3. Let go WITHOUT moving. What is under the pointer now is what the browser drops on.
  await pointerEvent(page, 'drop', x, y);

  await expect.poll(() => moves.length).toBe(1);
  // The operator was shown the folder outlined as the destination. That is where it must go.
  expect(moves[0].group_id).toBe(F);
});

test.describe('while a folder is still loading its members', () => {
  // The same tree, but the OUTER folder's members never arrive — so its "Loading nodes…" row stays
  // on screen, indented inside it, reading as its interior.
  test.use({
    mockConfig: {
      overrides: {
        ...BOOTSTRAP_OVERRIDES,
        '/api/v1/node-groups': groups(),
        '/api/v1/fleet/group-summary': summary(),
      },
      // A failed read draws the "could not load" row, which sits in the same place and had the
      // same defect: no `dragover`, so the browser refused the drop.
      failures: { '/api/v1/nodes/by-group': 503 },
    },
  });

  // ⚠️ This pins that the row ACCEPTS a drag, which was the defect. Where the drop then writes is
  // `dropAction` for an `inside` target, unit-tested in `nodeTreeDnd.test.ts`.
  test('the folder placeholder row accepts a drag — a browser allows a drop only where dragover was cancelled', async ({
    page,
  }) => {
    const moves = await captureMoves(page);
    await page.goto('/nodes');
    // The settled row. "Loading…" is drawn first and replaced when the read fails, so a locator
    // matching either one can be holding a row that is about to leave the DOM.
    const placeholder = page.locator('.ntree-failed').first();
    await expect(placeholder).toBeVisible();

    // Something to carry: any draggable row will do. The bootstrap has none here (every read of
    // members fails), so the drag is started on the inner folder — a folder INTO its sibling's
    // parent is a legal move, and what is under test is only that the placeholder accepts a drop.
    await page.evaluate((name: string) => {
      const w = window as unknown as { __dt?: DataTransfer };
      w.__dt = new DataTransfer();
      const src = [...document.querySelectorAll('.ntree-row')].find((r) => r.textContent?.includes(name));
      if (!src) throw new Error('no drag source');
      src.dispatchEvent(new DragEvent('dragstart', { bubbles: true, cancelable: true, dataTransfer: w.__dt }));
    }, F_NAME);

    const box = await placeholder.boundingBox();
    if (!box) throw new Error('no box for the placeholder');
    const x = box.x + box.width / 2;
    const y = box.y + box.height / 2;

    // A drop target is an element whose `dragover` was cancelled. That is the whole defect.
    const accepted = await page.evaluate(
      ({ x, y }) => {
        const w = window as unknown as { __dt?: DataTransfer };
        const el = document.elementFromPoint(x, y);
        if (!el) throw new Error('nothing under the pointer');
        const ev = new DragEvent('dragover', { bubbles: true, cancelable: true, dataTransfer: w.__dt, clientX: x, clientY: y });
        el.dispatchEvent(ev);
        return ev.defaultPrevented;
      },
      { x, y },
    );
    expect(accepted).toBe(true);
    expect(moves).toEqual([]); // hovering writes nothing
  });
});
