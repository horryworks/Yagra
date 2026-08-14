// SPDX-License-Identifier: AGPL-3.0-only
// Tier2a: does the deployed thing actually run, and is what nginx handed the browser what nginx
// was configured to hand it?
//
// ⚠️ THIS IS DELIBERATELY THIN, and the reason is worth keeping. The instinct is to re-assert here
// everything Tier1 asserts, on real data. That is the failure mode ADR-052 決定 9 names outright:
// Tier2 becomes a slow duplicate, and a slow gate is a disabled gate. Tier1 already proves each
// screen renders the data it was given, and it can prove it *better*, because it chose the data.
// What Tier1 cannot see is that the bundle in the running image is the one that was built, that
// nginx answers for it, and that the SPA survives contact with data nobody wrote down.
//
// The delivery-edge assertions below are the two `fix(web)` regressions in the last 300 commits
// that no mocked run could ever have caught — both of them cache/serving behaviour, both of them
// invisible to a suite that serves `dist/` itself.

import { NAV_SCREENS } from '../ui/screens';
import { expect, test } from './support/live';

/** The screens, from `src/nav.ts`, exactly as Tier1 derives them — one list, two tiers. */
const SCREENS = NAV_SCREENS;

test.describe('every nav screen survives the real deployment', () => {
  for (const screen of SCREENS) {
    test(`${screen.path} renders`, async ({ page, errors }) => {
      await page.goto(screen.path);

      // The shell is up: `AppShell` renders the nav for every authenticated route. If the token
      // seed failed or the bundle threw during boot, this is what notices.
      await expect(page.getByRole('navigation').first()).toBeVisible();

      // No `<Navigate>` bounce. A screen that redirects to /login or /dashboard would otherwise
      // "render" perfectly — the assertion above would pass on the page it landed on instead.
      expect(new URL(page.url()).pathname, 'the screen redirected somewhere else').toBe(
        screen.path,
      );

      // There is no ErrorBoundary in this app: a render throw blanks the page and nothing else
      // reports it. Real data is what makes this worth repeating outside Tier1 — a device's real
      // sysDescr, a real interface count, a real event volume are shapes no fixture chose.
      expect(errors.uncaught, `${screen.path} threw while rendering`).toEqual([]);
      expect(errors.logged, `${screen.path} logged a console error`).toEqual([]);
    });
  }
});

test.describe('what nginx served', () => {
  test('answers every asset the document asked for, with the declared cache policy', async ({
    page,
    traffic,
  }) => {
    await page.goto('/dashboard');
    await expect(page.getByRole('navigation').first()).toBeVisible();

    // 🚨 The failure this exists for: an upgrade replaces the document root, a browser holding the
    // pre-upgrade index.html asks for hashed bundles that no longer exist, and `location /assets/`
    // answers 404 rather than falling through to index.html. A 200 on the document therefore says
    // nothing — which is exactly what `/flashdeploy`'s two `curl`s can see and no more.
    const broken = traffic.assets.filter((a) => a.status >= 400);
    expect(broken, 'the page asked for something the deployment does not have').toEqual([]);

    // `web/nginx.conf` declares both halves of the policy, and they are a pair: hashed assets are
    // immutable *because* the document is revalidated. Asserting only one would let the dangerous
    // half rot — a cached index.html naming bundles that are gone is the blank page above.
    const document = traffic.assets.find((a) => a.type === 'document');
    expect(document, 'no document response was observed').toBeTruthy();
    expect(document?.cacheControl, 'the SPA entry point may not be served without revalidating').toContain(
      'no-cache',
    );

    const hashed = traffic.assets.filter((a) => a.url.startsWith('/assets/'));
    expect(hashed.length, 'no hashed bundle was requested — is this really the built SPA?').toBeGreaterThan(
      0,
    );
    for (const asset of hashed) {
      expect(asset.cacheControl, `${asset.url} is not cacheable`).toContain('immutable');
    }
  });

  test('a hashed asset that does not exist is a 404, not the index page', async ({ page }) => {
    // The other half of the same decision, stated directly. Falling through to index.html would
    // answer a `.js` request with the SPA document; the browser rejects it on MIME grounds and the
    // page is blank with nothing saying why. `try_files $uri =404` is what stops that, and nothing
    // between releases exercises it.
    const res = await page.request.get('/assets/does-not-exist-ymock.js');
    expect(res.status()).toBe(404);

    // ⚠️ NOT "the content type is not text/html" — that was the first version of this assertion and
    // it failed, because nginx's own 404 page is HTML and always was. `nginx.conf` declares a
    // status code, not a media type, so the status is the contract and the thing to rule out is
    // specifically the SPA document being served under it.
    expect(await res.text(), 'the deployment answered a missing bundle with the app').not.toContain(
      'id="root"',
    );
  });
});
