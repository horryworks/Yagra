// SPDX-License-Identifier: AGPL-3.0-only
// Topology ▸ Dependencies while comparing the two graphs (ADR-200 Inc.23).
//
// The walk opens this screen in the hand-authored mode, where the comparison columns are not drawn,
// so nothing else looks at them. Two things here must not be lost when prose goes:
//
// - `Only derived` is the direction that can suppress a real outage. Its hover sentence became a
//   pressable pill; this checks the press opens it.
// - Switching suppression onto the derived graph now asks first, and the question carries that same
//   warning. A switch that went straight through would put it back to a sentence nobody reads.

import { expect, test } from '../support/app';
import { BOOTSTRAP_OVERRIDES } from '../support/bootstrap';
import { defaultBodyFor, instanceOf, type Json } from '../support/openapi';

/** A comparison in which one node gains an upstream only the derivation found. */
function comparing(): Json {
  const topology = defaultBodyFor('/api/v1/topology') as unknown as { nodes: { id: string }[] };
  const [child, parent] = topology.nodes.map((n) => n.id);
  const base = instanceOf('TopologyShadow') as Record<string, Json>;
  return {
    ...base,
    mode: 'shadow',
    only_in_manual: [],
    only_in_derived: [{ child, parent }],
    opted_out: [],
    unresolved_pools: [],
    unresolved_pollers: [],
    would_suppress: [instanceOf('ShadowAlert')],
    would_unsuppress: [],
  } as unknown as Json;
}

test.use({
  mockConfig: {
    overrides: { ...BOOTSTRAP_OVERRIDES, '/api/v1/topology/shadow': () => comparing() },
  },
});

test('the Only derived pill opens its warning, and switching asks first', async ({ page, mock }) => {
  await page.goto('/topology/dependency');

  const pill = page.locator('button.infopress.dep-verdict-only_derived').first();
  await expect(pill).toBeVisible();
  // Still drawn as a pill: InfoTip.css's reset of a pressable label must not have won.
  const border = await pill.evaluate((el) => getComputedStyle(el).borderTopStyle);
  expect(border).toBe('solid');
  await pill.click();
  await expect(page.locator('.infotip-pop')).toContainText('real outage');
  await page.keyboard.press('Escape');

  await page.getByRole('button', { name: 'Use the derived graph' }).click();
  const dialog = page.getByRole('dialog', { name: 'Use the derived graph' });
  await expect(dialog).toContainText('real outage');
  await expect(dialog).toContainText('would newly be suppressed');
  // Nothing is sent until the operator confirms.
  expect(mock.requests.some((r) => r.method !== 'GET' && r.pathname === '/api/v1/settings/topology')).toBe(
    false,
  );
  await dialog.getByRole('button', { name: 'Cancel' }).click();
  await expect(dialog).toHaveCount(0);
});
