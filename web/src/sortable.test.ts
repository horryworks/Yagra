// SPDX-License-Identifier: AGPL-3.0-only
// ADR-255: a list is put in order by dragging the ⠿ at the left of its rows, through
// `components/ui/Sortable.tsx`, and in no other way. These checks keep the next reorderable list
// from growing ↑ / ↓ buttons again, and keep drag code from spreading past the places that own it.
//
// What each check cannot see is said beside it. A reorder written with plain buttons and strings
// that avoid both spellings below would pass; the rule in `ui-conventions.md` is what binds then.

import { describe, expect, it } from 'vitest';
import { SRC, readSources } from './testSupport/sources';
import { flattenStrings, loadLocales } from './testSupport/locales';

/** Where dnd-kit may be used, and why. A new entry needs a reason a reorderable list does not. */
const DND_ALLOWED: Record<string, string> = {
  'components/ui/Sortable.tsx': 'the shared reorderable list (ADR-255)',
  'components/ui/useSortableSensors.ts': 'the sensors the lists and the board share (decision 3)',
  'dashboard/WidgetFrame.tsx': 'one widget of the 2-D board; its grip is the shared .drag-grip (decision 5)',
  'dashboard/MyDashboardPage.tsx': 'the board the widgets are dragged on (a grid, not a list)',
  'dashboard/SharedDashboardPage.tsx': 'the same board, shared',
  'dashboard/PublicDashboardPage.tsx': 'the same board, public',
  'pages/PollersPage.tsx': 'drags a poller onto another pool: a move, not a reorder',
};

describe('reordering is the left-hand drag grip (ADR-255)', () => {
  const sources = readSources(SRC);

  it('reads the sources it is about', () => {
    // Floor: the walk found the tree, and the shared component in it.
    expect(sources.length).toBeGreaterThan(500);
    expect(sources.some(([p]) => p === 'components/ui/Sortable.tsx')).toBe(true);
  });

  it('no string offers to move something up or down', () => {
    // The ↑ / ↓ buttons each had a "Move X up" / "Move up" label; with them gone, a string of that
    // shape is the sign of a new pair. Checked in EN, which every key has.
    const namespaces = Object.entries(loadLocales());
    expect(namespaces.length).toBeGreaterThan(10);
    const offenders = namespaces.flatMap(([ns, { en }]) =>
      Object.entries(flattenStrings(en))
        .filter(([k, v]) => /(^|\.)move(Up|Down)$/i.test(k) || /^Move\b.*\b(up|down)$/i.test(v.trim()))
        .map(([k]) => `${ns}: ${k}`),
    );
    expect(offenders).toEqual([]);
  });

  it('only the places that own a drag use dnd-kit', () => {
    const users = sources
      .filter(([, text]) => /from '@dnd-kit\/(core|sortable)'/.test(text))
      .map(([p]) => p)
      .sort();
    expect(users).toEqual(Object.keys(DND_ALLOWED).sort());
  });

  it('every reorderable list hands its rows a DragGrip', () => {
    // A `SortableList` with no grip inside would be a list nobody can reorder by pointer.
    const lists = sources.filter(([p, text]) => p !== 'components/ui/Sortable.tsx' && /<SortableList\b/.test(text));
    expect(lists.length).toBeGreaterThanOrEqual(3);
    for (const [p, text] of lists) expect(text, p).toMatch(/<DragGrip\b/);
  });
});
