// SPDX-License-Identifier: AGPL-3.0-only
// Node detail tabs, generated from the declaration that owns them (ADR-052 決定 7, 出典 2).
//
// Nothing here is written down twice. `tabs.ts` says which nodes see which tab; this asks the
// running app the same question and demands the same answer. Add a tab, or change one `kinds`
// list, and these tests change with it — there is no expectation to update, which is the whole
// point of taking expectations from declarations instead of prose.
//
// 🚨 THE MATRIX HAS TWO AXES, AND THE SECOND ONE DOES NOT DEFEND ITSELF. Since ADR-119 visibility
// also depends on `snmp_configured`, and `tests/support/openapi.ts` answers every boolean with
// `false` — so iterating kinds alone would mock four *ping-only* nodes, never render Interfaces or
// Neighbors anywhere in this file, and stay entirely green while covering less than it did before.
// The cross below is what stops that; `SNMP_STATES` is why each describe block names its half.
//
// WHY THIS SCREEN. ADR-031's bug was "the Flow button renders, and clicking it bounces back to
// Overview" — the tab list and the tab *body* were two lists, and only one of them knew about
// Flow. Rendering is therefore not the assertion: **clicking is**. A test that only counted the
// buttons would have passed throughout that bug's life.

import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import { expect, test } from '../support/app';
import { BOOTSTRAP_OVERRIDES, TREE_SIBLING_IDS } from '../support/bootstrap';
import { defaultBodyFor, type Json } from '../support/openapi';
import {
  NODE_DETAIL_TABS,
  visibleNodeDetailTabs,
  type NodeDetailSubject,
} from '../../src/components/NodeDetail/tabs';
import { NODE_KINDS, type NodeKind } from '../../src/types/api';

const NODE_ID = '00000000-0000-4000-8000-0000000000aa';

/** Tab labels read from the EN locale — the same file the app renders from, so a renamed label
 *  cannot make this test disagree with the screen. Read rather than imported: JSON import
 *  attributes differ between the two module systems this repo's tooling uses. */
const TAB_LABELS = JSON.parse(
  readFileSync(join(process.cwd(), 'src/locales/en/nodes.json'), 'utf8'),
).tabs as Record<string, string>;

function nodeOf(subject: NodeDetailSubject): Json {
  const body = defaultBodyFor(`/api/v1/nodes/${NODE_ID}`) as {
    kind: NodeKind;
    snmp_configured: boolean;
  };
  body.kind = subject.kind;
  body.snmp_configured = subject.snmpConfigured;
  return body as unknown as Json;
}

/** The second axis, named so each describe block says which half it is walking. `false` is the
 *  ping-only node — a real device with no SNMP resolved, which ADR-119 offers four tabs. */
const SNMP_STATES = [
  { snmpConfigured: true, name: 'snmp' },
  { snmpConfigured: false, name: 'ping-only' },
] as const;

for (const kind of NODE_KINDS) {
  for (const { snmpConfigured, name } of SNMP_STATES) {
    const subject: NodeDetailSubject = { kind, snmpConfigured };
    test.describe(`a ${kind} node (${name})`, () => {
      test.use({
        mockConfig: {
          overrides: { ...BOOTSTRAP_OVERRIDES, '/api/v1/nodes/{node_id}': () => nodeOf(subject) },
        },
      });

      test('shows exactly the tabs it declares, in declared order', async ({ page }) => {
        await page.goto(`/nodes/${NODE_ID}`);
        const tabs = page.getByRole('tab');
        await expect(tabs.first()).toBeVisible();

        const expected = visibleNodeDetailTabs(subject).map((t) => TAB_LABELS[t]);
        // Order matters: `visibleNodeDetailTabs` filters NODE_DETAIL_TABS, so the declaration
        // fixes the sequence too. A screen that showed the right set in a different order would be
        // a different screen from the one the declaration describes.
        await expect(tabs).toHaveText(expected.map((label) => new RegExp(`^${label}`)));
      });

      test('opens every tab it offers, and none it does not', async ({ page }) => {
        for (const tab of NODE_DETAIL_TABS) {
          const allowed = visibleNodeDetailTabs(subject).includes(tab);
          await page.goto(`/nodes/${NODE_ID}?tab=${tab}`);
          await expect(page.getByRole('tab').first()).toBeVisible();

          // Deep-linking is the harsher half of the same contract: `resolveNodeDetailTab` must
          // send a URL for a tab this node cannot show back to Overview, and must leave the others
          // alone.
          const selected = page.getByRole('tab', { selected: true });
          await expect(
            selected,
            allowed
              ? `?tab=${tab} should open ${tab} on a ${kind}/${name} node`
              : `?tab=${tab} is not offered to a ${kind}/${name} node and must fall back to Overview`,
          ).toHaveText(new RegExp(`^${TAB_LABELS[allowed ? tab : 'overview']}`));
        }
      });
    });
  }
}

