// SPDX-License-Identifier: AGPL-3.0-only
// A widget is in the catalog, and placing it renders something (ADR-069).
//
// This exists because the same gap has now been left open three times. ADR-046 Inc.2 and Inc.3 both
// shipped a dashboard widget and both still carry "not seen on real hardware: does it appear in the
// catalogue, and does placing it draw anything" as an outstanding item — because nothing between
// `registry.test.ts` and a person's eyes covers it. `registry.test.ts` proves the entry exists and
// that its two strings resolve in both locales; it cannot prove the component mounts, because
// Vitest runs `environment: 'node'` and never executes `.tsx`.
//
// So the assertions here are deliberately about the seam those two cannot reach:
//   1. the catalogue *renders* the entry (registry → CatalogModal → translated card), and
//   2. clicking it puts a widget on the board whose body and header actions actually mount.
//
// Since ADR-072 it also carries the other half of that seam: *which* controls a card offers depends
// on whether the board is being customized, and the type system can only police what a control is
// allowed to write, never where it is drawn. So the assertions about the ⚙ split live here.
// A widget whose component throws blanks the screen with no HTTP error at all — the `pageerror`
// channel in `support/app.ts` is the only place that surfaces, and it fails the test.

import { expect, test } from '../support/app';
import { BOOTSTRAP_OVERRIDES } from '../support/bootstrap';
import type { Json } from '../support/openapi';

const NODE_ID = '00000000-0000-4000-8000-0000000000aa';

/** An empty board, so the assertions are about what this test places rather than about whatever
 *  the generated mock happened to invent for the layout document. */
const EMPTY_BOARD = { version: 3, boards: [{ id: 'b1', name: 'Board', widgets: [] }] };

/** A board that already plots one link, for the assertions about the chart itself. */
const SEEDED_BOARD = {
  version: 3,
  boards: [
    {
      id: 'b1',
      name: 'Board',
      widgets: [
        {
          instanceId: 'w1',
          type: 'interface-traffic',
          span: 12,
          settings: {
            links: [{ nodeId: NODE_ID, nodeName: 'router-a', ifindex: 3, ifName: 'Gi0/3' }],
            unit: 'bps',
            rangeSecs: 3600,
          },
        },
      ],
    },
  ],
};

/** Two interfaces on the node, so "the roster relabels the link" is provable and the picker has
 *  something to offer. */
const ROSTER = [
  { ifindex: 3, if_name: 'Gi0/3', if_alias: 'uplink', stale: false },
  { ifindex: 4, if_name: 'Gi0/4', if_alias: null, stale: false },
] as unknown as Json;

/** A series covering the window the client actually asked for.
 *
 *  ⚠️ The generated mock puts its single sample at Unix second 1 — an hour of 1970, outside every
 *  requested range — so uPlot reports no data in range and the idle legend never resolves. Reading
 *  `from`/`to` off the request keeps this honest whatever the widget's default window becomes. */
function seriesBody(url: URL): Json {
  const from = Number(url.searchParams.get('from'));
  const to = Number(url.searchParams.get('to'));
  const n = 8;
  const timestamps = Array.from({ length: n }, (_, i) =>
    Math.round(from + ((to - from) * i) / (n - 1)),
  );
  const flat = (v: number) => timestamps.map(() => v);
  return {
    timestamps,
    // Deliberately unequal, and both well above 1 kbps, so the two legend readouts are distinct
    // strings and neither can be confused with the other.
    in_bps: flat(8_000_000),
    out_bps: flat(2_000_000),
    in_ucast_pps: flat(1_000),
    out_ucast_pps: flat(250),
    in_errors: flat(0),
    out_errors: flat(0),
    in_discards: flat(0),
    out_discards: flat(0),
    rx_power_dbm: flat(-5),
    tx_power_dbm: flat(-3),
  } as unknown as Json;
}

test.use({
  viewport: { width: 1440, height: 900 },
  mockConfig: {
    overrides: {
      ...BOOTSTRAP_OVERRIDES,
      '/api/v1/dashboard': () => EMPTY_BOARD,
      '/api/v1/nodes/{node_id}/interfaces': () => ROSTER,
      '/api/v1/nodes/{node_id}/interfaces/{ifindex}/series': (url: URL) => seriesBody(url),
    },
  },
});

