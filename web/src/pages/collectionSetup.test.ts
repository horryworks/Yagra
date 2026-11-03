// SPDX-License-Identifier: AGPL-3.0-only
import { describe, expect, it } from 'vitest';
import {
  COLLECTION_MOVED_FROM,
  COLLECTION_TABS,
  collectionTabOf,
  collectionTabPath,
  NEW_PROFILE_HREF,
} from './collectionSetup';
import { navItemForPath, rememberableRoute } from '../nav';

describe('collection setup tabs (ADR-231)', () => {
  it('reads the tab from the path, and lands the hub on profiles', () => {
    expect(collectionTabOf('/nodes/collection/rules')).toBe('rules');
    expect(collectionTabOf('/nodes/collection/metric-sets')).toBe('metric-sets');
    expect(collectionTabOf('/nodes/collection')).toBe('profiles');
    expect(collectionTabPath('profiles')).toBe('/nodes/collection');
    expect(collectionTabOf('/nodes/collection/nonsense')).toBe('profiles');
  });

  it('every tab is the one menu item, and is remembered as the tab', () => {
    for (const tab of COLLECTION_TABS) {
      const path = collectionTabPath(tab);
      expect(navItemForPath(path)?.item.path).toBe('/nodes/collection');
      expect(rememberableRoute(path, '?q=x')?.route).toBe(`${path}?q=x`);
    }
  });

  it('every old path moved somewhere distinct, and none of them is a menu item any more', () => {
    const olds = Object.values(COLLECTION_MOVED_FROM);
    expect(new Set(olds).size).toBe(COLLECTION_TABS.length);
    for (const old of olds) expect(navItemForPath(old)).toBeNull();
  });

  it('the new-profile link opens the profiles tab with its dialog', () => {
    expect(NEW_PROFILE_HREF).toBe('/nodes/collection?add=1');
  });
});