const SNMP_DEVICE: NodeDetailSubject = { kind: 'device', snmpConfigured: true };

test.describe('clicking a tab', () => {
  test.use({
    mockConfig: {
      overrides: { ...BOOTSTRAP_OVERRIDES, '/api/v1/nodes/{node_id}': () => nodeOf(SNMP_DEVICE) },
    },
  });

  test('selects it and keeps it selected — the ADR-031 regression', async ({ page, errors }) => {
    await page.goto(`/nodes/${NODE_ID}`);
    // The SNMP device on purpose: it is the only node with all six tabs, so this is the widest
    // version of the regression.
    for (const tab of visibleNodeDetailTabs(SNMP_DEVICE)) {
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

// Moving between nodes keeps the tab (ADR-134).
//
// WHY TIER1. The judgement is unit-tested (`requestedNodeDetailTab`), and it is three lines. What no
// unit test can say is whether the memory survives the trip: the inventory split **deletes** `?tab=`
// on every new selection and remounts `<NodeDetail>` through its `key`, so "the tab stays" depends
// on the store, the URL write and the remount agreeing. That is wiring, which is what a browser is
// for — and the original complaint was reported in exactly this gesture.
test.describe('walking the inventory with a tab open', () => {
  /** Answer each of the tree's three nodes differently, so the middle one is a URL monitor with no
   *  Interfaces tab. The whole point of 決定 2 is what happens when such a node is stepped through. */
  const perNode: Record<string, 'device' | 'url'> = {
    [TREE_SIBLING_IDS[0]]: 'device',
    [TREE_SIBLING_IDS[1]]: 'url',
    [TREE_SIBLING_IDS[2]]: 'device',
  };

  test.use({
    mockConfig: {
      overrides: {
        ...BOOTSTRAP_OVERRIDES,
        '/api/v1/nodes/{node_id}': (url) => {
          const id = url.pathname.split('/').pop() ?? '';
          const kind = perNode[id] ?? 'device';
          const body = defaultBodyFor(`/api/v1/nodes/${id}`) as {
            id: string;
            kind: string;
            snmp_configured: boolean;
          };
          body.id = id;
          body.kind = kind;
          // A URL monitor is never SNMP-polled; a device here is, so it has all six tabs.
          body.snmp_configured = kind === 'device';
          return body as unknown as Json;
        },
      },
    },
  });

  /** The tab currently selected, by label. */
  const openTab = (page: import('@playwright/test').Page) =>
    page.getByRole('tab', { selected: true });

  test('picking another node opens it on the tab already in view', async ({ page, errors }) => {
    // 🚨 THE REPORTED DEFECT, in the gesture it was reported in: open Interfaces on one switch,
    // click the next switch, and it used to come back on Overview — because `select()` deletes
    // `?tab=` and nothing else answered for the tab.
    await page.goto('/nodes');
    const rows = page.locator('.ntree-node');
    await expect(rows).toHaveCount(3);

    await rows.nth(0).click();
    await page
      .getByRole('tab', { name: new RegExp(`^${TAB_LABELS.interfaces}`) })
      .click();
    await expect(openTab(page)).toHaveText(new RegExp(`^${TAB_LABELS.interfaces}`));

    // The third row, not the second: the second is the URL monitor, which is the next test.
    await rows.nth(2).click();
    await expect(
      openTab(page),
      'selecting another node dropped the tab and fell back to Overview',
    ).toHaveText(new RegExp(`^${TAB_LABELS.interfaces}`));
    expect(errors.uncaught).toEqual([]);
  });

  test('a node without that tab falls back, and does not take the memory with it', async ({
    page,
    errors,
  }) => {
    // 🚨 ADR-134 決定 2. The correction effect rewrites a tab the loaded node cannot show — and if
    // it recorded that rewrite, this sequence would leave every later switch on Overview, so the
    // memory would mean "the last screen I was dropped onto" instead of "the last one I chose".
    await page.goto('/nodes');
    const rows = page.locator('.ntree-node');
    await expect(rows).toHaveCount(3);

    await rows.nth(0).click();
    await page
      .getByRole('tab', { name: new RegExp(`^${TAB_LABELS.interfaces}`) })
      .click();
    await expect(openTab(page)).toHaveText(new RegExp(`^${TAB_LABELS.interfaces}`));

    // The URL monitor: Interfaces is not among its tabs, so Overview is correct here.
    await rows.nth(1).click();
    await expect(
      openTab(page),
      'a URL monitor was left on a tab it does not offer',
    ).toHaveText(new RegExp(`^${TAB_LABELS.overview}`));

    // …and back to a device, which must return to Interfaces.
    await rows.nth(2).click();
    await expect(
      openTab(page),
      'stepping through a node without the tab erased the remembered one',
    ).toHaveText(new RegExp(`^${TAB_LABELS.interfaces}`));
    expect(errors.uncaught).toEqual([]);
  });
});
