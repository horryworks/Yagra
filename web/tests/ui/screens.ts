// SPDX-License-Identifier: AGPL-3.0-only
// What the route walk covers, and what "it rendered" means for each screen (ADR-052 decision 3).
//
// The LIST is derived from `src/nav.ts` — the IA's source of truth — so a screen added to the nav
// is walked from that moment, with no list here to forget. What cannot be derived is the
// *expectation*: a screen made of numbers and charts has no generated string to look for. So the
// per-screen decision is an explicit map, and `screens.spec.ts` pins its key set to the nav in
// both directions. Adding a screen is then one line, and forgetting it is a failed test naming the
// path — the shape `components/NodeDetail/tabs.ts` uses for the same reason.

import { NAV, sectionItems } from '../../src/nav';
import { REPORT_TOOL } from '../support/bootstrap';
import { defaultBodyFor } from '../support/openapi';
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

/**
 * One Meraki organization's page, opened on **the organization the mock serves**.
 *
 * Unlike `/nodes/{id}`, any UUID will not do here: the page has no endpoint of its own for the
 * organization — it picks the row whose `id` matches the route out of `GET /meraki/orgs` — so an
 * invented id renders its "does not exist" notice, which would pass every check below while
 * walking none of the screen. The id is read from the generated body rather than transcribed, so
 * it moves with the generator instead of drifting from it.
 */
export const MERAKI_ORG_SCREEN = `/settings/integrations/meraki/${
  (defaultBodyFor('/api/v1/meraki/orgs') as { id: string }[])[0].id
}`;

/** Routes with no nav entry, because they are parameterized or outside the shell. These ARE a
 *  hand-written list, and that is the honest cost of a route the IA does not name — `routes.tsx`
 *  and the three `routeGroups/` files are where to check it against. */
