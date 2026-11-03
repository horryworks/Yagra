// SPDX-License-Identifier: AGPL-3.0-only
// Nodes ▸ Collection setup: the five screens that shape what is collected, as tabs of one page
// (ADR-231). A tab is a path, not a `?tab=` — three of the five write their filters to the URL under
// the same empty prefix, so one shared query string would let one tab's filter leak into the next.

/** The hub's own path, and the default tab's: a menu item's path has to be a screen of its own
 *  rather than a redirect, so the profiles tab is the hub's bare path. */
export const COLLECTION_SETUP_PATH = '/nodes/collection';

/** In the order one uses the other: a rule picks a profile, a profile attaches metric sets, a set
 *  names MIB objects. Reclassify is last because it is what a rule change leads to. */
export const COLLECTION_TABS = ['rules', 'profiles', 'metric-sets', 'mib', 'reclassify'] as const;
export type CollectionTab = (typeof COLLECTION_TABS)[number];

/** The tab at the bare path: the profiles are what a device is given. */
export const DEFAULT_COLLECTION_TAB: CollectionTab = 'profiles';

/** The tab's label — the screen's old menu name, so a bookmark's title still finds it. */
export const COLLECTION_TAB_LABEL_KEYS: Record<CollectionTab, string> = {
  rules: 'nav:nodes.classificationRules',
  profiles: 'nav:nodes.profiles',
  'metric-sets': 'nav:nodes.metricSets',
  mib: 'nav:nodes.mib',
  reclassify: 'nav:nodes.reclassify',
};

/** Where each screen lived before ADR-231. Each is a `MovedTo`, so a bookmark keeps its query. */
export const COLLECTION_MOVED_FROM: Record<CollectionTab, string> = {
  rules: '/nodes/classification-rules',
  profiles: '/nodes/profiles',
  'metric-sets': '/nodes/collection-templates',
  mib: '/nodes/mib',
  reclassify: '/nodes/reclassify',
};

export function collectionTabPath(tab: CollectionTab): string {
  return tab === DEFAULT_COLLECTION_TAB ? COLLECTION_SETUP_PATH : `${COLLECTION_SETUP_PATH}/${tab}`;
}

/** The tab a route is on, or the default for the hub itself and for anything it does not know. */
export function collectionTabOf(pathname: string): CollectionTab {
  const rest = pathname.startsWith(COLLECTION_SETUP_PATH + '/')
    ? pathname.slice(COLLECTION_SETUP_PATH.length + 1).split('/')[0]
    : '';
  return (COLLECTION_TABS as readonly string[]).includes(rest)
    ? (rest as CollectionTab)
    : DEFAULT_COLLECTION_TAB;
}

/** Opens the new-profile dialog on the profiles tab — offered on a node that fell to the generic
 *  profile (ADR-231 decision 7). */
export const NEW_PROFILE_HREF = `${collectionTabPath('profiles')}?add=1`;
