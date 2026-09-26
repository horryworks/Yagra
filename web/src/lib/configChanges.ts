// SPDX-License-Identifier: AGPL-3.0-only
// The configuration change feed, followed once for the whole app (ADR-019 増分 2).
//
// The server sends a revision number on connect and again whenever inventory or configuration
// changes. This turns it into one counter, `changes`, that a screen reads to decide when to re-read
// what it shows. Screens compare the counter, never the revision: the revision is the server's
// (process-local, restarts from 0), the counter is ours and only ever goes up.
//
// A folder-scoped account is refused the stream (403) and the counter simply never moves for it —
// its screens stay as they were before this existed, re-read on reload.

import { useEffect, useRef } from 'react';
import { create } from 'zustand';
import { subscribeConfigChanges } from '../services/sse';

export interface ConfigChangeState {
  /** The last revision heard, or null before the first frame. */
  revision: number | null;
  /** How many times the revision has changed since the app started. */
  changes: number;
}

/** What one frame does to the state. The first revision is only remembered — the screens that are
 *  open have just read everything. After that, any DIFFERENT revision is a change: a reconnect that
 *  finds the same number did not miss anything, and one that finds another number (including a
 *  restarted or failed-over core, whose count restarted) did. */
export function nextConfigState(s: ConfigChangeState, revision: number): ConfigChangeState {
  if (s.revision === null) return { revision, changes: s.changes };
  if (revision === s.revision) return s;
  return { revision, changes: s.changes + 1 };
}

export const useConfigChangeStore = create<ConfigChangeState>(() => ({
  revision: null,
  changes: 0,
}));

/** Follow the change feed. Mounted once, by `AppShell`. */
export function useConfigChangeStream(): void {
  useEffect(
    () =>
      subscribeConfigChanges((revision) =>
        useConfigChangeStore.setState((s) => nextConfigState(s, revision)),
      ),
    [],
  );
}

/** How many configuration changes this tab has heard of. Put it in a load effect's deps to re-read
 *  on each one; its value at mount is not a change. */
export function useConfigChanges(): number {
  return useConfigChangeStore((s) => s.changes);
}

/** Run `onChange` each time the configuration changes after this component mounted — never for
 *  changes heard before it (the mount reads everything anyway), and never twice for one change
 *  because the callback's identity moved. The same "seen" discipline as the node-state resyncs. */
export function useOnConfigChange(onChange: () => void): void {
  const changes = useConfigChanges();
  const seen = useRef(changes);
  const latest = useRef(onChange);
  useEffect(() => {
    latest.current = onChange;
  }, [onChange]);
  useEffect(() => {
    if (changes === seen.current) return;
    seen.current = changes;
    latest.current();
  }, [changes]);
}
