// SPDX-License-Identifier: AGPL-3.0-only
// The reveal after a cleared search lands on the node even when folders above it fill in late
// (ADR-073 Inc.2, ④ from the lab box).
//
// Measured on a lab deployment with ~3,000 nodes: search for a node in a folder of hundreds, pick
// it, clear the box with ✕ — the folder opened, but the tree stopped ~3,000px short. The tree had
// scrolled exactly once, to the row's index at that moment, and the members of a folder ABOVE it
// landed afterwards: a hundred rows inserted over the target pushed it off the bottom of the pane.
//
// ⚠️ The order is forced here rather than hoped for: the folder above answers well after the target's
// own folder, which is the order the lab produced (a batch of on-screen folders queued behind the
// reveal's single request). With both folders answering at once the bug does not show at all.

import { expect, test } from "../support/app";
import { BOOTSTRAP_OVERRIDES } from "../support/bootstrap";
import { defaultBodyFor, MOCK_PREFIX, type Json } from "../support/openapi";

type Page = import("@playwright/test").Page;

const ABOVE_ID = "00000000-0000-4000-8000-0000000000d1";
const TARGET_ID = "00000000-0000-4000-8000-0000000000d2";
const ABOVE_SIZE = 120;
const TARGET_SIZE = 200;
const PICK = 150;

const nodeId = (folder: "a" | "b", i: number) =>
  `00000000-0000-4000-8000-${folder === "a" ? "e" : "f"}${String(i).padStart(11, "0")}`;
const nodeName = (folder: "a" | "b", i: number) =>
  `${MOCK_PREFIX}${folder}-${String(i).padStart(3, "0")}`;

const groups = (() => {
  const [template] = defaultBodyFor("/api/v1/node-groups") as Record<
    string,
    Json
  >[];
  return [
    {
      ...template,
      id: ABOVE_ID,
      name: `${MOCK_PREFIX}above`,
      parent_id: null,
      group_type: "generic",
      sort_order: 1,
    },
    {
      ...template,
      id: TARGET_ID,
      name: `${MOCK_PREFIX}target`,
      parent_id: null,
      group_type: "generic",
      sort_order: 2,
    },
  ] as unknown as Json;
})();

const members = (() => {
  const body = defaultBodyFor("/api/v1/nodes/by-group") as {
    nodes: Record<string, Json>[];
  };
  const [template] = body.nodes;
  const make = (folder: "a" | "b", groupId: string, size: number) =>
    Array.from({ length: size }, (_, i) => ({
      ...template,
      id: nodeId(folder, i),
      name: nodeName(folder, i),
      group_id: groupId,
      sort_order: i + 1,
    }));
  const byGroup: Record<string, ReturnType<typeof make>> = {
    [ABOVE_ID]: make("a", ABOVE_ID, ABOVE_SIZE),
    [TARGET_ID]: make("b", TARGET_ID, TARGET_SIZE),
  };
  return byGroup;
})();

const summary = (() => {
  const empty = {
    critical: 0,
    maintenance: 0,
    ok: 0,
    unknown: 0,
    unreachable: 0,
    warning: 0,
  };
  return {
    groups: {
      [ABOVE_ID]: { ...empty, ok: ABOVE_SIZE },
      [TARGET_ID]: { ...empty, ok: TARGET_SIZE },
    },
  } as unknown as Json;
})();

const searchOver = (url: URL): Json => {
  const term = (url.searchParams.get("search") ?? "").toLowerCase();
  const all = [...members[ABOVE_ID], ...members[TARGET_ID]];
  return {
    nodes: all.filter((n) => String(n.name).toLowerCase().includes(term)),
    truncated: false,
    next_cursor: null,
  } as unknown as Json;
};

const rowById = (page: Page, id: string) =>
  page.locator(`[id="ntree-n:${id}"]`);

async function rowInView(page: Page, id: string): Promise<boolean> {
  if ((await rowById(page, id).count()) === 0) return false;
  const row = await rowById(page, id).boundingBox();
  const pane = await page.locator(".ntree-body").boundingBox();
  if (!row || !pane) return false;
  return row.y >= pane.y && row.y + row.height <= pane.y + pane.height;
}

/** `/nodes/by-group`, answering the folder above late. Registered after the shared mock, so it
 *  wins (Playwright runs the most recently registered matching route first). */
