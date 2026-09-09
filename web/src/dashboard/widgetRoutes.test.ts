// SPDX-License-Identifier: AGPL-3.0-only
// The `reads` declarations are an access-control table (ADR-123 決定 5/6), and almost nothing can
// check them: what a `.tsx` fetches is not statically reachable, so "does this widget really read
// that route" has no mechanical answer — the public board's "view as anonymous" preview is the
// only way to find an omission. What CAN be checked is everything around that gap, and this file
// checks all of it, because the parts that are checkable are the parts that fail silently.

import { readFileSync } from 'node:fs';
import { describe, expect, it } from 'vitest';
import { REGISTRY } from './registry';

const WIDGET_ROUTES: Record<string, string[]> = JSON.parse(
  readFileSync(new URL('./widgetRoutes.json', import.meta.url), 'utf8'),
);
const OPENAPI: { paths: Record<string, Record<string, unknown>> } = JSON.parse(
  readFileSync(new URL('../api/openapi.json', import.meta.url), 'utf8'),
);

/** Every `METHOD /path` the backend actually serves, from the committed contract. */
function servedRoutes(): Set<string> {
  const out = new Set<string>();
  for (const [path, ops] of Object.entries(OPENAPI.paths)) {
    for (const method of Object.keys(ops)) out.add(`${method.toUpperCase()} ${path}`);
  }
  return out;
}

describe('widget route declarations', () => {
  it('every widget declares at least one route it reads', () => {
    // Required by the type, so this cannot fail at runtime — it fails at `tsc`. Asserted anyway
    // because the type could be relaxed to optional by someone who reads the field as
    // documentation, and an optional one defaults to "declares nothing", which is indistinguishable
    // from a widget that genuinely reads nothing.
    for (const def of REGISTRY) {
      expect(def.reads.length, `${def.type} declares no routes`).toBeGreaterThan(0);
    }
    expect(REGISTRY.length).toBeGreaterThanOrEqual(40);
  });

  it('every declared route is one the backend actually serves', () => {
    // 🚨 The failure this exists for is quiet in the worst way: a declared route that does not
    // exist contributes nothing to the anonymous allow-list, so the widget renders empty for
    // visitors and correctly for the admin who composed the board.
    const served = servedRoutes();
    const bad: string[] = [];
    for (const def of REGISTRY) {
      for (const route of def.reads) if (!served.has(route)) bad.push(`${def.type}: ${route}`);
    }
    expect(bad, 'declared routes the OpenAPI document does not describe').toEqual([]);
  });

  it('the committed widgetRoutes.json matches the registry', () => {
    // The generated half, checked the way `schema.d.ts` is: regenerate and diff. Here the
    // comparison is in-process so a stale file fails the unit suite rather than only CI.
    const fromRegistry = Object.fromEntries(REGISTRY.map((d) => [d.type, [...d.reads]]));
    expect(WIDGET_ROUTES).toEqual(fromRegistry);
  });

  it('no widget declares a route that writes', () => {
    // ADR-123 決定 7: `public_dashboard` opens reads only, and the allow-list must not become a
    // way around that. `POST /api/v1/node-names` is the one POST allowed through — it is an
    // id→name lookup guarded by `RequireView`, the read-shaped write `api-conventions.md`
    // describes — so it is named here rather than pattern-matched.
    const READ_SHAPED_WRITES = ['POST /api/v1/node-names'];
    const bad: string[] = [];
    for (const def of REGISTRY) {
      for (const route of def.reads) {
        if (!route.startsWith('GET ') && !READ_SHAPED_WRITES.includes(route)) {
          bad.push(`${def.type}: ${route}`);
        }
      }
    }
    expect(bad, 'a widget declares a mutating route').toEqual([]);
  });

  it('the audit widget is still the case that must never reach the public board', () => {
    // A recognition test for the filter in `CatalogModal`: `GET /api/v1/audit` takes `view_audit`,
    // so the audit widget is the one every reviewer pictures. Pinning it here means the catalog
    // filter has a concrete example that must stay excluded, rather than only a rule.
    const audit = REGISTRY.find((d) => d.type === 'audit');
    expect(audit?.reads).toEqual(['GET /api/v1/audit']);
  });
});
