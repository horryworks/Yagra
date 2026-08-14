// SPDX-License-Identifier: AGPL-3.0-only
// Which Playwright projects this run selected.
//
// Both the config and `globalSetup` need the answer before Playwright will give it to them: the
// config resolves `webServer` before it knows the selection, and `FullConfig.projects` in
// `globalSetup` is the *declared* list, not the filtered one — checked, and it is why the stale-
// `dist` guard fired on a Tier2-only run that serves nothing from `dist/`. So the command line is
// read directly, in one place, rather than guessed at in two.

/** Project names named with `--project` / `--project=…`; empty when the run took all of them. */
export function selectedProjects(argv: readonly string[] = process.argv): string[] {
  return argv.flatMap((a, i, all) =>
    a === '--project' ? [all[i + 1] ?? ''] : a.startsWith('--project=') ? [a.slice(10)] : [],
  );
}

/** True when the run touches only the live-deployment project, which needs no local build. */
export function isTier2Only(argv: readonly string[] = process.argv): boolean {
  const selected = selectedProjects(argv);
  return selected.length > 0 && selected.every((p) => p === 'e2e');
}
