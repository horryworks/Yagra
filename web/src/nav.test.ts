// SPDX-License-Identifier: AGPL-3.0-only
import { describe, expect, it } from 'vitest';
import { NAV, sectionForPath, sectionItems, sidebarGroups } from './nav';
import enNav from './locales/en/nav.json';

/** Resolve a dotted i18n key (e.g. 'nodes.metricSets') against the English nav bundle. */
function resolveKey(key: string): unknown {
  return key.split('.').reduce<unknown>((acc, part) => {
    if (acc && typeof acc === 'object') return (acc as Record<string, unknown>)[part];
    return undefined;
  }, enNav);
}

describe('nav IA', () => {
  it('maps each top-level path to its own section', () => {
    expect(sectionForPath('/dashboard').key).toBe('dashboard');
    expect(sectionForPath('/nodes').key).toBe('nodes');
    expect(sectionForPath('/alerts').key).toBe('alerts');
  });

  it('keeps sub-feature and drill-down paths within their section', () => {
    expect(sectionForPath('/nodes/profiles').key).toBe('nodes');
    expect(sectionForPath('/nodes/abc-123').key).toBe('nodes'); // node detail
    expect(sectionForPath('/alerts/rules').key).toBe('alerts');
    expect(sectionForPath('/settings/credentials').key).toBe('settings');
    expect(sectionForPath('/topology/map').key).toBe('topology');
    expect(sectionForPath('/dashboard/reports').key).toBe('dashboard'); // Reports moved here from Metrics
  });

  it('keeps the Integrations detail path within Settings', () => {
    expect(sectionForPath('/settings/integrations').key).toBe('settings');
    expect(sectionForPath('/settings/integrations/meraki').key).toBe('settings');
  });

  it('keeps the (redirected) nodes/dependencies path within Nodes', () => {
    // The route redirects to /topology/dependency, but the path must still resolve to a section
    // so nothing off-nav lights the wrong tab in transit.
    expect(sectionForPath('/nodes/dependencies').key).toBe('nodes');
  });

  it('falls back to the first section for unknown paths', () => {
    expect(sectionForPath('/nope').key).toBe(NAV[0].key);
  });

  it('every nav item has a unique absolute path', () => {
    const paths = NAV.flatMap((s) => sectionItems(s).map((i) => i.path));
    expect(new Set(paths).size).toBe(paths.length);
    expect(paths.every((p) => p.startsWith('/'))).toBe(true);
  });

  it('each section lands on a path that belongs to that section', () => {
    for (const s of NAV) {
      expect(sectionForPath(s.path).key).toBe(s.key);
    }
  });

  it('Settings lands on System health (the first sidebar item)', () => {
    expect(NAV.find((s) => s.key === 'settings')?.path).toBe('/settings/system-health');
  });

  it('Settings ▸ System health is backed by a real page', () => {
    const health = sectionItems(NAV.find((s) => s.key === 'settings')!).find(
      (i) => i.path === '/settings/system-health',
    );
    expect(health?.implemented).toBe(true);
  });

  it('Settings ▸ Pollers is backed by a real page (distributed poller fleet)', () => {
    const pollers = sectionItems(NAV.find((s) => s.key === 'settings')!).find(
      (i) => i.path === '/settings/pollers',
    );
    expect(pollers?.implemented).toBe(true);
  });

  it('the Nodes ▸ Dependencies placeholder was removed (now a Topology redirect)', () => {
    const nodes = NAV.find((s) => s.key === 'nodes')!;
    expect(sectionItems(nodes).some((i) => i.path === '/nodes/dependencies')).toBe(false);
  });
});

describe('nav i18n keys resolve', () => {
  it('every section, group, item, and description key exists in the English bundle', () => {
    const missing: string[] = [];
    for (const s of NAV) {
      if (resolveKey(s.labelKey) === undefined) missing.push(s.labelKey);
      for (const g of s.groups) {
        if (g.labelKey && resolveKey(g.labelKey) === undefined) missing.push(g.labelKey);
        for (const item of g.items) {
          if (resolveKey(item.labelKey) === undefined) missing.push(item.labelKey);
          if (item.descKey && resolveKey(item.descKey) === undefined) missing.push(item.descKey);
        }
      }
    }
    // The SideBar synthesizes this group header, so it must resolve too.
    if (resolveKey('groups.comingSoon') === undefined) missing.push('groups.comingSoon');
    expect(missing).toEqual([]);
  });
});

describe('sidebarGroups', () => {
  it('routes every unimplemented item into a single trailing coming-soon group, losing none', () => {
    for (const s of NAV) {
      const groups = sidebarGroups(s);
      const soonGroups = groups.filter((g) => g.comingSoon);
      expect(soonGroups.length).toBeLessThanOrEqual(1);

      const allItems = sectionItems(s);
      const rendered = groups.flatMap((g) => g.items);
      // No item is lost or duplicated.
      expect(rendered.map((i) => i.path).sort()).toEqual(allItems.map((i) => i.path).sort());

      // Working groups hold only implemented items; the coming-soon group holds only placeholders.
      for (const g of groups) {
        if (g.comingSoon) expect(g.items.every((i) => !i.implemented)).toBe(true);
        else expect(g.items.every((i) => i.implemented)).toBe(true);
      }

      // The coming-soon group, when present, is last.
      if (soonGroups.length === 1) expect(groups[groups.length - 1].comingSoon).toBe(true);
    }
  });

  it('collects the known placeholders (Geo map, Scheduled, Saved findings)', () => {
    const soonPaths = NAV.flatMap((s) => sidebarGroups(s))
      .filter((g) => g.comingSoon)
      .flatMap((g) => g.items.map((i) => i.path));
    for (const p of ['/topology/geo', '/troubleshoot/scheduled', '/troubleshoot/findings']) {
      expect(soonPaths).toContain(p);
    }
    // Authentication (/settings/auth) is now implemented — no longer a placeholder.
    expect(soonPaths).not.toContain('/settings/auth');
  });
});
