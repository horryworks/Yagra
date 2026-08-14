// SPDX-License-Identifier: AGPL-3.0-only
// Playwright — the browser-level verification runner (ADR-052). Deliberately separate from Vitest:
// specs live in `tests/**/*.spec.ts` and Vitest collects `src/**/*.test.ts`, so the two cannot
// capture each other's files by extension *or* by directory. The `.tsx` test ban stays in force
// (`.claude/rules/testing.md`) — jsdom has no layout engine, which is the whole reason this exists.
//
// Tier1 ("ui") serves the production bundle from `dist/` and intercepts every API call, so no
// backend runs. Tier2 ("e2e", ADR-052 Inc.3) drives a real deployment over its real TLS edge and
// mocks nothing — it only exists when `web/.env.e2e` names one, so a checkout without those
// credentials still runs `test:ui` and simply has no Tier2 project to select.
//
// ⚠️ `dist/` must already exist for Tier1 — `npm run test:ui` does NOT build. That is the point:
// the two callers (`/verify` Step 18 and `/flashdeploy`) both run `npm run build` beforehand for
// their own reasons, and rebuilding here would pay for it twice. `vite preview` fails loudly if it
// is absent.

import { defineConfig, devices, type Project } from '@playwright/test';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { isTier2Only } from './tests/support/selection';

const PORT = 4173;

// ── Tier2 credentials (ADR-052 決定 6) ────────────────────────────────────────────────────────
// Admin, write-capable, on the test server; the file is git-ignored by the root `.env.*` rule and
// the values are never written down in the repo — `tests/README.md` documents the variable NAMES
// only. `loadEnvFile` is built into Node (20.12+), so this costs no dependency.
//
// ⚠️ It must not overwrite a variable already exported in the environment: that is how a
// shell-level override stays in charge. `loadEnvFile` leaves existing keys alone — verified, not
// assumed. The path is resolved from this file rather than from `cwd`, so the config behaves the
// same whether it was reached through the npm script or by `--config` from the repo root.
try {
  process.loadEnvFile(join(dirname(fileURLToPath(import.meta.url)), '.env.e2e'));
} catch {
  // Absent is the normal state on a machine that has never been pointed at a deployment.
}
const E2E_BASE_URL = process.env.E2E_BASE_URL;

const projects: Project[] = [
  { name: 'ui', testDir: './tests/ui', use: { ...devices['Desktop Chrome'] } },
];
if (E2E_BASE_URL) {
  projects.push({
    name: 'e2e',
    testDir: './tests/e2e',
    use: {
      ...devices['Desktop Chrome'],
      baseURL: E2E_BASE_URL,
      // The deployment serves its own self-signed certificate and there is deliberately no
      // plaintext listener to fall back to — `http://` does not even 301, because a webhook sender
      // would not follow one (ADR-044). So the base URL is always `https://` and the cert is
      // always untrusted; refusing it here would mean Tier2 could never run at all.
      ignoreHTTPSErrors: true,
    },
    // A real deployment answers over the network and repaints from live data. Tier1's 20s is a
    // statement about a local bundle, not about this.
    timeout: 60_000,
  });
}

export default defineConfig({
  testDir: './tests',
  // Refuses to run against a `dist/` older than `src/`. See the file — the guard exists because a
  // deliberately broken page once left the whole suite green.
  globalSetup: './tests/support/globalSetup.ts',
  fullyParallel: true,
  // No retries. A flaky UI test that passes on the second attempt is a flaky UI, and hiding it
  // behind a retry is how a suite stops meaning anything.
  retries: 0,
  reporter: [['list']],
  timeout: 20_000,
  expect: { timeout: 5_000 },
  use: {
    baseURL: `http://localhost:${PORT}`,
    trace: 'retain-on-failure',
    screenshot: 'only-on-failure',
  },
  projects,
  // Tier2 drives a deployment, not `dist/`. Starting `vite preview` for it would make the live
  // suite fail on a missing local build, which has nothing to do with what it is testing.
  webServer: isTier2Only()
    ? undefined
    : {
        command: `npm run preview -- --port ${PORT} --strictPort`,
        url: `http://localhost:${PORT}/`,
        reuseExistingServer: true,
        timeout: 60_000,
      },
});
