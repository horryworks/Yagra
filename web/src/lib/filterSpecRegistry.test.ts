// SPDX-License-Identifier: AGPL-3.0-only
// Every column-filter spec in the app, in one place, so `reservedKeyCollisions` runs against all of
// them (ADR-053 Inc.10 決定 Y).
//
// **Why this exists rather than one assert per screen.** `columnFilter.ts` says the column key IS
// the URL key — no prefix — because the screens spell `severity`, `state` and `q` bare and a prefix
// would break every bookmark taken before ADR-053 shipped. The cost of that choice is
// `RESERVED_URL_KEYS`, and the cost was being paid by eight screens out of twenty-five: the other
// seventeen had no collision check at all. A key like `sort` or `before` on one of those would not
// fail to compile, would not throw, and would not look wrong — the page's own pagination and the
// operator's filter would simply take turns writing the same query parameter.
//
// So the check moved to where the set is: a screen is covered by being *listed*, and the second
// test below makes forgetting to list one a failure rather than a silence.
//
// ⚠️ The stubs are deliberately empty — `[]` for every option list, `() => ''` for every label
// resolver. This test reads **keys**, never values, and a stub that tried to look realistic would
// be a second, worse copy of each screen's own test data.

import { readdirSync, readFileSync, statSync } from 'node:fs';
import { join, relative, sep } from 'node:path';
import { describe, expect, it } from 'vitest';
import type { TFunction } from 'i18next';
import {
  RESERVED_URL_KEYS,
  reservedKeyCollisions,
  specColumns,
  type FilterableColumn,
} from './columnFilter';

import { eventFilters } from '../components/EventLog/eventFilterSpec';
import { flowFilters } from '../components/NodeDetail/flowTabFilters';
import { interfaceFilters, metricFilters, neighborFilters } from '../components/NodeDetail/tabFilters';
import { activeAlertFilters } from '../pages/activeAlertFilters';
import { tokenFilters } from '../pages/apiTokenFilters';
import { auditFilters } from '../pages/auditQuery';
import { classificationRuleFilters } from '../pages/classificationFilters';
import { credentialFilters } from '../pages/credentialList';
import { dependencyFilters } from '../pages/dependencyFilters';
import { candidateFilters, endpointFilters } from '../pages/discoveryFilters';
import { eventRuleFilters, eventSourceFilters } from '../pages/eventConfigFilters';
import { forwardingFilters } from '../pages/forwardingListFilters';
import { historyFilters } from '../pages/historyQuery';
import { inventoryFilterSpecs } from '../pages/inventoryFilters';
import {
  metricSetFilters,
  profileCategoryFilter,
  profileFilters,
} from '../pages/monitoringConfigFilters';
import { pollerFilters } from '../pages/pollerFilters';
import { channelFilters, routingRuleFilters } from '../pages/routingFilters';
import { muteFilters, windowFilters } from '../pages/suppressionFilters';
import { thresholdFilters } from '../pages/thresholdQuery';
import { userFilters } from '../pages/userFilters';
import {
  definitionFilters,
  reportScheduleFilters,
  savedRunFilters,
} from '../reports/reportListFilters';
import { findingFilters } from '../troubleshoot/findingsQuery';
import {
  authProbeFilters,
  flowScanFilters,
  ruleGapFilters,
} from '../troubleshoot/report/reportFilters';
import { runFilters } from '../troubleshoot/runFilters';
import { scheduleFilters } from '../troubleshoot/scheduleFilters';
import { nodeTabFilterPrefix } from '../components/NodeDetail/tabs';
import { TREE_SEARCH_KEY } from '../pages/inventoryFilters';
import { CHANNEL_FILTER_PREFIX, ROUTING_RULE_FILTER_PREFIX } from '../pages/routingFilters';
import { CANDIDATE_FILTER_PREFIX, ENDPOINT_FILTER_PREFIX } from '../pages/discoveryFilters';
import { REPORT_TABLE_PREFIX } from '../reports/reportListFilters';