/** The catalogue card for one widget, matched on its TITLE.
 *
 *  ⚠️ Not `hasText`. A substring match over the whole card also reads the blurb, and
 *  `aggregate-throughput`'s blurb contains the words "interface traffic" — so the loose selector
 *  silently resolved to the wrong card and the section and copy assertions below passed about a
 *  widget this file is not testing. Caught by placing it: the board grew an Aggregate throughput.
 */
function card(page: import('@playwright/test').Page, title: string) {
  return page.locator(`.catalog-item:has(.catalog-item-title:text-is("${title}"))`);
}

/** Open My Dashboard's catalogue. */
async function openCatalog(page: import('@playwright/test').Page) {
  await page.goto('/dashboard/my');
  await page.getByRole('button', { name: 'Customize' }).click();
  await page.getByRole('button', { name: 'Add widget' }).click();
  await expect(page.locator('.catalog')).toBeVisible({ timeout: 15_000 });
}

// The three widgets that shipped before this harness existed and each left "does it appear in the
// catalogue, and does placing it draw anything" as an outstanding manual check (ADR-046 Inc.2,
// Inc.3 and Inc.4). They are covered here rather than in three files because the seam is identical
// — and because a check nobody performed three times running is a check that wants mechanising.
//
// What is asserted is only the half a mock can answer: the card renders, and the placed widget
// mounts into a state it can reach without real hardware. The rest of each item's homework (a USG's
// `huawei_*` metrics appearing, a counter being refused a ranking) still needs a device.
const PRIOR_WIDGETS = [
  { title: 'Metric chart', section: /performance/i, placed: 'press Customize, then the ⚙ on this card' },
  { title: 'Top nodes by metric', section: /performance/i, placed: 'press Customize, then the ⚙ on this card' },
  { title: 'Most interface discards', section: /performance/i, placed: null },
];

for (const w of PRIOR_WIDGETS) {
  test(`${w.title} is in the catalogue and mounts when placed`, async ({ page, errors }) => {
    await openCatalog(page);
    const item = card(page, w.title);
    await expect(item).toHaveCount(1);
    const section = item.locator('xpath=ancestor::div[contains(@class,"catalog-section")]');
    await expect(section.locator('.catalog-section-title')).toHaveText(w.section);

    await item.click();
    await page.locator('.modal').getByRole('button', { name: 'Done' }).click();
    const cell = page.locator('.mydash-cell').first();
    await expect(cell).toBeVisible({ timeout: 15_000 });
    // `placed` is the widget's own no-input prompt where it has one; the ranking widget just has to
    // render rows from the mocked Top-N, which `toBeVisible` above already establishes.
    if (w.placed) await expect(cell).toContainText(w.placed);
    expect(errors.uncaught, 'the widget threw while rendering').toEqual([]);
  });
}

test('the interface-traffic widget is offered in the catalogue, with real copy', async ({ page }) => {
  await openCatalog(page);

  const item = card(page, 'Interface traffic');
  await expect(item).toHaveCount(1);
  await expect(item).toBeVisible();

  // It is filed under Capacity, beside the fleet-wide throughput chart it is the per-link
  // counterpart of — a widget in the wrong section is found by nobody and reported by nothing.
  const section = item.locator('xpath=ancestor::div[contains(@class,"catalog-section")]');
  await expect(section.locator('.catalog-section-title')).toHaveText(/capacity/i);

  // The card's own copy, not a raw i18n key. `registry.test.ts` proves the two keys resolve in both
  // locales; only this proves the resolved strings are what the card puts on screen.
  const blurb = await item.locator('.catalog-item-blurb').innerText();
  expect(blurb.length).toBeGreaterThan(10);
  expect(blurb, 'raw i18n key rendered instead of copy').not.toMatch(/^registry\./);
});

test('placing it mounts a widget that says what it needs', async ({ page, errors }) => {
  await openCatalog(page);
  await card(page, 'Interface traffic').click();
  // Close the catalogue — its footer Done, not the edit bar's.
  await page.locator('.modal').getByRole('button', { name: 'Done' }).click();

  const cell = page.locator('.mydash-cell').first();
  await expect(cell).toBeVisible({ timeout: 15_000 });

  // The body mounted and is in its no-selection state. Asserting the prompt rather than merely
  // "a card exists" is what separates a mounted widget from an empty box — and since ADR-072 the
  // prompt has to name where the control it asks for actually lives, because it is not on this
  // screen (ADR-055 R6).
  await expect(cell).toContainText('Pick one or more interfaces to plot');
  await expect(cell).toContainText('press Customize, then the ⚙ on this card');

  // Header actions only render in view mode, so leave customize first. Exactly the two view
  // controls must be there — the unit and the window.
  await page.getByRole('button', { name: 'Done' }).click();
  await expect(cell.locator('.iftraffic-actions select')).toHaveCount(2);

  expect(errors.uncaught, 'the widget threw while rendering').toEqual([]);
});

