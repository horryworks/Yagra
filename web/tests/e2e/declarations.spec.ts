// SPDX-License-Identifier: AGPL-3.0-only
// Tier2a, ADR-052 決定 7 出典 2: expectations taken from declarations, asked of the running system.
//
// Tier1 already asks the tab question — but it asks it of a node whose kind Tier1 chose. Here the
// kind is whatever `NodeKind::resolve` decided on the server for a row an operator really created,
// which makes this a question about the *seam*: the backend resolves a node's kind from which side
// table carries a row, the frontend keys the tab rules off the kind string it receives, and nothing
// in either language checks that the two vocabularies still line up. A kind the server can emit and
// the client has no rules for is a node-detail page with the wrong tabs and no error anywhere.
//
// The roles half is the same move against a different declaration. `rbac.rs::description` is
// rendered verbatim to the operator — it is the only in-product explanation of what a role may do,
// so a matrix that drifts from the enforcement it describes is a security-relevant lie, and the
// only place both halves exist at once is a running deployment.

import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import { NODE_DETAIL_TABS, visibleNodeDetailTabs } from '../../src/components/NodeDetail/tabs';
import { NODE_KINDS, type NodeKind } from '../../src/types/api';
import { expect, test } from './support/live';

const TAB_LABELS = JSON.parse(
  readFileSync(join(process.cwd(), 'src/locales/en/nodes.json'), 'utf8'),
).tabs as Record<string, string>;

interface NodeRow {
  id: string;
  name: string;
  kind: NodeKind;
}

interface RoleMatrix {
  permissions: { key: string; label: string; description: string }[];
  roles: { key: string; label: string; description: string; permissions: string[] }[];
}

test('every node kind the deployment actually holds is one the tab rules know', async ({
  page,
  api,
}) => {
  const { nodes } = await api<{ nodes: NodeRow[] }>('/api/v1/nodes?limit=500');
  expect(nodes.length, 'the deployment has no nodes').toBeGreaterThan(0);

  // The seam, stated plainly: the server emitted these kind strings, and `NODE_KINDS` is the
  // client's complete list. A new backend kind reaching an unchanged frontend lands here, not in a
  // support ticket about a node page with no Interfaces tab.
  const unknown = [...new Set(nodes.map((n) => n.kind))].filter(
    (k) => !(NODE_KINDS as readonly string[]).includes(k),
  );
  expect(unknown, 'the deployment serves node kinds this frontend has no rules for').toEqual([]);

  // One representative per kind present — the point is coverage of what exists, not of the fleet.
  const perKind = [...new Map(nodes.map((n) => [n.kind, n])).values()];

  for (const node of perKind) {
    await page.goto(`/nodes/${node.id}`);
    const tabs = page.getByRole('tab');
    await expect(tabs.first(), `${node.name} showed no tabs at all`).toBeVisible();

    const expected = visibleNodeDetailTabs(node.kind).map((t) => TAB_LABELS[t]);
    await expect(tabs, `${node.name} is a ${node.kind}`).toHaveText(
      expected.map((label) => new RegExp(`^${label}`)),
    );

    // 🚨 Rendering the button was never the bug. ADR-031 shipped a Flow tab whose button appeared
    // and whose click bounced back to Overview, because the tab bar and the body switch were two
    // lists. So each offered tab is opened, on a real node, against real data.
    for (const tab of NODE_DETAIL_TABS) {
      if (!visibleNodeDetailTabs(node.kind).includes(tab)) continue;
      await page.getByRole('tab', { name: new RegExp(`^${TAB_LABELS[tab]}`) }).click();
      await expect(
        page.getByRole('tab', { name: new RegExp(`^${TAB_LABELS[tab]}`) }),
        `${node.name}: the ${tab} tab did not stay open when clicked`,
      ).toHaveAttribute('aria-selected', 'true');
    }
  }
});

test('the permission matrix shows what the backend says it enforces', async ({ page, api }) => {
  const matrix = await api<RoleMatrix>('/api/v1/roles');
  expect(matrix.roles.length, 'the deployment reported no roles').toBeGreaterThan(0);

  await page.goto('/settings/roles');
  await expect(page.getByRole('heading', { name: 'Roles' }).first()).toBeVisible();
  const body = page.locator('body');

  // Verbatim, both halves. The label alone would pass on a page that showed the right role names
  // beside the wrong explanations — and the explanation is the part an operator reads before
  // deciding who to give the role to.
  for (const role of matrix.roles) {
    await expect(body, `the ${role.key} role is missing`).toContainText(role.label);
    await expect(body, `the ${role.key} role's description is not the backend's`).toContainText(
      role.description,
    );
  }
  for (const perm of matrix.permissions) {
    await expect(body, `the ${perm.key} privilege is missing`).toContainText(perm.label);
    await expect(body, `the ${perm.key} privilege's description is not the backend's`).toContainText(
      perm.description,
    );
  }
});