const t = ((k: string) => k) as unknown as TFunction;
const none = () => '';

/** One filter-row builder, and the module it must be found in. */
interface Entry {
  /** Path relative to `src/`, POSIX-spelled. Pinned to disk by the coverage test. */
  module: string;
  /** How the entry reads when it fails — the builder, plus the variant when there is more than one. */
  name: string;
  build: () => FilterableColumn<never>[];
}

const REGISTRY: readonly Entry[] = [
  {
    module: 'components/EventLog/eventFilterSpec.ts',
    name: 'eventFilters',
    build: () => specColumns(eventFilters(t)),
  },
  {
    // The Events log renders without the Source column when it is already scoped to one node, and a
    // conditional column is exactly the kind that reaches production unchecked. Both shapes.
    module: 'components/EventLog/eventFilterSpec.ts',
    name: 'eventFilters (showSource: false)',
    build: () => specColumns(eventFilters(t, { showSource: false })),
  },
  {
    module: 'components/NodeDetail/flowTabFilters.ts',
    name: 'flowFilters',
    build: () => specColumns(flowFilters(t)),
  },
  {
    module: 'components/NodeDetail/tabFilters.ts',
    name: 'interfaceFilters',
    build: () => specColumns(interfaceFilters(t)),
  },
  {
    module: 'components/NodeDetail/tabFilters.ts',
    name: 'neighborFilters',
    build: () => specColumns(neighborFilters(t)),
  },
  {
    module: 'components/NodeDetail/tabFilters.ts',
    name: 'metricFilters',
    build: () => specColumns(metricFilters(t)),
  },
  {
    module: 'pages/activeAlertFilters.ts',
    name: 'activeAlertFilters',
    build: () => specColumns(activeAlertFilters(t, none, [], [])),
  },
  {
    module: 'pages/apiTokenFilters.ts',
    name: 'tokenFilters',
    build: () => specColumns(tokenFilters(t, new Date(0))),
  },
  {
    module: 'pages/auditQuery.ts',
    name: 'auditFilters',
    build: () => specColumns(auditFilters(t)),
  },
  {
    module: 'pages/classificationFilters.ts',
    name: 'classificationRuleFilters',
    build: () => specColumns(classificationRuleFilters(t, none)),
  },
  {
    module: 'pages/credentialList.ts',
    name: 'credentialFilters',
    build: () => specColumns(credentialFilters(t, [], none)),
  },
  {
    module: 'pages/dependencyFilters.ts',
    name: 'dependencyFilters',
    build: () => specColumns(dependencyFilters(t, new Map(), false)),
  },
  {
    // The three comparison columns only exist while a derived graph is being compared.
    module: 'pages/dependencyFilters.ts',
    name: 'dependencyFilters (comparing)',
    build: () => specColumns(dependencyFilters(t, new Map(), true)),
  },
  {
    module: 'pages/discoveryFilters.ts',
    name: 'candidateFilters',
    build: () => specColumns(candidateFilters(t)),
  },
  {
    module: 'pages/discoveryFilters.ts',
    name: 'endpointFilters',
    build: () => specColumns(endpointFilters(t)),
  },
  {
    module: 'pages/eventConfigFilters.ts',
    name: 'eventSourceFilters',
    build: () => specColumns(eventSourceFilters(t, [])),
  },
  {
    module: 'pages/eventConfigFilters.ts',
    name: 'eventRuleFilters',
    build: () => specColumns(eventRuleFilters(t, [], none)),
  },
  {
    module: 'pages/forwardingListFilters.ts',
    name: 'forwardingFilters',
    build: () => specColumns(forwardingFilters(t, [])),
  },
  {
    module: 'pages/historyQuery.ts',
    name: 'historyFilters',
    build: () => specColumns(historyFilters(t)),
  },
  {
    module: 'pages/inventoryFilters.ts',
    name: 'inventoryFilterSpecs',
    build: () => specColumns(inventoryFilterSpecs(t, [])),
  },
  {
    module: 'pages/monitoringConfigFilters.ts',
    name: 'metricSetFilters',
    build: () => specColumns(metricSetFilters(t)),
  },
  {
    module: 'pages/monitoringConfigFilters.ts',
    name: 'profileFilters',
    build: () => specColumns(profileFilters(t)),
  },
  {
    module: 'pages/monitoringConfigFilters.ts',
    name: 'profileCategoryFilter',
    build: () => specColumns(profileCategoryFilter(t, [])),
  },
  {
    module: 'pages/pollerFilters.ts',
    name: 'pollerFilters',
    build: () => specColumns(pollerFilters(t, [])),
  },
  {
    module: 'pages/routingFilters.ts',
    name: 'channelFilters',
    build: () => specColumns(channelFilters(t, [])),
  },
  {
    module: 'pages/routingFilters.ts',
    name: 'routingRuleFilters',
    build: () => specColumns(routingRuleFilters(t, none)),
  },
  {
    module: 'pages/suppressionFilters.ts',
    name: 'muteFilters',
    build: () => specColumns(muteFilters(t, none, 0)),
  },
  {
    module: 'pages/suppressionFilters.ts',
    name: 'windowFilters',
    build: () => specColumns(windowFilters(t, none, 0)),
  },
  {
    module: 'pages/thresholdQuery.ts',
    name: 'thresholdFilters',
    build: () => specColumns(thresholdFilters(t)),
  },
  {
    module: 'pages/userFilters.ts',
    name: 'userFilters',
    build: () => specColumns(userFilters(t)),
  },
  {
    module: 'reports/reportListFilters.ts',
    name: 'definitionFilters',
    build: () => specColumns(definitionFilters(t)),
  },
  {
    module: 'reports/reportListFilters.ts',
    name: 'reportScheduleFilters',
    build: () => specColumns(reportScheduleFilters(t)),
  },
  {
    module: 'reports/reportListFilters.ts',
    name: 'savedRunFilters',
    build: () => specColumns(savedRunFilters(t)),
  },
  {
    module: 'troubleshoot/findingsQuery.ts',
    name: 'findingFilters',
    build: () => specColumns(findingFilters(t)),
  },
  {
    module: 'troubleshoot/report/reportFilters.ts',
    name: 'ruleGapFilters',
    build: () => specColumns(ruleGapFilters(t)),
  },
  {
    module: 'troubleshoot/report/reportFilters.ts',
    name: 'flowScanFilters',
    build: () => specColumns(flowScanFilters(t)),
  },
  {
    module: 'troubleshoot/report/reportFilters.ts',
    name: 'authProbeFilters',
    build: () => specColumns(authProbeFilters(t)),
  },
  {
    module: 'troubleshoot/runFilters.ts',
    name: 'runFilters',
    build: () => specColumns(runFilters(t)),
  },
  {
    module: 'troubleshoot/scheduleFilters.ts',
    name: 'scheduleFilters',
    build: () => specColumns(scheduleFilters(t)),
  },
];