// 🚨 The rule ADR-072 exists for, stated as what an operator can and cannot reach.
//
// The type system stops a subject control from *writing* through the view-mode slot; it says
// nothing about whether one is drawn. Only a browser can answer that, and only by looking at both
// modes of the same card — an absence on its own is equally consistent with the widget being
// broken, which is why every assertion below has its presence counterpart further down.
test('a card outside Customize offers no way to choose its subject', async ({ page }) => {
  await openCatalog(page);
  await card(page, 'Interface traffic').click();
  await page.locator('.modal').getByRole('button', { name: 'Done' }).click();
  await page.getByRole('button', { name: 'Done' }).click();

  const cell = page.locator('.mydash-cell').first();
  await expect(cell.locator('.iftraffic-actions select')).toHaveCount(2); // unit + window survive
  await expect(cell.locator('[aria-haspopup="dialog"]')).toHaveCount(0); // …the picker does not
  await expect(cell.locator('.widgetframe-gear')).toHaveCount(0); // and the ⚙ is Customize-only
});

test('the metric chart, whose every control is a subject, has an empty header outside Customize', async ({
  page,
}) => {
  await openCatalog(page);
  await card(page, 'Metric chart').click();
  await page.locator('.modal').getByRole('button', { name: 'Done' }).click();
  await page.getByRole('button', { name: 'Done' }).click();

  const cell = page.locator('.mydash-cell').first();
  // Node picker and metric select both gone: this widget has no window and no lens, so it is the
  // one that ends up with nothing at all in its view-mode header.
  await expect(cell.locator('.nodepick-control')).toHaveCount(0);
  await expect(cell.locator('.card-actions select')).toHaveCount(0);
});

test('Customize puts a ⚙ on the cards that have a subject, and only those', async ({ page }) => {
  await openCatalog(page);
  await card(page, 'Interface traffic').click();
  await card(page, 'Top CPU').click();
  await page.locator('.modal').getByRole('button', { name: 'Done' }).click();

  // Still in Customize. The traffic card chooses interfaces; a plain Top-N chooses nothing, and a
  // ⚙ on all 48 would read as "every card is configurable" while configuring nothing.
  const cells = page.locator('.mydash-cell');
  await expect(cells.nth(0).locator('.widgetframe-gear')).toHaveCount(1);
  await expect(cells.nth(1).locator('.widgetframe-gear')).toHaveCount(0);
  // Remove stays on both — the ⚙ is an addition, not a replacement.
  await expect(cells.nth(1).locator('.widgetframe-remove')).toHaveCount(1);
});

/** Place the traffic widget and open its ⚙ panel, staying in Customize. Returns the panel. */
async function openTrafficSettings(page: import('@playwright/test').Page) {
  await openCatalog(page);
  await card(page, 'Interface traffic').click();
  await page.locator('.modal').getByRole('button', { name: 'Done' }).click();
  await page.locator('.widgetframe-gear').first().click();
  // The popover is portalled to the body — asserting it from inside the cell would pass for the
  // wrong reason if someone "simplified" it back to an in-place absolute panel.
  const pop = page.locator('.apop.widgetframe-settings-pop');
  await expect(pop).toBeVisible({ timeout: 15_000 });
  return pop;
}

test('the link picker opens from the ⚙ and offers a node', async ({ page }) => {
  const pop = await openTrafficSettings(page);
  await expect(pop).toContainText('Nothing plotted yet.');
  // The interface select is present and refuses to be used before a node is chosen.
  await expect(pop.locator('select')).toBeDisabled();
});