export const EXTRA_SCREENS: Screen[] = [
  // Any UUID matches the `/nodes/{id}` template, so the mock answers whatever we ask for.
  { path: '/nodes/00000000-0000-4000-8000-000000000001', label: 'nodes.detail' },
  { path: '/settings/integrations/meraki', label: 'settings.integrations.meraki' },
  { path: MERAKI_ORG_SCREEN, label: 'settings.integrations.meraki.org' },
  { path: '/settings/integrations/netbox', label: 'settings.integrations.netbox' },
  // A report screen with no `?job=` is a scope form, not a report. The id is arbitrary — the mock
  // answers `/analysis/jobs/{id}` for any of them.
  {
    path: `/troubleshoot/report/${REPORT_TOOL}`,
    label: 'troubleshoot.report',
    query: '?job=00000000-0000-4000-8000-000000000002',
  },
  // Off the menu since ADR-191 (opened from the tree, a node's detail and a Geo map pin), so the
  // nav-derived walk no longer reaches it.
  { path: '/topology/map', label: 'topology.map' },
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

/** The page-note limit (ADR-200): the nav description's 80 English characters. */
export const NOTE_MAX = 80;

/**
 * Screens whose page note is still longer than `NOTE_MAX`, with the increment that shortens it.
 * Each is a `PAGE_NOTES` entry with `until` in `src/proseBudget.test.ts` — a fact the screen does
 * not show yet. Only shrinks: the walk fails an entry whose note has come down to the limit, so a
 * row cannot outlive its reason.
 */
export const LONG_NOTE: Record<string, string> = {
  '/dashboard/public': 'Inc.21 — nothing else is reachable without an account',
  '/events': 'Inc.16 — unmatched events are kept for 24 hours',
  '/events/forwarding': 'Inc.16 — flow and BigQuery destinations',
  '/events/webhooks': 'Inc.14 — the token is shown once',
  '/alerts/event-rules': 'Inc.14 — an info rule only records',
  '/nodes/collection-templates': 'Inc.13 — editing a set changes every profile using it',
  '/nodes/missing-prefixes': 'Inc.12 — what a site is, and where addresses come from',
  '/nodes/subnet-overlaps': 'Inc.12 — what a site is',
  '/settings/ai': 'Inc.8 — nothing runs until a provider is set',
  '/settings/audit': 'Inc.8 — kept for 365 days',
  '/settings/auth': 'Inc.7 — local accounts keep working',
  '/settings/integrations/netbox': 'Inc.6 — read-only, never writes to NetBox',
  [MERAKI_ORG_SCREEN]: 'Inc.6 — what the organization page holds',
  '/settings/pollers': 'Inc.4 — a pool with no live poller is not monitored',
  '/settings/system': 'Inc.5 — per-profile settings take precedence',
  '/settings/tls': 'Inc.5 — takes effect within seconds, no restart',
  '/settings/users': 'Inc.8 — what the three roles mean',
  '/topology/dependency': 'Inc.23 — the upstream decides suppression',
  '/topology/map': 'Inc.23 — off the menu, so it has no nav description',
  [`/troubleshoot/report/${REPORT_TOOL}`]: 'Inc.22 — each analysis explains its own scoring',
};

/**
 * The walk's tenth check (ADR-200): the characters of a screen's own prose — every element whose own
 * text is 40 characters or more, outside the shell, the page note, alerts and table rows, and not
 * served by the mock (`screenGeometry.ts`). Measured on the first run; lower a number in the change
 * that removed the prose, and raise one only with the reason in the commit message.
 *
 * A screen with no entry has a ceiling of 0, so a new screen starts with no prose at all.
 * Blind spots: dialogs, tabs other than the default, Japanese, and phone width.
 */
export const PROSE_CEILING: Record<string, number> = {
  '/alerts/routing': 156,
  '/alerts/rules': 734,
  '/dashboard/public': 182,
  '/events': 159,
  '/events/forwarding': 97,
  '/nodes': 41,
  '/nodes/discovery': 856,
  '/nodes/duplicates': 338,
  '/nodes/missing-prefixes': 411,
  '/nodes/reclassify': 263,
  '/nodes/subnet-overlaps': 284,
  '/settings/about': 139,
  '/settings/ai': 581,
  '/settings/auth': 1024,
  '/settings/config-bundle': 564,
  '/settings/integrations': 210,
  '/settings/integrations/meraki': 250,
  [MERAKI_ORG_SCREEN]: 470,
  '/settings/integrations/netbox': 251,
  '/settings/pollers': 891,
  '/settings/relocation': 1851,
  '/settings/roles': 149,
  '/settings/support-bundle': 321,
  '/settings/system': 2035,
  '/settings/system-health': 269,
  '/settings/tls': 940,
  '/settings/upgrade': 813,
  '/topology/dependency': 151,
  '/topology/map': 239,
  '/troubleshoot': 2076,
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
  // The public board starts empty and the Tier1 mock has no saved layout, so there is no widget
  // marker to find — what renders is the empty state and the warning banner. The banner rather than
  // `NONE`: it is the one thing on this screen that must never silently disappear (ADR-055 R6), and
  // an empty board is exactly when a missing warning would go unnoticed.
  // ⚠️ **The banner element, not one of its texts.** It says three things: "visible from outside"
  // until the switch has answered, then "live" or "Not published". The mock answers "not
  // published", so asserting the first text passed only when the check happened to look before
  // that request landed — green alone, red in a full run.
  '/dashboard/public': { kind: 'locator', sel: '.shared-dash-warning' },
  '/dashboard/reports': MARKER,
  '/nodes': MARKER,
  '/nodes/discovery': MARKER,
  '/nodes/profiles': MARKER,
  '/nodes/classification-rules': MARKER,
  '/nodes/reclassify': MARKER,
  '/nodes/duplicates': MARKER,
  '/nodes/subnet-overlaps': MARKER,
  '/nodes/missing-prefixes': MARKER,
  '/nodes/collection-templates': MARKER,
  '/nodes/mib': MARKER,
  // An SVG box has no text a query reaches reliably (labels are cut to fit), so the box itself.
  '/topology/map': { kind: 'locator', sel: '.topomap-node' },
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
  '/settings/relocation': MARKER,
  '/settings/upgrade': MARKER,
  '/nodes/credentials': MARKER,
  '/settings/users': MARKER,
  '/settings/roles': MARKER,
  '/settings/auth': MARKER,
  '/settings/api-tokens': MARKER,
  '/settings/audit': MARKER,
  '/settings/about': MARKER,
  '/nodes/00000000-0000-4000-8000-000000000001': MARKER,
  '/settings/integrations/meraki': MARKER,
  [MERAKI_ORG_SCREEN]: MARKER,
  '/settings/integrations/netbox': MARKER,
  [`/troubleshoot/report/${REPORT_TOOL}`]: MARKER,
  '/login': {
    kind: 'none',
    why: 'The sign-in form, reached unauthenticated by design. Its behaviour (success / 401 / 429) is Inc.2.',
  },
};
