import { describe, expect, it } from 'vitest';
import { NAV, sectionForPath } from './nav';

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

  it('falls back to the first section for unknown paths', () => {
    expect(sectionForPath('/nope').key).toBe(NAV[0].key);
  });

  it('every nav item has a unique absolute path', () => {
    const paths = NAV.flatMap((s) => s.items.map((i) => i.path));
    expect(new Set(paths).size).toBe(paths.length);
    expect(paths.every((p) => p.startsWith('/'))).toBe(true);
  });

  it('each section lands on a path that belongs to that section', () => {
    for (const s of NAV) {
      expect(sectionForPath(s.path).key).toBe(s.key);
    }
  });

  it('Settings ▸ System health is backed by a real page', () => {
    const health = NAV.find((s) => s.key === 'settings')?.items.find(
      (i) => i.path === '/settings/system-health',
    );
    expect(health?.implemented).toBe(true);
  });

  it('Settings ▸ Pollers is backed by a real page (distributed poller fleet)', () => {
    const pollers = NAV.find((s) => s.key === 'settings')?.items.find(
      (i) => i.path === '/settings/pollers',
    );
    expect(pollers?.implemented).toBe(true);
  });
});