// ---------------------------------------------------------------------------------------------------
// The route ledger (ADR-153 決定 3 / 決定 8).
//
// `reservedKeyCollisions` answers "does this ONE table collide with a page parameter". Since every
// filter lives in the URL, the question that can actually go wrong is wider: **do the tables on one
// route write distinct keys?** `/nodes` carries the inventory tree's `kind` and — once a node is
// selected — the Events tab's `kind` too; Notification delivery has two tables that both have a
// `name` and a `status`. Unprefixed, each pair would filter the other, silently: nothing throws and
// both tables look like they are working.
//
// So each route declares what shares its query string — the page's own keys and every table with its
// prefix — and the tests below check the union is disjoint. ⚠️ The paths are labels for the reader;
// nothing compares them with `routes.tsx`. What IS pinned: every builder in REGISTRY is on a route
// (or waiting below with a reason), every prefix is the constant the screen itself imports, and every
// prefixed table's file spells that constant — so a screen that forgot to pass it fails here.

interface RouteTable {
  /** REGISTRY names. More than one when a table has shapes (a column that comes and goes). */
  entries: readonly string[];
  prefix: string;
  /** Where a prefixed table is wired, and the spelling of its prefix there. Required with a prefix. */
  wiredIn?: { file: string; spelling: string };
}

