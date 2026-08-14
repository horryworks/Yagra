// SPDX-License-Identifier: AGPL-3.0-only
// The Tier1 interception layer: every `/api/**` request is answered from the OpenAPI document,
// so no backend runs (ADR-052 決定 1). One route handler, not several — Playwright matches
// handlers in reverse registration order, and a suite whose correctness depends on that ordering
// is a suite that breaks when someone adds a route in the wrong place.

import type { Page, Route } from '@playwright/test';
import {
  defaultResponse,
  isReachableOverrideKey,
  matchRoute,
  templatesLike,
  type Json,
} from './openapi';

export type Override = Json | ((url: URL) => Json);

export interface MockConfig {
  /** Keyed by pathname (`/api/v1/auth/me`) or by path template (`/api/v1/analysis/jobs/{id}`),
   *  pathname first. Wins over the generated default. */
  overrides?: Record<string, Override>;
  /** Keyed by pathname. Answers with this status and the standard error envelope instead —
   *  the lever the "prove it can fail" check pulls. */
  failures?: Record<string, number>;
}

export interface MockState {
  /** `/api/**` requests matching no path template in the document. Must stay empty: a request the
   *  contract does not describe is either a hand-rolled `fetch` that escaped the typed client, or
   *  a stale committed document. Both are findings. */
  unmatched: string[];
  /** Requests answered, for the "this screen actually asked for something" check. */
  served: string[];
  /** Every answered request with its query string kept.
   *
   *  `served` deliberately drops the query so it reads as a list of endpoints; this keeps it,
   *  because for a server-filtered list "did the filter reach the request?" is the whole
   *  question — `fix(web): the History filters never reached the request` was exactly a screen
   *  that narrowed its own state and never told the server. */
  requests: { method: string; pathname: string; search: string }[];
}

const SSE_PREFIX = '/api/v1/stream/';

function envelope(code: string, message: string): string {
  return JSON.stringify({ error: { code, message } });
}

async function fulfilJson(route: Route, status: number, body: Json | undefined): Promise<void> {
  if (body === undefined) {
    await route.fulfill({ status, headers: { 'content-length': '0' }, body: '' });
    return;
  }
  await route.fulfill({ status, contentType: 'application/json', body: JSON.stringify(body) });
}

export async function installMockApi(page: Page, config: MockConfig = {}): Promise<MockState> {
  const state: MockState = { unmatched: [], served: [], requests: [] };
  const { overrides = {}, failures = {} } = config;

  // Fail loudly on a key nothing can reach. Without this the test still runs, the screen still
  // renders, and the assertion is made against the generated default instead of the data the test
  // asked for — a green test about a fiction. See `isReachableOverrideKey`.
  const unreachable = [...Object.keys(overrides), ...Object.keys(failures)].filter(
    (key) => !isReachableOverrideKey(key),
  );
  if (unreachable.length > 0) {
    const hints = unreachable
      .map((key) => {
        const near = templatesLike(key).slice(0, 4);
        return near.length ? `  ${key}\n    did you mean: ${near.join(' | ')}` : `  ${key}`;
      })
      .join('\n');
    throw new Error(
      `mock override keys match no path in the OpenAPI document — they would be silently ignored:\n${hints}`,
    );
  }

  await page.route('**/api/**', async (route) => {
    const request = route.request();
    const url = new URL(request.url());
    const pathname = url.pathname;
    const method = request.method();
    const label = `${method} ${pathname}`;
    state.requests.push({ method, pathname, search: url.search });

    // SSE first. `services/sse.ts` streams over `fetch` (not `EventSource`, which cannot carry the
    // bearer token), so an unhandled stream would hang the request until the test times out.
    // `AppShell` mounts `TroubleshootToast`, so all four streams open on *every* screen.
    // An immediately-ended stream makes the client reconnect after RECONNECT_MS (3s); over a
    // page-load test that is at most one extra request, which is cheaper than holding a route open.
    if (pathname.startsWith(SSE_PREFIX)) {
      state.served.push(label);
      await route.fulfill({ status: 200, contentType: 'text/event-stream', body: ':ok\n\n' });
      return;
    }

    const failure = failures[pathname];
    if (failure !== undefined) {
      state.served.push(label);
      await route.fulfill({
        status: failure,
        contentType: 'application/json',
        body: envelope('mock_forced_failure', `forced ${failure} for ${pathname}`),
      });
      return;
    }

    // Match the route BEFORE consulting overrides, so an override may be keyed by the template
    // (`/api/v1/analysis/jobs/{id}`) as well as by a literal pathname. An unmatched path is still
    // reported as unmatched even if an override would have answered it — the point of that list is
    // "the app asked for something the contract does not describe", which an override would hide.
    const entry = matchRoute(pathname, method);
    const override = overrides[pathname] ?? (entry ? overrides[entry.template] : undefined);
    if (override !== undefined && entry) {
      state.served.push(label);
      await fulfilJson(route, 200, typeof override === 'function' ? override(url) : override);
      return;
    }

    if (!entry) {
      state.unmatched.push(label);
      await route.fulfill({
        status: 404,
        contentType: 'application/json',
        body: envelope('mock_unmatched', `${label} matches no path in the OpenAPI document`),
      });
      return;
    }

    state.served.push(label);
    const res = defaultResponse(entry, pathname);
    if (res.contentType && res.contentType !== 'application/json') {
      await route.fulfill({ status: res.status, contentType: res.contentType, body: '' });
      return;
    }
    await fulfilJson(route, res.status, res.body);
  });

  return state;
}
