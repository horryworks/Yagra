// SPDX-License-Identifier: AGPL-3.0-only
// The four addresses ADR-055 Inc.2 vacated still work, and still carry what they were given.
//
// Why this is a browser test and not a unit one: the redirect is a route, and the thing that can go
// wrong is not the destination — it is the QUERY. `<Navigate to="/events" replace />` compiles,
// renders, redirects, and silently drops `?node_id=<uuid>`. The result is a perfectly healthy
// Events page showing every node's events instead of one node's. Nothing throws, nothing 404s,
// nothing logs. The only way to see it is to follow the link and look at the URL that comes out,
// which is what this does.
//
// `/alerts/events?node_id=…` is not hypothetical: the node detail Events tab emits exactly it, and
// it is what an operator pastes into a chat during an incident.

import { expect, test } from '../support/app';

/** A URL any UUID satisfies — the mock answers `/nodes/{id}` for whatever we ask. */
const NODE = '00000000-0000-4000-8000-000000000001';

const MOVES: { from: string; to: string; what: string }[] = [
  { from: '/alerts/events', to: '/events', what: 'the event log' },
  { from: '/alerts/event-sources', to: '/events/webhooks', what: 'webhook sources' },
  { from: '/settings/forwarding', to: '/events/forwarding', what: 'forwarding' },
  { from: '/settings/credentials', to: '/nodes/credentials', what: 'credentials' },
];

for (const { from, to, what } of MOVES) {
  test(`${from} still reaches ${what}`, async ({ page }) => {
    await page.goto(from);
    await expect(page).toHaveURL(new RegExp(`${to.replace(/\//g, '\\/')}$`), { timeout: 15_000 });
  });
}

test('a redirect carries the query string it was given, not just the path', async ({ page }) => {
  await page.goto(`/alerts/events?node_id=${NODE}`);
  await expect(page).toHaveURL(new RegExp(`\\/events\\?node_id=${NODE}$`), { timeout: 15_000 });
});