interface Route {
  path: string;
  /** Keys the page itself writes: a selection, a tab, a search term, a scope. */
  own: readonly string[];
  tables: readonly RouteTable[];
}

/** The node-detail tabs a host renders, each under its tab's prefix. Both hosts render the same set. */
const NODE_TABS: readonly RouteTable[] = [
  {
    entries: ['interfaceFilters'],
    prefix: nodeTabFilterPrefix('interfaces'),
    wiredIn: { file: 'components/NodeDetail/InterfacesTab.tsx', spelling: "nodeTabFilterPrefix('interfaces')" },
  },
  {
    entries: ['neighborFilters'],
    prefix: nodeTabFilterPrefix('neighbors'),
    wiredIn: { file: 'components/NodeDetail/NeighborsTab.tsx', spelling: "nodeTabFilterPrefix('neighbors')" },
  },
  {
    entries: ['metricFilters'],
    prefix: nodeTabFilterPrefix('collection'),
    wiredIn: { file: 'components/NodeDetail/CollectionTab.tsx', spelling: "nodeTabFilterPrefix('collection')" },
  },
  {
    entries: ['eventFilters (showSource: false)'],
    prefix: nodeTabFilterPrefix('events'),
    wiredIn: { file: 'components/NodeDetail/EventsTab.tsx', spelling: "nodeTabFilterPrefix('events')" },
  },
  {
    entries: ['flowFilters'],
    prefix: nodeTabFilterPrefix('flow'),
    wiredIn: { file: 'components/NodeDetail/FlowTab.tsx', spelling: "nodeTabFilterPrefix('flow')" },
  },
];

