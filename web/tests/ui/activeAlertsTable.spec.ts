// SPDX-License-Identifier: AGPL-3.0-only
// Alerts ▸ Active on the shared table with History's columns (ADR-243).
//
// Why Tier1: which notes an alert carries and which alerts a scope keeps are unit-tested
// (`alertNotes.test.ts`, `activeAlertFilters.test.ts`). What Vitest cannot run is the table
// drawing History's header row, the Notes column stacking its notes so the row grows, and a folder
// scope narrowing the list once `node-names` has said where each alerting node is filed.
//
// Every body is the generated one, patched — a hand-written one would be a second copy of the
// contract (testing.md).

import type { Request } from '@playwright/test';
import { expect, test } from '../support/app';
import { BOOTSTRAP_OVERRIDES } from '../support/bootstrap';
import { defaultBodyFor, MOCK_PREFIX, type Json } from '../support/openapi';

const id = (n: number) => `00000000-0000-4000-8000-${String(n).padStart(12, '0')}`;
const SITE = id(1);
const FLOOR = id(2); // beneath SITE
const ELSEWHERE = id(3);
const node = (i: number) => id(1000 + i);

/** Where each node is filed: 0 and 1 in the site, 2 on its floor, 3 elsewhere. */
const FOLDER_OF = [SITE, SITE, FLOOR, ELSEWHERE];

function folders(): Json {
  const [t] = defaultBodyFor('/api/v1/node-groups') as Record<string, Json>[];
  const g = (gid: string, name: string, parent: string | null) => ({
    ...t,
    id: gid,
    name: `${MOCK_PREFIX}${name}`,
    parent_id: parent,
    origin: null,
  });
  return [g(SITE, 'site-a', null), g(FLOOR, 'floor-3', SITE), g(ELSEWHERE, 'site-b', null)] as unknown as Json;
}

/** Four alerts, one per node. Node 0's carries three notes: an upstream cause, flapping, and the
 *  neighbour hold — enough that a row that did not grow would cut two of them off. */
function alerts(): Json {
  const [t] = defaultBodyFor('/api/v1/alerts') as Record<string, Json>[];
  const a = (i: number, severity: string, over: Record<string, Json> = {}) => ({
    ...t,
    node: node(i),
    check: `check-${i}`,
    subject_kind: 'node',
    subject_name: null,
    severity,
    state: severity === 'critical' ? 'unreachable' : 'warning',
    metric: `metric_${i}`,
    breach: null,
    ifindex: null,
    row: null,
    root_cause: null,
    flapping: false,
    notify_held_until: null,
    at_unix_ms: Date.now() - (i + 1) * 60_000,
    ...over,
  });
  return [
    a(0, 'critical', { root_cause: node(1), flapping: true, notify_held_until: Date.now() + 60_000 }),
    a(1, 'critical'),
    a(2, 'warning'),
    a(3, 'warning'),
  ] as unknown as Json;
}

function names(_url: URL, request: Request): Json {
  const ids = (request.postDataJSON() as { ids: string[] }).ids;
  return ids.map((nid) => {
    const i = Number(nid.slice(-4)) - 1000;
    return { id: nid, name: `${MOCK_PREFIX}sw-${i}`, group_id: FOLDER_OF[i] ?? null };
  }) as unknown as Json;
}

test.use({
  mockConfig: {
    overrides: {
      ...BOOTSTRAP_OVERRIDES,
      '/api/v1/node-groups': folders(),
      '/api/v1/alerts': alerts(),
      '/api/v1/node-names': names,
      '/api/v1/held-notifications': [] as unknown as Json,
    },
  },
});

test('the table has History’s header row and funnels, and severity reads as a word', async ({
  page,
  errors,
}) => {
  await page.goto('/alerts');
  const head = page.locator('.dt-head');
  for (const h of ['Severity', 'Node', 'What', 'State', 'Notes', 'When']) {
    await expect(head).toContainText(h);
  }
  // Funnels beside the headers, as History has, and no toolbar funnel standing in for them.
  await expect(head.locator('.hf-trigger')).toHaveCount(4);
  await expect(page.locator('.mfilt-menu')).toHaveCount(0);
  await expect(page.locator('.dt-row')).toHaveCount(4);
  await expect(page.locator('.dt-row').first()).toContainText('Critical');
  expect(errors.uncaught).toEqual([]);
});

test('the Notes column stacks its notes and the row grows to hold them', async ({ page }) => {
  await page.goto('/alerts');
  const row = page.locator('.dt-row', { hasText: `${MOCK_PREFIX}sw-0` }).first();
  const notes = row.locator('.alert-notes > *');
  await expect(notes).toHaveCount(3);
  await expect(row.locator('.alert-notes')).toContainText(`${MOCK_PREFIX}sw-1`);
  // One per line: three distinct tops. A count of three would also pass side by side.
  const tops = await notes.evaluateAll((els) =>
    els.map((e) => Math.round(e.getBoundingClientRect().top)),
  );
  expect(new Set(tops).size).toBe(3);
  // The row holds them: the last note ends inside it, and a row with no notes stays 44px.
  const box = (await row.boundingBox())!;
  const bottom = await notes.evaluateAll((els) =>
    Math.max(...els.map((e) => e.getBoundingClientRect().bottom)),
  );
  expect(bottom).toBeLessThanOrEqual(box.y + box.height);
  const plain = page.locator('.dt-row', { hasText: `${MOCK_PREFIX}sw-3` }).first();
  expect(Math.round((await plain.boundingBox())!.height)).toBe(44);
});

test('a folder scope keeps the alerts filed in it or beneath it', async ({ page }) => {
  await page.goto(`/alerts?group_id=${SITE}`);
  // Nodes 0, 1 (the site) and 2 (its floor); not 3, filed elsewhere.
  await expect(page.locator('.dt-row')).toHaveCount(3);
  await expect(page.locator('.dt-body')).not.toContainText(`${MOCK_PREFIX}sw-3`);
  // "Clear all filters" takes the scope off with the columns.
  await page.getByRole('button', { name: /Clear all filters/ }).click();
  await expect(page).not.toHaveURL(/group_id=/);
  await expect(page.locator('.dt-row')).toHaveCount(4);
});

test('a node scope keeps that node’s alerts', async ({ page }) => {
  await page.goto(`/alerts?node_id=${node(2)}`);
  await expect(page.locator('.dt-row')).toHaveCount(1);
  await expect(page.locator('.dt-row')).toContainText(`${MOCK_PREFIX}sw-2`);
});