// The receiving side of the three absence assertions above. Without this, "the picker is gone from
// both modes" satisfies every one of them — a split and a demolition look identical from the
// outside, and this repo has shipped a green suite over exactly that shape before.
test('an interface picked through the ⚙ is actually plotted', async ({ page, errors }) => {
  const pop = await openTrafficSettings(page);

  await pop.locator('.nodepick-trigger').click();

  // ⚠️ A popover inside a popover, and the inner one does NOT portal — `.nodepick-pop` is
  // `position: absolute` inside the panel the frame drew. That is what keeps the outer popover
  // open (its outside-click test asks whether the click landed inside its own subtree, and this
  // one does), but it also means the list is laid out against a 340px-max surface instead of the
  // page. Check it landed somewhere a person can use rather than off the right edge.
  const list = await pop.locator('.nodepick-pop').boundingBox();
  expect(list, 'the node list did not open').not.toBeNull();
  const vw = page.viewportSize()!.width;
  expect(list!.x, 'the node list opened off the left edge').toBeGreaterThanOrEqual(0);
  expect(list!.x + list!.width, 'the node list opened off the right edge').toBeLessThanOrEqual(vw);
  expect(list!.width, 'the node list is too narrow to read a node name').toBeGreaterThan(140);

  await pop.locator('.nodepick-option').first().click();
  // The roster arrives before the select can offer anything.
  await expect(pop.locator('select')).toBeEnabled({ timeout: 15_000 });
  await pop.locator('select').selectOption({ index: 1 });
  await pop.getByRole('button', { name: 'Add' }).click();

  // Chosen, and said so in the panel's own list.
  await expect(pop.locator('.iftraffic-item')).toHaveCount(1);

  // …and on the chart, once Customize is left: one link ⇒ receive + transmit + uPlot's x row.
  await page.getByRole('button', { name: 'Done' }).click();
  const cell = page.locator('.mydash-cell').first();
  await expect(cell.locator('.metricchart-fill')).toBeVisible({ timeout: 15_000 });
  await expect(cell.locator('.u-legend .u-series')).toHaveCount(3);
  expect(errors.uncaught).toEqual([]);
});

test.describe('with a link already plotted', () => {
  test.use({
    mockConfig: {
      overrides: {
        ...BOOTSTRAP_OVERRIDES,
        '/api/v1/dashboard': () => SEEDED_BOARD,
        '/api/v1/nodes/{node_id}/interfaces': () => ROSTER,
        '/api/v1/nodes/{node_id}/interfaces/{ifindex}/series': (url: URL) => seriesBody(url),
      },
    },
  });

  test('draws both directions and reports each as a magnitude', async ({ page, errors }) => {
    await page.goto('/dashboard/my');
    const cell = page.locator('.mydash-cell').first();
    await expect(cell.locator('.metricchart-fill')).toBeVisible({ timeout: 15_000 });

    // One link ⇒ two series, plus uPlot's x-axis row: three legend rows. Two would mean one
    // direction never made it onto the chart.
    const legend = cell.locator('.u-legend .u-series');
    await expect(legend).toHaveCount(3);

    // 🚨 The assertion this file exists for. Transmit is plotted BELOW zero, so its stored value is
    // negative — and if `legendFormat` did not take the magnitude, the readout would say
    // `-2.0 Mbps`, a rate that cannot exist. Read the value cell alone: the label beside it
    // contains `router-a`, whose hyphen would make a "no minus sign" check on the whole row pass
    // or fail for reasons that have nothing to do with the sign.
    const inValue = await legend.nth(1).locator('.u-value').innerText();
    const outValue = await legend.nth(2).locator('.u-value').innerText();
    expect(inValue).toBe('8.0 Mbps');
    expect(outValue).toBe('2.0 Mbps');
    expect(outValue, 'transmit reported as a negative rate').not.toContain('-');

    // The labels came from the roster, and each names its direction.
    await expect(legend.nth(1)).toContainText('router-a · Gi0/3 In');
    await expect(legend.nth(2)).toContainText('router-a · Gi0/3 Out');

    expect(errors.uncaught).toEqual([]);
  });

  test('switching to pps redraws from the packet counters without a refetch', async ({
    page,
    mock,
  }) => {
    await page.goto('/dashboard/my');
    const cell = page.locator('.mydash-cell').first();
    await expect(cell.locator('.metricchart-fill')).toBeVisible({ timeout: 15_000 });

    const seriesCalls = () =>
      mock.requests.filter((r) => /\/interfaces\/\d+\/series$/.test(r.pathname)).length;
    const before = seriesCalls();

    await cell.locator('.iftraffic-actions select').first().selectOption('pps');

    const legend = cell.locator('.u-legend .u-series');
    // Different numbers from the bps pair, so this cannot pass by drawing the same arrays under a
    // new axis label — and still magnitudes, so the mirroring survives the unit change.
    await expect(legend.nth(1).locator('.u-value')).toHaveText('1 kpps');
    await expect(legend.nth(2).locator('.u-value')).toHaveText('250 pps');

    // ADR-060 decision 5: the response carries both units, so the toggle re-reads what is already
    // in hand. A refetch here would be a regression in the dependency list, not a visible bug.
    expect(seriesCalls(), 'flipping the unit re-queried the store').toBe(before);
  });
});

