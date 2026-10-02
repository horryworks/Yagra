// SPDX-License-Identifier: AGPL-3.0-only
// The search on one level of the network map (ADR-191 Inc.11, Inc.13): the condition the caller
// keeps in its URL, what it finds on the drawn level, and the hit Enter last brought to the middle.
// Both the full map and the map in a folder's pane read it through here, so the two cannot judge a
// hit or step through them differently; only the URL key differs (`?q=` / `?mq=`).

import { useCallback, useMemo, useState } from 'react';
import { decodeCondition, encodeCondition, type TextCondition } from '../../lib/filterCondition';
import type { MapLevel } from '../../types/api';
import type { GraphLayout } from './graphLayout';
import { searchMap, stepThrough, type MapSearch } from './mapSearch';

export interface MapSearchState {
  cond: TextCondition;
  setCond: (next: TextCondition) => void;
  search: MapSearch;
  /** The hit to centre; `seq` changes on every step so the same hit can be centred again. */
  focus: { id: string; seq: number } | null;
  /** Step to the next (1) or previous (-1) hit of `term` — the box's text at the moment of the
   *  key press, which may not have reached the URL yet. */
  step: (dir: 1 | -1, term: string) => void;
}

export function useMapSearch(
  level: MapLevel | null,
  layout: GraphLayout,
  encoded: string,
  write: (encoded: string) => void,
): MapSearchState {
  const cond = useMemo(() => decodeCondition(encoded), [encoded]);
  const setCond = useCallback((next: TextCondition) => write(encodeCondition(next)), [write]);
  const search = useMemo(() => searchMap(level, layout, cond), [level, layout, cond]);
  const [focus, setFocus] = useState<{ id: string; seq: number } | null>(null);
  // The hits are worked out from the box's text when Enter is pressed, not from the last render:
  // the Enter that commits a freshly typed term arrives before the URL has the term, and stepping
  // through the previous search's hits would centre the wrong box.
  const step = useCallback(
    (dir: 1 | -1, term: string) => {
      const order = searchMap(level, layout, { ...cond, term }).order;
      setFocus((cur) => {
        const id = stepThrough(order, cur?.id ?? null, dir);
        return id ? { id, seq: (cur?.seq ?? 0) + 1 } : cur;
      });
    },
    [level, layout, cond],
  );
  return { cond, setCond, search, focus, step };
}
