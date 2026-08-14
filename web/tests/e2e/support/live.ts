// SPDX-License-Identifier: AGPL-3.0-only
// The Tier2 fixture: a signed-in browser pointed at a REAL deployment, with nothing mocked.
//
// Tier1 and Tier2 answer different questions, and mixing them is how Tier2 becomes a slow
// duplicate nobody runs (ADR-052 決定 9). Tier1 owns everything reachable by controlling the data.
// What is left here — and the only thing that belongs here — is what no mock can produce:
//
//   (i)   two surfaces of the running system agreeing on a fact neither test nor mock chose,
//   (ii)  that what was *delivered* is what runs (real image, real nginx, real TLS, real headers),
//   (iii) the shape real data actually takes, rather than the shape a fixture asserts it takes,
//   (iv)  the integration seams — the proxy, the session, the stream, the scope.
//
// 🚨 READ-ONLY IS MECHANICAL, NOT POLICY. Tier2a runs against a live deployment on every deploy, so
// "these tests only read" cannot rest on the author's intent or on RBAC — the account is Admin and
// could write. Instead every request the browser makes is observed and a non-GET fails the test.
// The allow-list below is two entries long and each one is a read that happens to use POST.

import { test as base, expect, type Page } from '@playwright/test';
import { request as httpsRequest } from 'node:https';
import { matchRoute } from '../../support/openapi';

/** One JSON round-trip to the deployment, from Node rather than from the browser.
 *
 *  ⚠️ `rejectUnauthorized: false` is the self-signed certificate ADR-044 makes the default, and it
 *  is set HERE rather than by `NODE_TLS_REJECT_UNAUTHORIZED=0` on purpose: the env var turns
 *  verification off for the whole process, this turns it off for exactly the calls aimed at the
 *  deployment under test. `security.md` says not to disable verification; narrowing the exception
 *  to two call sites is the difference between an exception and a habit. (The browser side is the
 *  project's `ignoreHTTPSErrors`, which is a browser-context option and does nothing for `fetch`.) */
function json<T>(
  url: string,
  init: { method?: string; headers?: Record<string, string>; body?: string } = {},
): Promise<{ status: number; body: T }> {
  return new Promise((resolve, reject) => {
    const req = httpsRequest(
      url,
      {
        method: init.method ?? 'GET',
        headers: init.headers,
        rejectUnauthorized: false,
      },
      (res) => {
        const chunks: Buffer[] = [];
        res.on('data', (c: Buffer) => chunks.push(c));
        res.on('end', () => {
          const text = Buffer.concat(chunks).toString('utf8');
          try {
            resolve({ status: res.statusCode ?? 0, body: JSON.parse(text) as T });
          } catch {
            reject(new Error(`${url} answered ${res.statusCode} with a non-JSON body`));
          }
        });
      },
    );
    req.on('error', reject);
    if (init.body) req.write(init.body);
    req.end();
  });
}

/** Same three names `tests/README.md` documents. Values live only in `web/.env.e2e`. */
export interface LiveEnv {
  baseUrl: string;
  user: string;
  password: string;
}

export function liveEnv(): LiveEnv {
  const { E2E_BASE_URL, E2E_USER, E2E_PASSWORD } = process.env;
  if (!E2E_BASE_URL || !E2E_USER || !E2E_PASSWORD) {
    // Unreachable in practice — `playwright.config.ts` does not declare the project at all without
    // E2E_BASE_URL. Kept so a half-filled file fails saying which name is missing.
    throw new Error(
      'Tier2 needs E2E_BASE_URL, E2E_USER and E2E_PASSWORD in web/.env.e2e (see tests/README.md).',
    );
  }
  return { baseUrl: E2E_BASE_URL.replace(/\/$/, ''), user: E2E_USER, password: E2E_PASSWORD };
}

/** POST endpoints that read. Each needs a reason, and the list is asserted against the OpenAPI
 *  document so it cannot name a path that does not exist. */
export const READ_POSTS: { path: string; why: string }[] = [
  {
    path: '/api/v1/node-names',
    why: 'The fleet-wide name resolver. It is a POST only because the id list is unbounded and would not fit a query string; it changes nothing.',
  },
];

/** The sign-in POST, allowed only for the one spec that drives the real form. */
export const LOGIN_PATH = '/api/v1/auth/login';

/** The whole read-only rule, as a function, so it can be asserted directly.
 *
 *  🚨 A guard nobody tested is a guard that might allow everything — this repo has the scar
 *  (`msg_regex` rejected every pattern while the boundary tests, which only checked rejections,
 *  stayed green). `harness.spec.ts` exercises both answers. */
export function isForbiddenWrite(
  req: { method: string; pathname: string },
  opts: { allowLogin: boolean },
): boolean {
  if (req.method === 'GET' || req.method === 'HEAD') return false;
  if (req.method !== 'POST') return true;
  if (opts.allowLogin && req.pathname === LOGIN_PATH) return false;
  return !READ_POSTS.some((r) => r.path === req.pathname);
}

export interface Traffic {
  /** Every `/api/` request the browser made, in order. */
  requests: { method: string; pathname: string; search: string }[];
  /** Non-API responses (the document, the hashed bundles, fonts) with their status and headers —
   *  the delivery edge Tier1 structurally cannot see, because Tier1 serves `dist/` itself. */
  assets: { url: string; status: number; type: string; cacheControl: string | null }[];
}

export interface PageErrors {
  uncaught: string[];
  logged: string[];
}

/** Console errors that are expected on a live deployment. Empty is the correct state; an entry is
 *  a decision, not a convenience. */
export const ALLOWED_CONSOLE_ERRORS: { pattern: RegExp; why: string }[] = [];

