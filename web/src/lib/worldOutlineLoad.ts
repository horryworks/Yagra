// SPDX-License-Identifier: AGPL-3.0-only
// The coastline, loaded once for every small map that draws it (the dashboard's Geo map widget and
// the top-bar clock's zone map, ADR-221 Inc.2). The Geo map page imports the module directly.

/** The coastline as the Geo map widget and the top-bar clock draw it. */
export interface Outline {
  land: readonly string[];
  lakes: readonly string[];
}

let outlineLoad: Promise<Outline> | null = null;

/** The coastline, fetched the first time a Geo map widget mounts or the clock's map opens, and
 *  shared after that. It is ~240 KB compressed, so a screen showing neither never asks for it (the
 *  Geo map page imports the same module, so all three share one chunk). A failed fetch is not
 *  cached, so the next mount asks again. */
export function loadOutline(): Promise<Outline> {
  outlineLoad ??= import('../pages/worldOutline').then(
    (m) => ({ land: m.WORLD_OUTLINE, lakes: m.WORLD_LAKES }),
    (e: unknown) => {
      outlineLoad = null;
      throw e;
    },
  );
  return outlineLoad;
}
