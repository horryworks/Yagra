// SPDX-License-Identifier: AGPL-3.0-only
import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import { describe, expect, it } from 'vitest';
import { sourceFiles as walkSources } from '../testSupport/sources';
import { merakiOrgPath, nodeHref, nodesPageHref } from './entityHref';

const NODE = '0b0e6a9e-1c1c-4d5e-8a3f-2f4f6a7b8c9d';
const GROUP = '5f1d2c3b-4a59-4e6f-9a0b-1c2d3e4f5a6b';

describe('nodeHref', () => {
  it('is the node detail page', () => {
    expect(nodeHref(NODE)).toBe(`/nodes/${NODE}`);
  });
});

describe('nodesPageHref (re-exported)', () => {
  it('spells a uuid selection exactly as the hand-written encodeURIComponent form did', () => {
    // PollersPage and GeoMapPage built `?sel=` with encodeURIComponent before ADR-184 increment 8.
    // For a uuid the two encoders agree byte for byte, so moving them changed no URL.
    expect(nodesPageHref({ kind: 'node', id: NODE })).toBe(
      `/nodes?sel=${encodeURIComponent(`node:${NODE}`)}`,
    );
    expect(nodesPageHref({ kind: 'group', id: GROUP })).toBe(
      `/nodes?sel=${encodeURIComponent(`group:${GROUP}`)}`,
    );
  });
});

describe('merakiOrgPath (re-exported)', () => {
  it('is one organization page, keyed by its Yagra uuid', () => {
    expect(merakiOrgPath('o-1')).toBe('/settings/integrations/meraki/o-1');
  });
});

/**
 * The guard: a link to the node detail page is spelled by `nodeHref` and nowhere else.
 *
 * Exempt:
 *  - this module, which is the one spelling;
 *  - tests, which assert the URL;
 *  - `services/` (and the generated `api/`), where `/nodes/{id}/…` is an API endpoint, not a link to
 *    a page — a different URL space that merely shares a prefix;
 *  - comment lines, which describe a path rather than build one (`services/typedPaths.ts` quotes
 *    one, and so do several module docs).
 *
 * ⚠️ **The needles are assembled at runtime.** A literal would match this file's own source.
 */
describe('no screen builds a node detail link by hand', () => {
  const SRC = join(__dirname, '..');
  const TICK = String.fromCharCode(96);
  const NEEDLES = [
    `${TICK}/nodes/${'$'}{`,
    `'/nodes/' ${'+'}`,
    `"/nodes/" ${'+'}`,
  ];
  const EXEMPT_DIRS = ['services', 'api'];

  function rel(p: string): string {
    return p.slice(SRC.length + 1).replace(/\\/g, '/');
  }

  function sources(dir: string): string[] {
    return walkSources(dir, { skipDirs: EXEMPT_DIRS, declarations: true }).filter(
      (p) => rel(p) !== 'lib/entityHref.ts',
    );
  }

  function isComment(line: string): boolean {
    const t = line.trimStart();
    return t.startsWith('//') || t.startsWith('*') || t.startsWith('/*');
  }

  it('every link to /nodes/:id goes through nodeHref', () => {
    const offenders = sources(SRC).flatMap((p) =>
      readFileSync(p, 'utf8')
        .split('\n')
        .map((line, i) => [line, i + 1] as const)
        .filter(([line]) => !isComment(line) && NEEDLES.some((n) => line.includes(n)))
        .map(([, n]) => `${rel(p)}:${n}`),
    );

    expect(
      offenders,
      `these sources spell the node detail URL themselves. Use nodeHref from lib/entityHref ` +
        `(or nodesPageHref, if the All nodes tree is the page you mean):\n  ${offenders.join('\n  ')}`,
    ).toEqual([]);
  });

  it('finds the sources it is supposed to be reading', () => {
    // Without this, a broken path turns the check above into a test that scans nothing and passes.
    const all = sources(SRC);
    expect(all.length).toBeGreaterThan(400);
    expect(all.filter((p) => readFileSync(p, 'utf8').includes('nodeHref(')).length)
      .toBeGreaterThanOrEqual(10);
  });
});