// 🚨 Layout, and specifically the kind of layout bug nothing else in this repo can see: tsc does not
// read CSS and Vitest runs with no layout engine.
//
// The add row's controls overlapped on the real deployment — `NodePicker`'s `.nodepick-control`
// carries `min-width: 180px`, which is wider than the flex track it was given, so it spilled over
// the interface select and covered the interface name. `.metricchart-actions` overrides that same
// rule for the same reason; this widget did not.
//
// ⚠️ The first version of this test asserted only that each control sat inside the PANEL, and it
// passed while the bug was on screen: the overflowing element was `.nodepick-control`, which is
// inside the panel and merely outside its own parent. A containment check that does not name the
// element that overflows is a check that cannot fail. Assert what the operator actually lost —
// the select being covered — and assert the parent/child relation directly.
test('the picker controls do not overlap each other', async ({ page }) => {
  const pop = await openTrafficSettings(page);
  const box = async (sel: string) => {
    const b = await pop.locator(sel).first().boundingBox();
    expect(b, `${sel} has no box`).not.toBeNull();
    return b!;
  };

  const panel = (await pop.boundingBox())!;
  const node = await box('.iftraffic-node');
  const control = await box('.nodepick-control');
  const select = await box('select');
  const add = await box('.btn');

  // The node picker's inner control must fit the slot it was given. This is the assertion that
  // fails when the shared `min-width: 180px` is not overridden.
  expect(
    control.x + control.width,
    'the node picker control overflows its own wrapper',
  ).toBeLessThanOrEqual(node.x + node.width + 1);

  // Nothing covers the interface select.
  //
  // ⚠️ Overlap is a two-axis question, and asserting it on x alone is wrong in a way that looks
  // right: the controls are stacked, so the select legitimately starts to the LEFT of the node
  // picker's right edge while sitting on the row below it. Test the actual rectangle intersection
  // so the assertion survives the layout being changed again.
  const overlaps = (a: typeof control, b: typeof select) =>
    a.x < b.x + b.width && b.x < a.x + a.width && a.y < b.y + b.height && b.y < a.y + a.height;
  expect(overlaps(control, select), 'the node picker covers the interface select').toBe(false);

  // And it is wide enough to show a name rather than just its caret — `GigabitEthernet0/0/1` is an
  // ordinary interface name on the devices this monitors.
  expect(select.width, 'the interface select is too narrow to read a name').toBeGreaterThan(140);

  // And everything is still inside the panel it was drawn in.
  for (const [name, b] of [['add row', await box('.iftraffic-add')], ['select', select], ['add button', add]] as const) {
    expect(b.x, `${name} starts left of the panel`).toBeGreaterThanOrEqual(panel.x - 1);
    expect(b.x + b.width, `${name} overflows the panel`).toBeLessThanOrEqual(panel.x + panel.width + 1);
  }
});

