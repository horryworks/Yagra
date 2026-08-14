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
import { reservedKeyCollisions, specColumns, type FilterableColumn } from './columnFilter';

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