const ROUTES: readonly Route[] = [
  {
    path: '/nodes',
    own: ['sel', 'tab', TREE_SEARCH_KEY],
    tables: [{ entries: ['inventoryFilterSpecs'], prefix: '' }, ...NODE_TABS],
  },
  { path: '/nodes/:nodeId', own: ['tab'], tables: NODE_TABS },
  { path: '/nodes/credentials', own: [], tables: [{ entries: ['credentialFilters'], prefix: '' }] },
  {
    path: '/nodes/classification-rules',
    own: [],
    tables: [{ entries: ['classificationRuleFilters'], prefix: '' }],
  },
  { path: '/events', own: ['node_id'], tables: [{ entries: ['eventFilters'], prefix: '' }] },
  { path: '/events/webhooks', own: [], tables: [{ entries: ['eventSourceFilters'], prefix: '' }] },
  { path: '/events/forwarding', own: [], tables: [{ entries: ['forwardingFilters'], prefix: '' }] },
  { path: '/alerts', own: [], tables: [{ entries: ['activeAlertFilters'], prefix: '' }] },
  {
    path: '/alerts/history',
    own: ['node_id', 'group_id'],
    tables: [{ entries: ['historyFilters'], prefix: '' }],
  },
  { path: '/alerts/rules', own: [], tables: [{ entries: ['thresholdFilters'], prefix: '' }] },
  {
    path: '/alerts/routing',
    own: [],
    tables: [
      {
        entries: ['channelFilters'],
        prefix: CHANNEL_FILTER_PREFIX,
        wiredIn: { file: 'pages/RoutingPage.tsx', spelling: 'CHANNEL_FILTER_PREFIX' },
      },
      {
        entries: ['routingRuleFilters'],
        prefix: ROUTING_RULE_FILTER_PREFIX,
        wiredIn: { file: 'pages/RoutingPage.tsx', spelling: 'ROUTING_RULE_FILTER_PREFIX' },
      },
    ],
  },
  { path: '/alerts/event-rules', own: [], tables: [{ entries: ['eventRuleFilters'], prefix: '' }] },
  { path: '/alerts/maintenance', own: [], tables: [{ entries: ['windowFilters'], prefix: '' }] },
  { path: '/alerts/mutes', own: [], tables: [{ entries: ['muteFilters'], prefix: '' }] },
  {
    path: '/dashboard/reports',
    own: ['tab'],
    tables: [
      {
        entries: ['savedRunFilters'],
        prefix: REPORT_TABLE_PREFIX.saved,
        wiredIn: { file: 'reports/ReportsPage.tsx', spelling: 'REPORT_TABLE_PREFIX.saved' },
      },
      {
        entries: ['definitionFilters'],
        prefix: REPORT_TABLE_PREFIX.templates,
        wiredIn: { file: 'reports/ReportsPage.tsx', spelling: 'REPORT_TABLE_PREFIX.templates' },
      },
      {
        entries: ['reportScheduleFilters'],
        prefix: REPORT_TABLE_PREFIX.schedules,
        wiredIn: { file: 'reports/ReportsPage.tsx', spelling: 'REPORT_TABLE_PREFIX.schedules' },
      },
    ],
  },
  {
    path: '/topology/dependency',
    own: [],
    tables: [{ entries: ['dependencyFilters', 'dependencyFilters (comparing)'], prefix: '' }],
  },
  { path: '/settings/pollers', own: [], tables: [{ entries: ['pollerFilters'], prefix: '' }] },
  { path: '/settings/api-tokens', own: [], tables: [{ entries: ['tokenFilters'], prefix: '' }] },
  { path: '/troubleshoot/scheduled', own: [], tables: [{ entries: ['scheduleFilters'], prefix: '' }] },
  { path: '/troubleshoot/runs', own: [], tables: [{ entries: ['runFilters'], prefix: '' }] },
  {
    path: '/troubleshoot/findings',
    own: ['node_id', 'group_id'],
    tables: [{ entries: ['findingFilters'], prefix: '' }],
  },
  { path: '/settings/users', own: [], tables: [{ entries: ['userFilters'], prefix: '' }] },
  { path: '/settings/audit', own: [], tables: [{ entries: ['auditFilters'], prefix: '' }] },
  {
    // One table: the category bar and the column cells are one filter state.
    path: '/nodes/profiles',
    own: [],
    tables: [{ entries: ['profileFilters', 'profileCategoryFilter'], prefix: '' }],
  },
  { path: '/nodes/collection-templates', own: [], tables: [{ entries: ['metricSetFilters'], prefix: '' }] },
  {
    path: '/nodes/discovery',
    own: ['scan', 'group'],
    tables: [
      {
        entries: ['candidateFilters'],
        prefix: CANDIDATE_FILTER_PREFIX,
        wiredIn: { file: 'pages/DiscoveryPage.tsx', spelling: 'CANDIDATE_FILTER_PREFIX' },
      },
      {
        entries: ['endpointFilters'],
        prefix: ENDPOINT_FILTER_PREFIX,
        wiredIn: { file: 'pages/DiscoveryPage.tsx', spelling: 'ENDPOINT_FILTER_PREFIX' },
      },
    ],
  },
];

/** Builders whose table does not live in the URL yet, each with the ADR-153 increment that moves it.
 *  ⚠️ This list is meant to reach empty — an entry is a table that still loses its filter on a
 *  reload. It is not an exemption table. */