// 🚨 A card near the bottom of the board. Reported from the running deployment: the ⚙ panel opens,
// and the node list inside it runs off the bottom of the window showing two rows.
//
// Neither of the two surfaces can be scrolled to. The ⚙ panel is `position: fixed` (the shared
// popover, so a virtualized row's transform cannot re-anchor it) and the node list inside it is
// `position: absolute` against that panel — so the page scrollbar moves neither, and whatever falls
// past the fold is simply unreachable. `AnchoredPopover` flips its own panel when it does not fit;
// the list it contains had no such rule and always dropped downwards.
//
// The short viewport is the reproduction, not a shortcut: it is the same geometry as a tall board
// scrolled to its end, and it is deterministic.
test.describe('with the card near the bottom of the window', () => {
  // A fleet, not a node. The generated mock answers the picker's search with one row, and a
  // one-row list fits anywhere — the bug needs the list at its 240px cap, which is what an
  // operator with a real inventory always has.
  const FLEET = Array.from({ length: 20 }, (_, i) => ({
    id: `00000000-0000-4000-8000-0000000001${String(i).padStart(2, '0')}`,
    name: `node-${i}`,
    address: `10.0.0.${i + 1}`,
  })) as unknown as Json;

  test.use({
    viewport: { width: 1440, height: 560 },
    mockConfig: {
      overrides: {
        ...BOOTSTRAP_OVERRIDES,
        '/api/v1/dashboard': () => EMPTY_BOARD,
        '/api/v1/nodes/search': () => FLEET,
        '/api/v1/nodes/{node_id}/interfaces': () => ROSTER,
        '/api/v1/nodes/{node_id}/interfaces/{ifindex}/series': (url: URL) => seriesBody(url),
      },
    },
  });

  test('the node list inside the ⚙ panel stays on screen', async ({ page }) => {
    const pop = await openTrafficSettings(page);
    await pop.locator('.nodepick-trigger').click();

    const list = await pop.locator('.nodepick-pop').boundingBox();
    expect(list, 'the node list did not open').not.toBeNull();
    const vh = page.viewportSize()!.height;

    // Both edges, because a flip that trades the bottom for the top fixes nothing.
    expect(list!.y, 'the node list opened above the top of the window').toBeGreaterThanOrEqual(0);
    expect(
      list!.y + list!.height,
      'the node list runs off the bottom of the window',
    ).toBeLessThanOrEqual(vh);

    // And enough of it is on screen to choose from: a list clipped to a row and a half is
    // technically "within the viewport" the moment it is short enough.
    const rows = pop.locator('.nodepick-option');
    await expect(rows.first()).toBeVisible();
    const first = (await rows.first().boundingBox())!;
    const last = (await rows.last().boundingBox())!;
    expect(last.y + last.height - first.y, 'barely any of the node list is usable').toBeGreaterThan(
      60,
    );
  });
});

// Per-card names (ADR-071). The decisive assertion is that the widget TYPE survives: the name is
// drawn beside it, never instead of it, and a test that only looked for the operator's words would
// pass just as well against the replace-the-heading design this ADR started as and abandoned.
test('a placed card can be given its own name beside the type', async ({ page, errors }) => {
  await openCatalog(page);
  await card(page, 'Interface traffic').click();
  await page.locator('.modal').getByRole('button', { name: 'Done' }).click();

  const cell = page.locator('.mydash-cell').first();
  // Still in Customize, which is where the rename box lives.
  const box = cell.locator('.widgetframe-name');
  await expect(box).toBeVisible();
  await box.fill('HQ uplink');
  await box.press('Enter');

  // Leave edit mode: the box becomes text, and both halves are on screen.
  await page.getByRole('button', { name: 'Done' }).click();
  await expect(cell.locator('.widgetframe-kind')).toHaveText('Interface traffic');
  await expect(cell.locator('.widgetframe-own')).toHaveText('HQ uplink');

  // The controls that name this card announce the same line the screen shows — neither the type
  // alone (six cards would sound identical) nor the name alone (the screen shows two things).
  await page.getByRole('button', { name: 'Customize' }).click();
  await expect(
    cell.getByRole('button', { name: 'Remove Interface traffic · HQ uplink' }),
  ).toBeVisible();

  // Clearing the box is the undo: the type is left standing on its own.
  await cell.locator('.widgetframe-name').fill('');
  await cell.locator('.widgetframe-name').press('Enter');
  await page.getByRole('button', { name: 'Done' }).click();
  await expect(cell.locator('.widgetframe-kind')).toHaveText('Interface traffic');
  await expect(cell.locator('.widgetframe-own')).toHaveCount(0);

  expect(errors.uncaught).toEqual([]);
});

// The rename box must fit what is typed into it (ADR-071). It sits in `.card-title`, which is
// shrink-to-fit by default — so the box was sized to its own content and clipped a name of any
// length while several hundred pixels of the card header sat empty beside it.
test('the rename box is wide enough for a long name', async ({ page }) => {
  await openCatalog(page);
  await card(page, 'Interface traffic').click();
  await page.locator('.modal').getByRole('button', { name: 'Done' }).click();

  const name = page.locator('.mydash-cell').first().locator('.widgetframe-name');
  await expect(name).toBeVisible();
  await name.fill('Home Matsuyama Aggregation uplink');

  // The direct statement of "all of it is visible": the content is not wider than the box. Reading
  // the geometry rather than the text matters — the value is in the DOM either way, so an
  // assertion on `inputValue()` passes while the operator can only see half of it.
  const fits = await name.evaluate(
    (el: HTMLInputElement) => el.scrollWidth <= el.clientWidth + 1,
  );
  expect(fits, 'the typed name is clipped by the box').toBe(true);
});
