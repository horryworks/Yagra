// SPDX-License-Identifier: AGPL-3.0-only
// The read-only guard, tested in both directions.
//
// 🚨 A guard that only ever answers "allowed" is indistinguishable from no guard, and this repo
// has already paid for that shape: `msg_regex` rejected every pattern it was given while the
// boundary tests — which only ever checked rejections — stayed green for its whole life. So this
// asserts what the guard permits *and* what it stops, and it asserts the allow-list names real
// endpoints, because a typo there would narrow the guard silently rather than widen it.
//
// The rule itself is not decorative. Tier2a runs on every deploy against a live system with an
// Admin token; "these tests only read" cannot rest on the author's care.

import {
  isForbiddenWrite,
  LOGIN_PATH,
  READ_POSTS,
  readPostsAreRealEndpoints,
  expect,
  test,
} from './support/live';

test('the allow-list names endpoints that exist', () => {
  expect(
    readPostsAreRealEndpoints(),
    'an allow-listed path is not a POST route in the OpenAPI document — it would never match, and ' +
      'the guard would fail on the legitimate request it was meant to permit',
  ).toEqual([]);

  // Every entry carries its reason. An allow-list that grows without an argument is how "read-only"
  // stops meaning anything, and the argument is what a reviewer reads.
  for (const entry of READ_POSTS) {
    expect(entry.why.length, `${entry.path} was allow-listed without a reason`).toBeGreaterThan(20);
  }
});

test('the guard stops a write and permits a read', () => {
  const opts = { allowLogin: false };

  // Reads pass, whatever they are.
  expect(isForbiddenWrite({ method: 'GET', pathname: '/api/v1/nodes' }, opts)).toBe(false);
  expect(isForbiddenWrite({ method: 'HEAD', pathname: '/api/v1/nodes' }, opts)).toBe(false);
  for (const entry of READ_POSTS) {
    expect(isForbiddenWrite({ method: 'POST', pathname: entry.path }, opts)).toBe(false);
  }

  // Writes do not — including the ones a screen could plausibly fire on load.
  expect(isForbiddenWrite({ method: 'POST', pathname: '/api/v1/nodes' }, opts)).toBe(true);
  expect(isForbiddenWrite({ method: 'PUT', pathname: '/api/v1/settings/system' }, opts)).toBe(true);
  expect(isForbiddenWrite({ method: 'DELETE', pathname: '/api/v1/nodes/abc' }, opts)).toBe(true);
  expect(isForbiddenWrite({ method: 'PATCH', pathname: '/api/v1/nodes/abc' }, opts)).toBe(true);
});

test.describe('the guard is wired to the browser, not just to itself', () => {
  test.use({ expectWrites: true });

  test('a non-GET from the page is caught', async ({ page }) => {
    // 🚨 The three tests above prove the *rule*. This proves the rule is connected: a classifier
    // nobody calls, and a `page.on('request')` listener that stopped firing, both leave every
    // Tier2 test green while nothing is being watched. Tier1 keeps `detects.spec.ts` for the same
    // reason — a suite with only passing examples cannot show that it works.
    //
    // The probe is a POST to a path the router does not serve, so it reaches core, is answered
    // 404, and cannot act on anything. Read-only survives literally as well as in spirit.
    await page.goto('/dashboard');
    const answered = page.waitForResponse((r) => r.url().includes('/api/v1/e2e-guard-probe'));
    await page.evaluate(() =>
      fetch('/api/v1/e2e-guard-probe', { method: 'POST' }).catch(() => undefined),
    );
    // Wait for it to land, so the observation cannot race teardown. The assertion itself is the
    // fixture's: with `expectWrites`, teardown fails unless the guard saw this.
    expect((await answered).status()).toBe(404);
  });
});

test('the sign-in exemption is exactly one path, and only when asked for', () => {
  expect(isForbiddenWrite({ method: 'POST', pathname: LOGIN_PATH }, { allowLogin: false })).toBe(
    true,
  );
  expect(isForbiddenWrite({ method: 'POST', pathname: LOGIN_PATH }, { allowLogin: true })).toBe(
    false,
  );
  // The exemption does not spread to the rest of auth: logout, OIDC callback and token rotation
  // are all POSTs under the same prefix, and none of them is a read.
  expect(
    isForbiddenWrite({ method: 'POST', pathname: '/api/v1/auth/logout' }, { allowLogin: true }),
  ).toBe(true);
});
