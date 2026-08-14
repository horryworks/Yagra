// SPDX-License-Identifier: AGPL-3.0-only
// Refuse to run the walk against a stale bundle.
//
// 🚨 THIS GUARD WAS EARNED. While proving the walk can fail, a deliberate `throw` was planted in a
// page and the suite stayed green — because `test:ui` serves `dist/`, and `dist/` was the previous
// build. Fifty passing tests said nothing about the working tree. That is the same failure this
// repo has paid for on the backend (a green CI shipping a binary built from a cached `COPY . .`),
// and it is worse here, because a UI suite exists precisely to be believed.
//
// Both callers build immediately beforehand (`/verify` Step 18, `/flashdeploy`'s web guard), so in
// normal use this never fires. It fires for the person running `npm run test:ui` by hand after an
// edit — which is exactly when a false green would be most convincing.

import { readdirSync, statSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { isTier2Only } from './selection';

const WEB_ROOT = resolve(dirname(fileURLToPath(import.meta.url)), '../..');

/** Newest mtime anywhere under a directory. */
function newestMtime(dir: string): number {
  let newest = 0;
  for (const entry of readdirSync(dir, { withFileTypes: true })) {
    const full = join(dir, entry.name);
    newest = Math.max(newest, entry.isDirectory() ? newestMtime(full) : statSync(full).mtimeMs);
  }
  return newest;
}

export default function globalSetup(): void {
  // Tier2 serves nothing from `dist/` — it drives whatever the deployment is running, and that is
  // the point of it, so this guard would refuse a perfectly valid run for the wrong reason.
  // ⚠️ `FullConfig.projects` is NOT filtered by `--project`; it is the declared list. That was the
  // first guess and it fired on a Tier2 run. See `selection.ts`.
  if (isTier2Only()) return;

  const indexHtml = join(WEB_ROOT, 'dist', 'index.html');
  let built: number;
  try {
    built = statSync(indexHtml).mtimeMs;
  } catch {
    throw new Error(
      'web/dist is missing. `npm run test:ui` serves the built bundle and never builds it — run `npm run build` first.',
    );
  }

  const sources = Math.max(
    newestMtime(join(WEB_ROOT, 'src')),
    statSync(join(WEB_ROOT, 'index.html')).mtimeMs,
  );

  if (sources > built) {
    const behind = Math.round((sources - built) / 1000);
    throw new Error(
      `web/dist is ${behind}s older than web/src — the walk would have verified the PREVIOUS build ` +
        'and reported it as green. Run `npm run build`, then `npm run test:ui`.',
    );
  }
}