const NOT_YET_IN_THE_URL: Readonly<Record<string, string>> = {
  ruleGapFilters: 'ADR-153 Inc.5 — Troubleshoot report body',
  flowScanFilters: 'ADR-153 Inc.5 — Troubleshoot report body',
  authProbeFilters: 'ADR-153 Inc.5 — Troubleshoot report body',
};

/** Modules that name `ColumnFilterSpec` but build none — the machinery, not a screen.
 *
 *  Listed rather than pattern-matched on purpose. A new shared helper lands here with a one-line
 *  reason; a new *screen* that lands here instead is a deliberate exemption someone has to write
 *  down, which is the whole point of the coverage test below. */
const NOT_A_SCREEN: readonly string[] = [
  'lib/columnFilter.ts', // the types themselves
  'lib/filterPredicate.ts', // compiles a spec to a row predicate
  'lib/filterSummary.ts', // renders a spec's value as prose
  'lib/useClientFilters.ts', // the client-side state hook
  'components/ui/ColumnFilterCell.tsx', // the control
  'components/ui/ColumnFilterRow.tsx', // the row of controls
  'components/ui/DataTable.tsx', // the table that hosts the row
];

const SRC = join(process.cwd(), 'src');

/** Every `.ts`/`.tsx` under `src/` that is not a test, as a POSIX path relative to `src/`. */
function sourceFiles(dir: string, out: string[] = []): string[] {
  for (const entry of readdirSync(dir, { withFileTypes: true })) {
    const full = join(dir, entry.name);
    if (entry.isDirectory()) sourceFiles(full, out);
    else if (/\.tsx?$/.test(entry.name) && !/\.test\.tsx?$/.test(entry.name)) out.push(full);
  }
  return out;
}

const posix = (full: string) => relative(SRC, full).split(sep).join('/');

describe('the filter spec registry', () => {
  it('has no column key that collides with a page query parameter', () => {
    for (const e of REGISTRY) {
      const cols = e.build();
      // ⚠️ The empty-set check is not padding. `reservedKeyCollisions([])` is `[]`, so a builder
      // whose stub arguments made it return nothing would pass the assertion below while covering
      // no column at all — a check that only ever sees the passing side cannot be told apart from
      // no check.
      expect(cols.length, `${e.name} (${e.module}) built no columns`).toBeGreaterThan(0);
      expect(
        reservedKeyCollisions(cols),
        `${e.name} (${e.module}) — rename the column, or the page's own parameter`,
      ).toEqual([]);
    }
  });

  it('covers every module that builds a spec', () => {
    // The registry is a hand-written list mirroring a directory, so it is pinned to that directory
    // (testing.md). Without this, the collision check silently stops covering the next screen —
    // which is exactly the failure mode it was written to end.
    const declared = new Set([...REGISTRY.map((e) => e.module), ...NOT_A_SCREEN]);
    const mentions = sourceFiles(SRC)
      .filter((f) => readFileSync(f, 'utf8').includes('ColumnFilterSpec'))
      .map(posix);
    const missing = mentions.filter((m) => !declared.has(m)).sort();
    expect(
      missing,
      'add the builder to REGISTRY, or the module to NOT_A_SCREEN with a reason',
    ).toEqual([]);
  });

  it('names only modules that exist', () => {
    // The other direction: a renamed or deleted module leaves a registry line that still compiles
    // (the import moved with it) but whose `module` string now covers nothing.
    for (const e of REGISTRY) {
      expect(statSync(join(SRC, e.module)).isFile(), `${e.module} is not a file`).toBe(true);
    }
  });

  it('is scanning a tree that has specs in it', () => {
    // Cheap insurance against the coverage test passing because it read nothing (a moved `src/`,
    // a changed cwd) — the same guard `testIds.test.ts` carries for the same reason.
    expect(sourceFiles(SRC).length).toBeGreaterThan(100);
    expect(REGISTRY.length).toBeGreaterThan(30);
  });
});

