// SPDX-License-Identifier: AGPL-3.0-only
// Wait until a deployment reports the version we just published, then let Tier2a run against it.
//
// WHY THIS EXISTS. Since ADR-050 a deployment upgrades *itself* — a person opens Settings ▸ Upgrade
// and presses the button — so `/release` ends with the images published and nothing anywhere
// running them. That is the gap ADR-050 knowingly opened: what remains is "all three images are
// pullable", which says they exist, not that they run.
//
// Tier2a can answer the rest, but only against a deployment that is actually on the new version,
// and only after a human has decided to upgrade. So the decision stays with the person and the
// *waiting* is automated: `/release` starts this in the background, the operator presses Upgrade
// whenever they like, and the 26-second suite runs itself the moment the version flips.
//
// ⚠️ **The trigger is core's version, and core is not the whole release.** `/api/v1/version`
// reports `yagra-core`'s crate version only; the WebUI carries its own build version, which is why
// Settings ▸ About shows both. A release whose web image failed to move would still trip this
// trigger. Tier2a's own assertions are what have to catch that, not this script.
//
// Not written in TypeScript on purpose: nothing transpiles on this path (Playwright compiles the
// specs, not a bare `node` script), so this is plain ESM and its ~10 lines of HTTPS do not share
// `support/live.ts`'s reader. The reason for relaxing verification is identical and equally
// scoped — one request, to a deployment whose certificate is self-signed by design (ADR-044).

import { readFileSync } from 'node:fs';
import { request } from 'node:https';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const HERE = dirname(fileURLToPath(import.meta.url));
const WEB = join(HERE, '..', '..');

/** Not a failure of the deployment — nobody pressed the button. Distinct from 1 so a caller can
 *  tell "not verified" from "the check itself broke". */
const EXIT_NEVER_UPGRADED = 2;

try {
  process.loadEnvFile(join(WEB, '.env.e2e'));
} catch {
  // Absent is normal on a machine that has never been pointed at a deployment; the check below
  // turns that into one clear sentence rather than a stack trace.
}

const baseUrl = process.env.E2E_BASE_URL?.replace(/\/$/, '');
if (!baseUrl) {
  console.error('E2E_BASE_URL is not set — see web/tests/README.md. Nothing to wait for.');
  process.exit(1);
}

/** The version to wait for. `web/package.json` mirrors the workspace version, which is what
 *  `CARGO_PKG_VERSION` — and therefore `/api/v1/version` — reports, so during a release the default
 *  is already right and nothing has to be passed.
 *
 *  ⚠️ The override is an **environment variable, not an argument**: `npm run x -- 0.2.7` appends to
 *  the *end* of the script string, so in a chained script the value would land on `playwright`
 *  rather than here. An explicit value may carry the tag's `v`. */
const target = (
  process.env.E2E_TARGET_VERSION ??
  process.argv[2] ??
  JSON.parse(readFileSync(join(WEB, 'package.json'), 'utf8')).version
)
  .trim()
  .replace(/^v/, '');

const pollMs = Number(process.env.E2E_UPGRADE_POLL_MS ?? 10_000);
const deadlineMs = Number(process.env.E2E_UPGRADE_DEADLINE_MS ?? 20 * 60_000);

/** `GET {baseUrl}/api/v1/version`. Unauthenticated (`security(())` on the handler), so this needs
 *  no credentials at all — only the address. */
function runningVersion() {
  return new Promise((resolve) => {
    const req = request(
      `${baseUrl}/api/v1/version`,
      { method: 'GET', rejectUnauthorized: false, timeout: 5_000 },
      (res) => {
        let body = '';
        res.on('data', (c) => (body += c));
        res.on('end', () => {
          if (res.statusCode !== 200) return resolve(null);
          try {
            resolve(JSON.parse(body).core ?? null);
          } catch {
            resolve(null);
          }
        });
      },
    );
    req.on('error', () => resolve(null));
    req.on('timeout', () => {
      req.destroy();
      resolve(null);
    });
    req.end();
  });
}

const started = Date.now();
let first = null;
let answered = false;

for (;;) {
  const seen = await runningVersion();
  if (seen !== null) {
    answered = true;
    if (first === null) {
      first = seen;
      console.log(
        seen === target
          ? `${baseUrl} is already on ${target} — nothing to wait for.`
          : `${baseUrl} is on ${seen}; waiting for ${target}. Press Settings ▸ Upgrade when ready.`,
      );
    }
    if (seen === target) {
      console.log(`${baseUrl} reports ${target}. Running Tier2a against it.`);
      process.exit(0);
    }
  }

  if (Date.now() - started >= deadlineMs) {
    // Seconds below a minute: the default deadline is 20 minutes, but a caller who shortened it is
    // usually testing this script, and "after 0 minutes" reads as a bug in the message.
    const waited =
      deadlineMs < 60_000
        ? `${Math.round(deadlineMs / 1000)} seconds`
        : `${Math.round(deadlineMs / 60_000)} minutes`;
    console.error(
      answered
        ? `NOT VERIFIED: ${baseUrl} is still on ${first} after ${waited} — it was never ` +
            `upgraded to ${target}, so nothing has rendered the published artefact in a browser. ` +
            `This is not a fault in the release; press Settings ▸ Upgrade and re-run ` +
            `\`npm run test:e2e\`.`
        : `NOT VERIFIED: ${baseUrl} never answered /api/v1/version in ${waited}. Check ` +
            `the address in web/.env.e2e and whether the deployment is up.`,
    );
    process.exit(EXIT_NEVER_UPGRADED);
  }

  // A stack mid-upgrade refuses connections for a while; that is the normal path through here, not
  // an error, so failures are silent until the deadline.
  await new Promise((r) => setTimeout(r, pollMs));
}