async function membersWithLateAbove(
  page: Page,
  delays: { above: number; target: number },
) {
  await page.route("**/api/v1/nodes/by-group**", async (route) => {
    const url = new URL(route.request().url());
    const batch = url.searchParams.get("groups");
    const asked = batch
      ? batch.split(",").filter(Boolean)
      : [url.searchParams.get("group") ?? ""];
    const wait = Math.max(
      0,
      ...asked.map((k) =>
        k === ABOVE_ID ? delays.above : k === TARGET_ID ? delays.target : 0,
      ),
    );
    if (wait) await new Promise((r) => setTimeout(r, wait));
    const nodes = asked.flatMap((k) => members[k] ?? []);
    const body = batch
      ? { nodes, truncated: false, answered: asked }
      : { nodes, truncated: false };
    await route.fulfill({
      status: 200,
      contentType: "application/json",
      body: JSON.stringify(body),
    });
  });
}

test.describe("a node in a big folder, below another big folder", () => {
  test.use({
    mockConfig: {
      overrides: {
        ...BOOTSTRAP_OVERRIDES,
        "/api/v1/node-groups": groups,
        "/api/v1/fleet/group-summary": summary,
        "/api/v1/nodes": searchOver,
      },
    },
  });

  test("clearing the search with ✕ lands on the node after the folder above fills in", async ({
    page,
  }) => {
    // Start in the search, so neither folder was ever loaded by browsing.
    await membersWithLateAbove(page, { above: 1500, target: 400 });
    const target = nodeName("b", PICK);
    await page.goto(`/nodes?q=${encodeURIComponent(target)}`);
    await page
      .locator(".ntree-body")
      .getByText(target, { exact: true })
      .click();
    await expect
      .poll(() => new URL(page.url()).searchParams.get("sel"))
      .toBe(`node:${nodeId("b", PICK)}`);

    await page
      .locator(".nodes-pane-search")
      .getByRole("button", { name: "Clear search" })
      .click();
    await expect
      .poll(() => new URL(page.url()).searchParams.get("q"))
      .toBeNull();
    // The folder above has answered: its last member exists as data, so the tree is now as tall as
    // it will get. Before the fix the node had been scrolled to before this, and was then pushed
    // ABOVE_SIZE rows down.
    await expect
      .poll(
        () =>
          page.evaluate(
            () => document.querySelector(".ntree-body")?.scrollHeight ?? 0,
          ),
        {
          timeout: 10_000,
        },
      )
      .toBeGreaterThan((ABOVE_SIZE + TARGET_SIZE) * 30);
    await page.waitForTimeout(500);
    expect(
      await rowInView(page, nodeId("b", PICK)),
      "the node was pushed out of view",
    ).toBe(true);
    await expect(rowById(page, nodeId("b", PICK))).toHaveClass(/\bsel\b/);
  });

  test("a wheel in the tree hands the scroll back: the late folder does not drag it to the node", async ({
    page,
  }) => {
    await membersWithLateAbove(page, { above: 2500, target: 300 });
    const target = nodeName("b", PICK);
    await page.goto(`/nodes?q=${encodeURIComponent(target)}`);
    await page
      .locator(".ntree-body")
      .getByText(target, { exact: true })
      .click();
    await expect
      .poll(() => new URL(page.url()).searchParams.get("sel"))
      .toBe(`node:${nodeId("b", PICK)}`);

    await page
      .locator(".nodes-pane-search")
      .getByRole("button", { name: "Clear search" })
      .click();
    await expect.poll(() => rowInView(page, nodeId("b", PICK))).toBe(true);
    // The operator scrolls away while the folder above is still on its way.
    const body = page.locator(".ntree-body");
    await body.hover();
    await page.mouse.wheel(0, -600);
    await expect
      .poll(() => body.evaluate((el) => el.scrollTop))
      .toBeLessThan(TARGET_SIZE * 30);
    const parked = await body.evaluate((el) => el.scrollTop);
    await expect
      .poll(() => body.evaluate((el) => el.scrollHeight), { timeout: 10_000 })
      .toBeGreaterThan((ABOVE_SIZE + TARGET_SIZE) * 30);
    await page.waitForTimeout(300);
    expect(
      await body.evaluate((el) => el.scrollTop),
      "the tree moved under the operator",
    ).toBe(parked);
  });
});