interface Fixtures {
  /** Allow the one sign-in POST. Only `auth.spec.ts` sets this. */
  allowLogin: boolean;
  /** Invert the guard: the test PASSES only if a forbidden write was seen. The one spec that
   *  proves this machinery works sets it; nothing else may. */
  expectWrites: boolean;
  traffic: Traffic;
  errors: PageErrors;
  /** Read the live API directly, with the same session the browser is using. This is what makes a
   *  UI⟷API assertion possible without deciding the answer in advance. */
  api: <T>(path: string) => Promise<T>;
}

/** Log in over the API, retrying while core is still coming up.
 *
 *  ⚠️ `/flashdeploy` restarts the stack immediately before Tier2a runs, and core's in-image
 *  HEALTHCHECK has `start-period=20s`. A single attempt would fail on a 502/503 that resolves
 *  itself seconds later, and a gate that reports a red for that reason is a gate people disable. */
async function login(env: LiveEnv): Promise<string> {
  const deadline = Date.now() + 45_000;
  let last = '';
  for (;;) {
    try {
      const res = await json<{ token?: string }>(`${env.baseUrl}${LOGIN_PATH}`, {
        method: 'POST',
        headers: { 'content-type': 'application/json' },
        body: JSON.stringify({ username: env.user, password: env.password }),
      });
      if (res.status === 200 && res.body.token) return res.body.token;
      last =
        res.status === 200
          ? 'the sign-in succeeded but returned no token'
          : `sign-in answered ${res.status}`;
    } catch (e) {
      last = `sign-in could not reach ${env.baseUrl}: ${String(e)}`;
    }
    if (Date.now() > deadline) throw new Error(`Tier2 could not sign in — ${last}`);
    await new Promise((r) => setTimeout(r, 2_000));
  }
}

async function seedSession(page: Page, token: string): Promise<void> {
  // Same shape Tier1 seeds. `useAuthStore.authed` is decided at module-init from `getToken()`, so
  // this has to land before the bundle evaluates; `addInitScript` is what guarantees that. The
  // prefs pin the locale to EN so an assertion about visible copy means one thing.
  await page.addInitScript(
    ([t]) => {
      localStorage.setItem('yagra_token', t);
      localStorage.setItem(
        'yagra_prefs',
        JSON.stringify({ state: { theme: 'dark', language: 'en', uiMode: 'desktop' }, version: 0 }),
      );
    },
    [token],
  );
}

interface WorkerFixtures {
  /** A bearer token for the `e2e` account, obtained over the API before any browser starts.
   *
   *  Worker-scoped on purpose. Per-test it would mean one sign-in per screen — forty-odd rows in
   *  the deployment's audit log every run, for no added coverage, since what a sign-in proves is
   *  proved once. `auth.spec.ts` still drives the real form, which is where that belongs. */
  token: string;
}

export const test = base.extend<Fixtures, WorkerFixtures>({
  allowLogin: [false, { option: true }],
  expectWrites: [false, { option: true }],

  token: [
    // eslint-disable-next-line no-empty-pattern -- a fixture signature always takes the bag first
    async ({}, use) => {
      await use(await login(liveEnv()));
    },
    { scope: 'worker' },
  ],

  // 🚨 `auto: true`, and the reason is Inc.2's most expensive lesson: Playwright only builds the
  // fixtures a test *destructures*, so an opt-in guard is a guard a spec can silently skip. A
  // Tier2 spec that forgot to ask for `traffic` would run with no read-only enforcement at all and
  // look exactly like one that had it.
  traffic: [
    async ({ page, token, allowLogin, expectWrites }, use) => {
      const traffic: Traffic = { requests: [], assets: [] };
      const seen: string[] = [];

      await seedSession(page, token);

      page.on('request', (req) => {
        const url = new URL(req.url());
        if (!url.pathname.startsWith('/api/')) return;
        const seenReq = { method: req.method(), pathname: url.pathname, search: url.search };
        traffic.requests.push(seenReq);
        if (isForbiddenWrite(seenReq, { allowLogin })) {
          seen.push(`${seenReq.method} ${seenReq.pathname}`);
        }
      });

      page.on('response', (res) => {
        const url = new URL(res.url());
        if (url.pathname.startsWith('/api/')) return;
        traffic.assets.push({
          url: url.pathname,
          status: res.status(),
          type: res.request().resourceType(),
          cacheControl: res.headers()['cache-control'] ?? null,
        });
      });

      await use(traffic);

      // Park first, so a stream reconnecting during teardown cannot append to `seen` after the
      // assertion has read it.
      await page.goto('about:blank').catch(() => undefined);
      if (expectWrites) {
        expect(
          seen,
          'the read-only guard did not notice a deliberate write — it is not wired to anything',
        ).not.toEqual([]);
        return;
      }
      expect(
        seen,
        'Tier2a is read-only and this run wrote to a live deployment. Either the screen under test ' +
          'performs a write on load (a finding), or this test belongs in Tier2b (ADR-052 Inc.4).',
      ).toEqual([]);
    },
    { auto: true },
  ],

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

  api: async ({ token }, use) => {
    const env = liveEnv();
    await use(async <T,>(path: string): Promise<T> => {
      const res = await json<T>(`${env.baseUrl}${path}`, {
        headers: { authorization: `Bearer ${token}` },
      });
      if (res.status !== 200) throw new Error(`GET ${path} answered ${res.status}`);
      return res.body;
    });
  },
});

/** The allow-list cannot name a path the contract does not describe — a typo would quietly narrow
 *  the guard into failing on a legitimate read, and the error would point at the screen. */
export function readPostsAreRealEndpoints(): string[] {
  return [...READ_POSTS.map((r) => r.path), LOGIN_PATH].filter(
    (p) => matchRoute(p, 'POST') === undefined,
  );
}

export { expect };
