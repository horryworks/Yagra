// SPDX-License-Identifier: AGPL-3.0-only
import { describe, expect, it } from 'vitest';
import { codeOnly, readSources } from '../testSupport/sources';
import type { FilterableColumn, FilterState } from './columnFilter';
import { columnLabels, labelGaps, resultCount, serverToolbarFilters } from './listToolbar';

describe('columnLabels', () => {
  it('names a column by its header when the header is text, by its key otherwise', () => {
    expect(
      columnLabels([
        { key: 'name', header: 'Name' },
        { key: 'state', header: { type: 'span' } },
      ]),
    ).toEqual({ name: 'Name', state: 'state' });
  });
});

describe('labelGaps', () => {
  const cols = [
    { key: 'name', filter: { kind: 'text' } },
    { key: 'state', filter: { kind: 'enum' } },
  ] as unknown as FilterableColumn<unknown>[];

  it('lists the filterable columns left with only their key', () => {
    expect(labelGaps(cols, { name: 'Name', state: 'state' })).toEqual(['state']);
    expect(labelGaps(cols, { name: 'Name' })).toEqual(['state']);
    expect(labelGaps(cols, { name: 'Name', state: 'State' })).toEqual([]);
  });
});

describe('resultCount', () => {
  it('shows "N of M" only while something narrows the list (T2)', () => {
    expect(resultCount({ shown: 12, total: 12, anyFiltered: false }).total).toBeUndefined();
    expect(resultCount({ shown: 3, total: 12, anyFiltered: true }).total).toBe(12);
  });

  it('agrees the noun with the number beside it (T1)', () => {
    // "1 of 5 windows": the noun stands beside the 5.
    expect(resultCount({ shown: 1, total: 5, anyFiltered: true }).nounCount).toBe(5);
    // "1 window": unfiltered, the noun stands beside the 1.
    expect(resultCount({ shown: 1, total: 5, anyFiltered: false }).nounCount).toBe(1);
    // A server list that sends no total counts what it shows.
    expect(resultCount({ shown: 7, anyFiltered: true })).toEqual({
      shown: 7,
      total: undefined,
      nounCount: 7,
    });
  });
});

describe('serverToolbarFilters', () => {
  const cols = [
    { key: 'severity', filter: { kind: 'enum', options: [] } },
  ] as unknown as FilterableColumn<unknown>[];

  function harness(filters: FilterState) {
    const writes: { next: FilterState; also?: (p: URLSearchParams) => void }[] = [];
    const params = {
      filters,
      setFilters: (next: FilterState, also?: (p: URLSearchParams) => void) =>
        writes.push({ next, also }),
    };
    return { params, writes };
  }

  it('clears the columns and the outside control in one write', () => {
    const { params, writes } = harness({ severity: 'critical' });
    let pickerReset = false;
    const clearNode = (p: URLSearchParams) => p.delete('node');
    const list = serverToolbarFilters(cols, params, {
      active: true,
      clear: clearNode,
      onClear: () => (pickerReset = true),
    });
    list.clear();
    // One write — two would each be built from the same snapshot, and the second would restore
    // what the first cleared.
    expect(writes).toHaveLength(1);
    expect(writes[0].next).toEqual({ severity: '' });
    expect(writes[0].also).toBe(clearNode);
    expect(pickerReset).toBe(true);
  });

  it('counts the outside control as narrowing the list', () => {
    const { params } = harness({ severity: '' });
    expect(serverToolbarFilters(cols, params).anyFiltered).toBe(false);
    expect(
      serverToolbarFilters(cols, params, { active: true, clear: () => undefined }).anyFiltered,
    ).toBe(true);
  });
});

/**
 * ADR-184 increment 28: a list's filter button, "clear all filters" and phone sheet are drawn by
 * `ListToolbar` / `FilterControls`, and nowhere else. Thirty-five screens drew them by hand.
 *
 * ⚠️ The needles are assembled at runtime and read from code lines only, so this file and a doc
 * comment naming a component cannot match.
 */
