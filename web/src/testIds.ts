// SPDX-License-Identifier: AGPL-3.0-only
// Test hooks, in one place (ADR-052 決定 4).
//
// WHY A MAP AND NOT JUST `data-testid="…"`. A testid is a fact repeated in two files that **the
// compiler does not ask for** — nothing fails when a component is renamed, a screen is rewritten,
// or the string is misspelled on one side. That is the exact shape `extensibility.md` opens with:
// a fact that must be repeated in N places will eventually be repeated in N-1. Routed through
// this map the spelling is type-checked, every hook is visible in one file, and
// `testIds.test.ts` refuses a `data-testid` written inline anywhere under `src/`.
//
// WHEN TO ADD ONE — sparingly, and only when nothing else can see the thing:
//   1. Prefer a role + accessible name. It asserts the a11y contract at the same time.
//   2. Prefer the text the screen actually renders.
//   3. A testid is for what has neither: an SVG shape, a canvas, a purely visual affordance.
//
// ⚠️ A testid ships in the production bundle — Tier2 runs against the real deployment, so these
// cannot be stripped at build time. Name them after what the operator sees, never after internal
// identifiers or state.

export const TEST_IDS = {
  /** One site pin on the Geo map. An SVG `<g>`: no text node, so no text query can reach it, and
   *  the walk would otherwise be unable to tell "plotted the groups it was given" from "drew an
   *  empty world map". */
  geoMapPin: 'geo-map-pin',
} as const;

export type TestId = (typeof TEST_IDS)[keyof typeof TEST_IDS];