describe('the route ledger (ADR-153)', () => {
  const byName = new Map(REGISTRY.map((e) => [e.name, e]));

  /** Every URL key a route's tables write, with the table that writes it. */
  function filterKeys(route: Route): { key: string; from: string }[] {
    return route.tables.flatMap((table) => {
      const keys = new Set(
        table.entries.flatMap((name) => byName.get(name)?.build().map((c) => c.key) ?? []),
      );
      return [...keys].map((k) => ({ key: table.prefix + k, from: table.entries[0] }));
    });
  }

  it('names only builders that are in the registry', () => {
    for (const route of ROUTES) {
      for (const table of route.tables) {
        for (const name of table.entries) {
          expect(byName.has(name), `${route.path} names ${name}, which REGISTRY does not have`).toBe(true);
        }
      }
    }
    for (const name of Object.keys(NOT_YET_IN_THE_URL)) {
      expect(byName.has(name), `NOT_YET_IN_THE_URL names ${name}, which REGISTRY does not have`).toBe(true);
    }
  });

  it('gives every key on a route exactly one writer', () => {
    // The check this ledger exists for. A key two tables write is two tables filtering each other.
    for (const route of ROUTES) {
      const seen = new Map<string, string>(route.own.map((k) => [k, 'the page itself']));
      for (const { key, from } of filterKeys(route)) {
        expect(
          seen.get(key),
          `${route.path}: ${from} writes "${key}", which ${seen.get(key)} already writes — give one of them a prefix`,
        ).toBeUndefined();
        seen.set(key, from);
      }
    }
  });

  it('puts no filter key on a parameter a page reserves', () => {
    // A prefixed key cannot hit `RESERVED_URL_KEYS`, but a bare one can — and a page's own `tab` or
    // `sort` is reserved precisely so that no column takes it.
    for (const route of ROUTES) {
      const bad = filterKeys(route)
        .map((k) => k.key)
        .filter((k) => (RESERVED_URL_KEYS as readonly string[]).includes(k));
      expect(bad, route.path).toEqual([]);
    }
  });

  it('puts every builder on a route, or names the increment that will', () => {
    const onARoute = new Set(ROUTES.flatMap((r) => r.tables.flatMap((t) => t.entries)));
    const waiting = new Set(Object.keys(NOT_YET_IN_THE_URL));
    for (const e of REGISTRY) {
      expect(
        onARoute.has(e.name) || waiting.has(e.name),
        `${e.name} (${e.module}) is on no route — declare where its table lives`,
      ).toBe(true);
      expect(
        onARoute.has(e.name) && waiting.has(e.name),
        `${e.name} is on a route AND waiting to be moved there — drop it from NOT_YET_IN_THE_URL`,
      ).toBe(false);
    }
  });

  it('finds every prefixed table wired with its own prefix', () => {
    // The ledger's prefixes are the screens' own constants, so they cannot drift in value. What this
    // catches is a screen that stopped passing one — its table would fall back to bare keys and
    // collide with a neighbour, with every test above still green.
    let inspected = 0;
    for (const route of ROUTES) {
      for (const table of route.tables) {
        if (!table.prefix) {
          expect(table.wiredIn, `${route.path}: a bare table needs no wiredIn`).toBeUndefined();
          continue;
        }
        expect(table.wiredIn, `${route.path}: ${table.entries[0]} has a prefix but no wiredIn`).toBeDefined();
        if (!table.wiredIn) continue;
        const text = readFileSync(join(SRC, table.wiredIn.file), 'utf8');
        expect(
          text.includes(table.wiredIn.spelling),
          `${table.wiredIn.file} does not spell ${table.wiredIn.spelling}`,
        ).toBe(true);
        inspected++;
      }
    }
    // A floor on what was inspected, not on what passed: a ledger that stopped declaring prefixes
    // would otherwise pass this by checking nothing.
    expect(inspected).toBeGreaterThanOrEqual(5);
  });
});