describe('a list draws its filter controls through ListToolbar', () => {
  const NEEDLES = ['MobileFilterSheet', 'FilterButton', 'ClearFilters'].map((n) => `<${n}`);
  const HOME = 'components/ui/ListToolbar.tsx';

  /** Where one of the three is drawn outside a list toolbar, on purpose. Empty for the button and
   *  the sheet: every surface that has them is a list. */
  const PERMANENT: Record<string, { needle: string; why: string }> = {
    'pages/InventoryFilterMenu.tsx': {
      needle: '<ClearFilters',
      why: 'the inventory tree filter is a popover, not a toolbar; its reset sits inside it',
    },
  };

  /** Toolbars still to move. Only ever shorter; deleted when empty (increment 33). */
  const NOT_YET_MIGRATED: string[] = [
    'components/NodeDetail/ApTab.tsx',
    'components/NodeDetail/CollectionTab.tsx',
    'components/NodeDetail/EventsTab.tsx',
    'components/NodeDetail/FlowTab.tsx',
    'components/NodeDetail/InterfacesTab.tsx',
    'components/NodeDetail/NeighborsTab.tsx',
    'pages/ActiveAlertsPage.tsx',
    'pages/ApiTokensPage.tsx',
    'pages/AuditPage.tsx',
    'pages/ClassificationRulesPage.tsx',
    'pages/CollectionTemplatesPage.tsx',
    'pages/CredentialsPage.tsx',
    'pages/DependencyPage.tsx',
    'pages/DiscoveryPage.tsx',
    'pages/EventRulesPage.tsx',
    'pages/EventSourcesPage.tsx',
    'pages/EventsPage.tsx',
    'pages/ForwardingPage.tsx',
    'pages/HistoryPage.tsx',
    'pages/MaintenancePage.tsx',
    'pages/MibRepositoryPage.tsx',
    'pages/MutesPage.tsx',
    'pages/PollersPage.tsx',
    'pages/ProfilesPage.tsx',
    'pages/RoutingPage.tsx',
    'pages/ThresholdsPage.tsx',
    'pages/UsersPage.tsx',
    'pages/integrations/MerakiOrgPage.tsx',
    'reports/ReportsPage.tsx',
    'troubleshoot/AnalysisRuns.tsx',
    'troubleshoot/SavedFindingsPage.tsx',
    'troubleshoot/ScheduledPage.tsx',
    'troubleshoot/report/bodies/AuthProbeBody.tsx',
    'troubleshoot/report/bodies/FlowScanBody.tsx',
    'troubleshoot/report/bodies/RuleGapBody.tsx',
  ];
  /** The ratchet: lowered by each batch, never raised. */
  const CEILING = 35;

  const sources = readSources().map(([p, src]) => [p, codeOnly(src)] as const);
  const drawing = (needle: string) =>
    sources.filter(([, code]) => code.includes(needle)).map(([p]) => p);

  it('no other file draws the filter button, the reset or the sheet', () => {
    const offenders = NEEDLES.flatMap((needle) =>
      drawing(needle)
        .filter((p) => p !== HOME && !NOT_YET_MIGRATED.includes(p))
        .filter((p) => PERMANENT[p]?.needle !== needle)
        .map((p) => `${p} (${needle})`),
    );
    expect(
      offenders,
      `these files draw a list's filter controls by hand. Use ListToolbar, or FilterControls ` +
        `inside a toolbar of the screen's own (components/ui/ListToolbar.tsx):\n  ` +
        offenders.join('\n  '),
    ).toEqual([]);
  });

  it('every listed file still does, so the lists cannot go stale', () => {
    const drawsAny = (p: string) => NEEDLES.some((n) => drawing(n).includes(p));
    const stale = [
      ...NOT_YET_MIGRATED.filter((p) => !drawsAny(p)),
      ...Object.entries(PERMANENT)
        .filter(([p, { needle }]) => !drawing(needle).includes(p))
        .map(([p]) => p),
    ];
    expect(stale, 'listed, but no longer draws them by hand — take it off the list').toEqual([]);
  });

  it('the migration list only shrinks', () => {
    expect(NOT_YET_MIGRATED.length).toBeLessThanOrEqual(CEILING);
    expect(new Set(NOT_YET_MIGRATED).size).toBe(NOT_YET_MIGRATED.length);
  });

  it('finds the lists it is supposed to be reading', () => {
    // Counts toolbars drawn by hand **or** through the shared one, so the number holds while
    // screens move and a walk that found nothing cannot pass.
    const IMPORT = `${'ListToolbar'}'`;
    const lists = sources.filter(
      ([p, code]) =>
        p !== HOME &&
        !PERMANENT[p] &&
        (NEEDLES.some((n) => code.includes(n)) || code.includes(IMPORT)),
    );
    expect(lists.length).toBeGreaterThanOrEqual(35);
  });
});
