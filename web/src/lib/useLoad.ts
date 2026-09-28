// SPDX-License-Identifier: AGPL-3.0-only
// A list screen's read, once (ADR-184 increment 24).
//
// Twenty screens held the same fifteen lines: `rows`, `block`, `loading`, a `load` callback whose
// `.catch` asked `classifyLoadError`, an effect to run it, and — on some — a config-change
// subscription or a timer. The copies had drifted the way copies do: most applied whatever answer
// arrived last rather than the one asked for last, some emptied a side list on a failed re-read,
// and the two timers kept polling in a hidden tab. What stays on the screen is what is genuinely
// per-screen: which call to make, and what to draw.
//
// The judgement is `loadReducer` in `loadState.ts`, where a node-environment test runs; this file
// is the wiring, tested under jsdom in `useLoad.test.ts`.
import { useCallback, useEffect, useMemo, useReducer, useRef } from 'react';
import { useOnConfigChange } from './configChanges';
import {
  initialLoadState,
  loadReducer,
  type LoadEvent,
  type LoadMachine,
  type LoadState,
} from './loadState';
import { pollWhileVisible } from './sharedPoll';

export interface LoadOptions<T> {
  /** `data` before the first answer. */
  initial: T;
  /** False ⇒ no request, and `loading` settles. For a read that waits on something else. */
  enabled?: boolean;
  /** Re-read whenever anyone changes the configuration (`lib/configChanges.ts`). */
  onConfigChange?: boolean;
  /** Re-read on this cadence while the tab is visible (`pollWhileVisible`). */
  intervalMs?: number;
  /** Turn a failure that is not a block into `error` text, with this as the fallback message. */
  errorFallback?: string;
}

export interface Loaded<T> extends LoadState<T> {
  /** Read again now — after a write, say. The answer replaces the rows in place. */
  reload: () => void;
}

/**
 * Read `fetcher` on mount and whenever `deps` change.
 *
 * CONTRACT — the one `usePolled` states: every value the fetcher closes over belongs in `deps`, or
 * the read keeps asking with the first render's value.
 *
 * `data` keeps its identity until an answer replaces it, so a memo keyed on it (`useClientFilters`
 * is one) does not recompute on every render.
 */
export function useLoad<T>(
  fetcher: () => Promise<T>,
  deps: readonly unknown[],
  opts: LoadOptions<T>,
): Loaded<T> {
  const [state, dispatch] = useReducer(
    (s: LoadMachine<T>, e: LoadEvent<T>) => loadReducer(s, e),
    opts.initial,
    initialLoadState,
  );
  const seq = useRef(0);
  const latest = useRef({ fetcher, fallback: opts.errorFallback });
  useEffect(() => {
    latest.current = { fetcher, fallback: opts.errorFallback };
  });

  const reload = useCallback(() => {
    const mine = ++seq.current;
    latest.current.fetcher().then(
      (data) => dispatch({ type: 'loaded', seq: mine, data }),
      (error: unknown) =>
        dispatch({ type: 'failed', seq: mine, error, fallback: latest.current.fallback }),
    );
  }, []);

  const enabled = opts.enabled ?? true;
  useEffect(() => {
    if (!enabled) {
      dispatch({ type: 'skipped', seq: ++seq.current });
      return;
    }
    reload();
    // `deps` is the caller's list, spread in on purpose — the contract above.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [enabled, reload, ...deps]);

  const onConfig = !!opts.onConfigChange && enabled;
  useOnConfigChange(
    useCallback(() => {
      if (onConfig) reload();
    }, [onConfig, reload]),
  );

  const intervalMs = enabled ? opts.intervalMs : undefined;
  useEffect(
    () => (intervalMs ? pollWhileVisible(reload, intervalMs) : undefined),
    [intervalMs, reload],
  );

  const { data, block, error, loading } = state;
  return useMemo(
    () => ({ data, block, error, loading, reload }),
    [data, block, error, loading, reload],
  );
}
