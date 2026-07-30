// SPDX-License-Identifier: AGPL-3.0-only
// public/favicon.svg is a static asset — it cannot import brandMark.ts, so this test is the only
// thing keeping the browser-tab icon and the in-app <Logo> the same mark (extensibility.md §2).
// Whitespace is normalized, so re-indenting the SVG is fine; changing a coordinate is not.
// (`?raw` rather than node:fs — the WebUI deliberately has no @types/node, whose globals would
// shadow the DOM timer types the app relies on.)
import faviconSvg from '../../../public/favicon.svg?raw';
import { describe, expect, it } from 'vitest';
import {
  BRAND,
  MARK,
  MARK_EDGES,
  MARK_NODES,
  MARK_VIEWBOX,
  MARK_WEIGHTS,
  SEAL_TILE,
} from './brandMark';

const favicon = faviconSvg.replace(/\s+/g, ' ');

describe('favicon.svg mirrors the brand mark', () => {
  it('draws it in the same viewBox on the same seal tile', () => {
    expect(favicon).toContain(`viewBox="${MARK_VIEWBOX}"`);
    expect(favicon).toContain(
      `<rect x="${SEAL_TILE.x}" y="${SEAL_TILE.y}" width="${SEAL_TILE.size}" ` +
        `height="${SEAL_TILE.size}" rx="${SEAL_TILE.rx}" fill="${BRAND}"/>`,
    );
  });

  it('draws the same edges at the favicon weight', () => {
    expect(favicon).toContain(`stroke="${MARK}" stroke-width="${MARK_WEIGHTS.favicon.stroke}"`);
    for (const d of MARK_EDGES) expect(favicon).toContain(`<path d="${d}"/>`);
  });

  it('draws the same nodes at the favicon weight', () => {
    const r = MARK_WEIGHTS.favicon.node;
    for (const n of MARK_NODES) {
      expect(favicon).toContain(`<circle cx="${n.cx}" cy="${n.cy}" r="${r}"/>`);
    }
  });
});
