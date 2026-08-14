// SPDX-License-Identifier: AGPL-3.0-only
// The Tier1 test fixture: a signed-in browser with every API call intercepted, plus the two
// error channels a broken screen actually shows up on.
//
// There is no `ErrorBoundary` anywhere in this app (checked). An exception thrown while rendering
// therefore unmounts the React tree and leaves a blank page — no HTTP error, no failed request,
// nothing a `curl` can see. `pageerror` is the only place it surfaces, which is why it is a hard
// failure here and not a warning.

import { test as base, expect } from '@playwright/test';
import { BOOTSTRAP_OVERRIDES } from './bootstrap';
import { installMockApi, type MockConfig, type MockState } from './mockApi';

/** Console errors that are expected and understood. Each entry needs a reason, and adding one is
 *  a decision — an allowlist that grows without argument is how "no console errors" stops meaning
 *  anything. Empty is the correct state. */
export const ALLOWED_CONSOLE_ERRORS: { pattern: RegExp; why: string }[] = [];

export interface PageErrors {
  /** Uncaught exceptions. With no ErrorBoundary these blank the screen. Always a failure. */
  uncaught: string[];
  /** `console.error` output not covered by ALLOWED_CONSOLE_ERRORS. */
  logged: string[];
}

interface Fixtures {
  mock: MockState;
  errors: PageErrors;
  /** Per-test mock configuration. Override in a `test.use({ mockConfig: … })` block. */
  mockConfig: MockConfig;
}

export const test = base.extend<Fixtures>({
  mockConfig: [{ overrides: BOOTSTRAP_OVERRIDES }, { option: true }],

  // Runs before the test body, so interception and the seeded session are in place before the
  // first navigation. `useAuthStore.authed` is decided at module-init time from `getToken()`, so
  // the token has to exist before the bundle evaluates — `addInitScript` is what guarantees that.
  //
  // 🚨 `auto: true` is load-bearing. Playwright only builds the fixtures a test *destructures*, so
  // while this was opt-in, a test written as `async ({ page })` ran with **no interception at
  // all** — every request went past the mock to the preview server's `/api` proxy and the screen
  // sat on "Loading…". Nothing said so; the failure looked like the feature under test. Making
  // it automatic means a spec cannot accidentally opt out of the thing that makes it a test.
  mock: [
    async ({ page, mockConfig }, use) => {
      // ⚠️ `filterRowOpen: true` is a deliberate departure from the shipped default, which is
      // *closed* (ADR-053 Inc.9). Every filter spec here is about what the row contains and where
      // it sits, not about whether it appears — making twenty tests press a button first would test
      // the button twenty times and the geometry once. The default itself is asserted, in both
      // directions, by `tests/ui/filterToggle.spec.ts`, which seeds it closed.
      await page.addInitScript(() => {
        localStorage.setItem('yagra_token', 'ymock-token');
        localStorage.setItem(
          'yagra_prefs',
          JSON.stringify({
            state: { theme: 'dark', language: 'en', uiMode: 'desktop', filterRowOpen: true },
            version: 0,
          }),
        );
      });
      const state = await installMockApi(page, mockConfig);
      await use(state);
      // Park the page before the routes come down. Otherwise the SSE reconnect and any in-flight
      // fetch outlive interception by a few milliseconds and reach the preview server — which
      // inherits `server.proxy` and forwards `/api` to localhost:8080. Harmless when nothing is
      // listening (a wall of ECONNREFUSED in the log), but on a workstation that happens to be
      // running yagra-core it would send test traffic at a real backend.
      await page.goto('about:blank').catch(() => undefined);
    },
    { auto: true },
  ],

  // Auto for the same reason: a spec that forgets to destructure `errors` would still *look* like
  // it covers the screen, while an uncaught exception went unrecorded. Requesting it only decides
  // whether the test asserts on the result, never whether it is collected.
  errors: [
    async ({ page }, use) => {
      const errors: PageErrors = { uncaught: [], logged: [] };
      page.on('pageerror', (e) => errors.uncaught.push(String(e)));
      page.on('console', (msg) => {
        if (msg.type() !== 'error') return;
        const text = msg.text();
        if (ALLOWED_CONSOLE_ERRORS.some((a) => a.pattern.test(text))) return;
        errors.logged.push(text);
      });
      await use(errors);
    },
    { auto: true },
  ],
});

export { expect };
