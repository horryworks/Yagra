// SPDX-License-Identifier: AGPL-3.0-only
// Tier2a, ADR-052 決定 9 型 2 and 型 3: the same fact, read two ways, must come out the same.
//
// WHY THIS SHAPE. A live deployment's data moves, so "there are three nodes" is not an assertion
// anyone can write. What survives is *agreement* — the aggregate against the list, the list against
// the detail — and agreement needs no fixture, no expected value and no maintenance when the data
// changes. It is also the only shape that reaches the damage `extensibility.md` §3 records: after a
// core restart the fleet summary and the PDF report said `unknown` about the very nodes the Nodes
// page beside them showed as `ok`, because a node's display state had been written out by hand in
// five places and two had dropped the freshness fallback.
//
// 🚨 That bug is unreachable from Tier1, and not for a boring reason. To mock it you would have to
// serve "the engine has no opinion yet but ICMP is fresh" — and the moment a fixture states that,
// it has decided the answer the code is supposed to derive. A live system is the only place the
// premise arrives unchosen.

import { expect, test } from './support/live';

type NodeState = 'ok' | 'warning' | 'critical' | 'unreachable' | 'unknown' | 'maintenance';

interface FleetSummary {
  total: number;
  states: Record<NodeState, number>;
}
interface NodeRow {
  id: string;
  name: string;
  state: NodeState;
  kind: string;
}
interface NodeList {
  nodes: NodeRow[];
  truncated: boolean;
}

/** `src/locales/en/format.json`'s `state` block — the labels `stateLabel()` resolves. Read here
 *  rather than imported from `lib/format`, which resolves through the global i18n instance and
 *  would drag the whole runtime into a Node process. */
const STATE_LABEL: Record<NodeState, string> = {
  ok: 'Ok',
  warning: 'Warning',
  critical: 'Critical',
  unreachable: 'Unreachable',
  unknown: 'Unknown',
  maintenance: 'Maintenance',
};

test('the inventory header agrees with the list it is a summary of', async ({ page, api }) => {
  const [summary, list, groups] = await Promise.all([
    api<FleetSummary>('/api/v1/fleet/summary'),
    api<NodeList>('/api/v1/nodes?limit=500'),
    api<{ id: string }[]>('/api/v1/node-groups'),
  ]);

  // 出典 1 — an invariant, true whatever the fleet contains: a total is the sum of its parts.
  const bucketed = Object.values(summary.states).reduce((a, b) => a + b, 0);
  expect(bucketed, 'the fleet summary does not add up to its own total').toBe(summary.total);

  // 型 2 — the aggregate against the list. Two different queries answer "how many nodes are
  // there", and the header shows the first while the tree below it is built from the second.
  test.skip(list.truncated, 'the node list was capped, so its length is not the fleet total');
  expect(list.nodes.length, 'the aggregate and the list disagree about the fleet').toBe(
    summary.total,
  );

  await page.goto('/nodes');
  const note = page.locator('.pageheader-note');
  await expect(note).toBeVisible();

  // 型 3 — and the screen says what the server said. Pluralization is part of it: the count and
  // the noun come from different places (`nodeCount` and an i18n plural rule) and a 1-node fleet
  // reading "1 nodes" is the kind of thing only a real deployment with one node ever shows.
  const noun = summary.total === 1 ? 'node' : 'nodes';
  await expect(note).toContainText(`${summary.total} ${noun}`);
  await expect(note).toContainText(`${groups.length} groups`);

  const attention = summary.states.warning + summary.states.critical + summary.states.unreachable;
  if (attention > 0) await expect(note).toContainText(`${attention} need attention`);
});

test('a node’s state on the list is the state on its own page', async ({ page, api }) => {
  const before = await api<NodeList>('/api/v1/nodes?limit=500');
  expect(before.nodes.length, 'the deployment has no nodes to compare').toBeGreaterThan(0);

  // A handful is enough — this is checking that two code paths agree, not auditing the fleet.
  // Take them from opposite ends so a same-group, same-kind trio cannot be the whole sample.
  const sample = [before.nodes[0], before.nodes.at(-1)!, before.nodes[Math.floor(before.nodes.length / 2)]];
  const picked = [...new Map(sample.map((n) => [n.id, n])).values()];

  for (const node of picked) {
    await page.goto(`/nodes/${node.id}`);
    const pill = page.locator('.nd-statepill');
    await expect(pill, `${node.name} never showed a state`).toBeVisible();
    const shown = (await pill.innerText()).trim();

    // ⚠️ The list was read before the page and the state genuinely can change between the two —
    // a poll lands, dwell expires. So the assertion is "the page shows a state the list reported
    // for this node, on one side or the other of the navigation", which is still false for the
    // bug this exists to catch (two surfaces deriving the state differently) and true for an
    // honest transition. Re-reading is not a way to make a flaky test pass: it is the difference
    // between disagreement and change, and only one of those is a defect.
    const after = await api<NodeList>('/api/v1/nodes?limit=500');
    const nowState = after.nodes.find((n) => n.id === node.id)?.state;
    const acceptable = [STATE_LABEL[node.state], nowState ? STATE_LABEL[nowState] : null].filter(
      Boolean,
    );
    expect(
      acceptable,
      `the list called ${node.name} "${node.state}" but its own page says "${shown}"`,
    ).toContain(shown);
  }
});

test('searching the inventory narrows it to matching nodes', async ({ page, api }) => {
  // 型 4 — an interaction, on data nobody wrote down. The search is served by `/nodes/search`,
  // a different endpoint from the one that fills the tree, so this also asks whether the two
  // agree about what a node is called.
  const list = await api<NodeList>('/api/v1/nodes?limit=500');
  const target = list.nodes[0];
  test.skip(!target, 'the deployment has no nodes to search for');

  await page.goto('/nodes');
  await expect(page.locator('.pageheader-note')).toBeVisible();

  // By role, not by placeholder: the topbar's global search carries the same placeholder and is a
  // `combobox`, so a placeholder query matches two boxes and types into whichever came first.
  const search = page.getByRole('textbox', { name: 'Search…' });
  await search.fill(target.name);

  const rows = page.locator('.ntree-node');
  await expect(rows.filter({ hasText: target.name }).first()).toBeVisible();

  // Everything still listed matches. Asserting a *count* would be asserting the fleet's contents;
  // asserting that nothing unrelated survived is true at any size.
  const names = await rows.allInnerTexts();
  const stray = names.filter((n) => !n.toLowerCase().includes(target.name.toLowerCase()));
  expect(stray, 'the search left rows that do not match the term').toEqual([]);

  await search.fill('');
  await expect(rows.first()).toBeVisible();
});
