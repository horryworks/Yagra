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
    expect(sectionForPath('/nodes/credentials').key).toBe('nodes');
    expect(sectionForPath('/events/webhooks').key).toBe('events');
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

  it('resolves every address ADR-055 vacated, so nothing lights a wrong tab in transit', () => {
    // These four redirect (`MovedTo` in routes.tsx). They still have to resolve to *a* section:
    // `sectionForPath` falls back to NAV[0] otherwise, so an old bookmark would flash Dashboard
    // before landing. The old owner is the right answer here — it is where the URL still says it is.
    expect(sectionForPath('/alerts/events').key).toBe('alerts');
    expect(sectionForPath('/alerts/event-sources').key).toBe('alerts');
    expect(sectionForPath('/settings/forwarding').key).toBe('settings');
    expect(sectionForPath('/settings/credentials').key).toBe('settings');
  });

  it('gives passive monitoring one tab, and puts every one of its screens under it', () => {
    // ADR-055 決定 2. The reason the URLs moved at all: `sectionForPath` matches on
    // `'/' + section.key`, so a screen left at `/alerts/events` could not light this tab.
    const events = NAV.find((s) => s.key === 'events')!;
    expect(sectionItems(events).map((i) => i.path)).toEqual([
      '/events',
      '/events/webhooks',
      '/events/forwarding',
    ]);
    for (const item of sectionItems(events)) {
      expect(sectionForPath(item.path).key).toBe('events');
    }
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

  it('orders Nodes ▸ Monitoring setup in the order the work is done', () => {
    // ADR-055 R7. Not cosmetic: it ran profiles-first, and the profiles entry's own description
    // says "Attach Metric sets here" — two items further down. A menu that tells the reader to
    // start at the end is the failure this pins against, so re-sorting it is a deliberate act.
    const nodes = NAV.find((s) => s.key === 'nodes')!;
    const setup = nodes.groups.find((g) => g.labelKey === 'groups.monitoringConfig')!;
    expect(setup.items.map((i) => i.path)).toEqual([
      '/nodes/credentials',
      '/nodes/mib',
      '/nodes/collection-templates',
      '/nodes/profiles',
      '/nodes/classification-rules',
    ]);
  });

  it('files About under System, and leaves Personal holding only what affects one account', () => {
    // ADR-055 決定 6. About describes the deployment; Personal means "affects only me". Folding
    // Preferences into System instead would make a theme switch look fleet-wide.
    const settings = NAV.find((s) => s.key === 'settings')!;
    const system = settings.groups.find((g) => g.labelKey === 'groups.system')!;
    const personal = settings.groups.find((g) => g.labelKey === 'groups.personal')!;
    expect(system.items[system.items.length - 1].path).toBe('/settings/about');
    expect(personal.items.map((i) => i.path)).toEqual(['/settings/preferences']);
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

  it('has no placeholders left, and none of the lifted ones came back', () => {
    // Every IA entry now has a real screen. The grouping mechanism above is still tested — this
    // asserts the *current* state, so re-adding a placeholder is a deliberate act that shows up
    // here rather than a quiet regression.
    //
    // Lifting one has two halves — `implemented: true` in `nav.ts` and the real element in
    // `routes.tsx` — and they have to happen together: doing only the second leaves a working page
    // rendered inside a greyed-out "Coming soon" group, with nothing failing.
    const soonPaths = NAV.flatMap((s) => sidebarGroups(s))
      .filter((g) => g.comingSoon)
      .flatMap((g) => g.items.map((i) => i.path));
    expect(soonPaths).toEqual([]);
    expect(NAV.flatMap(sectionItems).filter((i) => !i.implemented)).toEqual([]);
  });
});
