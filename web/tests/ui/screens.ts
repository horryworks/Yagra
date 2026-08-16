// SPDX-License-Identifier: AGPL-3.0-only
// What the route walk covers, and what "it rendered" means for each screen (ADR-052 決定 3).
//
// The LIST is derived from `src/nav.ts` — the IA's source of truth — so a screen added to the nav
// is walked from that moment, with no list here to forget. What cannot be derived is the
// *expectation*: a screen made of numbers and charts has no generated string to look for. So the
// per-screen decision is an explicit map, and `screens.spec.ts` pins its key set to the nav in
// both directions. Adding a screen is then one line, and forgetting it is a failed test naming the
// path — the shape `components/NodeDetail/tabs.ts` uses for the same reason.

import { NAV, sectionItems } from '../../src/nav';
import { REPORT_TOOL } from '../support/bootstrap';
import { TEST_IDS } from '../../src/testIds';

export interface Screen {
  path: string;
  /** Short name for the test title. */
  label: string;
  /** Query string the screen needs to show anything (a report needs the run it is reporting on).
   *  Kept apart from `path` because the walk asserts the *pathname* did not move. */
  query?: string;
}

/** Every nav item, flattened. Deduplicated because a section's landing path repeats its first
 *  child. Measured today: 42. The number is not written down anywhere on purpose. */
export const NAV_SCREENS: Screen[] = [
  ...new Map(
    NAV.flatMap((s) => sectionItems(s)).map((i) => [i.path, { path: i.path, label: i.labelKey }]),
  ).values(),
];

/** Routes with no nav entry, because they are parameterized or outside the shell. These ARE a
 *  hand-written list, and that is the honest cost of a route the IA does not name — `routes.tsx`
 *  and the three `routeGroups/` files are where to check it against. */
export const EXTRA_SCREENS: Screen[] = [
  // Any UUID matches the `/nodes/{id}` template, so the mock answers whatever we ask for.
  { path: '/nodes/00000000-0000-4000-8000-000000000001', label: 'nodes.detail' },
  { path: '/settings/integrations/meraki', label: 'settings.integrations.meraki' },
  // A report screen with no `?job=` is a scope form, not a report. The id is arbitrary — the mock
  // answers `/analysis/jobs/{id}` for any of them.
  {
    path: `/troubleshoot/report/${REPORT_TOOL}`,
    label: 'troubleshoot.report',
    query: '?job=00000000-0000-4000-8000-000000000002',
  },
  { path: '/login', label: 'login' },
];

export const ALL_SCREENS: Screen[] = [...NAV_SCREENS, ...EXTRA_SCREENS];

/**
 * Screens that legitimately have no `.pageheader-note` — ADR-055 G1's exemption table.
 *
 * Two rows, and that number is the argument for the check existing at all: the ADR reserved the
 * right to abandon G1 if the exemptions outgrew ten, on the grounds that maintaining an exception
 * list is not the same as being protected. Measured before writing it, four screens rendered a
 * `PageHeader` with no note (fixed in Inc.1) and two rendered no `PageHeader` at all — these.
 *
 * A reason here is not a formality. When one of these screens gains a normal header, its row is
 * what tells the next person the exemption has expired.
 */
export const NOTE_EXEMPT: Record<string, string> = {
  '/login': 'The sign-in form, outside the app shell. It has no page header and wants none.',
  '/nodes/00000000-0000-4000-8000-000000000001':
    'Node detail carries its own identity header (name, address, kind badges) instead of the shared one — the subject IS the explanation.',
};

export type Expect =
  /** A generated `ymock-` string is visible: the data reached the screen. The default. */
  | { kind: 'marker' }
  /** The screen renders its data as numbers, so assert the sentence those numbers produce. Weaker
   *  than a marker and coupled to English copy (the walk pins the locale), but it is still an
   *  assertion *about the served data* — "1 node in the inventory" is false if the fetch failed. */
  | { kind: 'text'; text: string }
  /** No generated string can reach this screen (SVG-only, charts). Assert a specific element. */
  | { kind: 'locator'; sel: string }
  /** Nothing beyond "no crash, no redirect, no unmocked call" is assertable. Needs a reason —
   *  and every reason here should be re-read when the screen gains content. */
  | { kind: 'none'; why: string };

const MARKER: Expect = { kind: 'marker' };

/** One entry per screen in ALL_SCREENS — no more, no less. See `screens.spec.ts`. */
export const SCREEN_EXPECT: Record<string, Expect> = {
  '/dashboard': MARKER,
  '/dashboard/my': MARKER,
  '/dashboard/reports': MARKER,
  '/nodes': MARKER,
  '/nodes/discovery': MARKER,
  '/nodes/profiles': MARKER,
  '/nodes/classification-rules': MARKER,
  '/nodes/collection-templates': MARKER,
  '/nodes/mib': MARKER,
  '/topology/map': { kind: 'text', text: 'node in the inventory' },
  '/topology/dependency': MARKER,
  // The one testid in the tree. An SVG `<g>` has no text for a query to reach, so without it the
  // walk could not tell "plotted the groups it was given" from "drew an empty world map".
  '/topology/geo': { kind: 'locator', sel: `[data-testid="${TEST_IDS.geoMapPin}"]` },
  '/alerts': MARKER,
  '/alerts/history': MARKER,
  '/events': MARKER,
  '/alerts/rules': MARKER,
  '/alerts/routing': MARKER,
  '/alerts/event-rules': MARKER,
  '/events/webhooks': MARKER,
  '/alerts/maintenance': MARKER,
  '/alerts/mutes': MARKER,
  '/troubleshoot': {
    kind: 'none',
    why: 'The tool catalog comes from the client-side registry, not the API. The only served data on it is the run counters, which are numbers.',
  },
  '/troubleshoot/runs': MARKER,
  '/troubleshoot/scheduled': MARKER,
  '/troubleshoot/findings': MARKER,
  '/settings/system-health': MARKER,
  '/settings/pollers': MARKER,
  '/events/forwarding': MARKER,
  '/settings/integrations': {
    kind: 'none',
    why: 'A static catalog card per integration. The Meraki connection state IS derived from the API but renders as a fixed phrase.',
  },
  '/settings/ai': MARKER,
  '/settings/system': {
    kind: 'none',
    why: 'Every field is a number or a checkbox (intervals, retention days, walk toggles) — no string for a marker to ride in on.',
  },
  '/settings/tls': MARKER,
  '/settings/config-bundle': {
    kind: 'none',
    why: 'Export/import buttons and prose only; the screen reads nothing until an operator picks a file.',
  },
  '/settings/support-bundle': {
    kind: 'none',
    why: 'A log-window select, a node picker and one button; the screen reads nothing until an operator presses it. The node picker does resolve names from the API, but only once opened.',
  },
  '/settings/upgrade': MARKER,
  '/nodes/credentials': MARKER,
  '/settings/users': MARKER,
  '/settings/roles': MARKER,
  '/settings/auth': MARKER,
  '/settings/api-tokens': MARKER,
  '/settings/audit': MARKER,
  '/settings/preferences': {
    kind: 'none',
    why: 'Purely local prefs (theme, language, layout). It calls no API at all, which is itself worth knowing.',
  },
  '/settings/about': MARKER,
  '/nodes/00000000-0000-4000-8000-000000000001': MARKER,
  '/settings/integrations/meraki': MARKER,
  [`/troubleshoot/report/${REPORT_TOOL}`]: MARKER,
  '/login': {
    kind: 'none',
    why: 'The sign-in form, reached unauthenticated by design. Its behaviour (success / 401 / 429) is Inc.2.',
  },
};
