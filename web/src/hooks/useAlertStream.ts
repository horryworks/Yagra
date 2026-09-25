// SPDX-License-Identifier: AGPL-3.0-only
// Seed active alerts, then keep them live via SSE (ADR-019). Mounted once by `AppShell`, so the
// top-bar bell and every screen that reads `useAlertStore` are live wherever the operator is
// (ADR-019 増分 1); the public board, which sits outside the shell, mounts its own.

import { useEffect } from 'react';
import { api } from '../services/api';
import { subscribeAlerts } from '../services/sse';
import { useAlertStore } from '../store';

/** `enabled` is for the public board, which must not subscribe to a stream its widgets have not
 *  opened (`boardReadsAlerts`). Every other caller is signed in and leaves it on. */
export function useAlertStream(enabled = true): void {
  const upsertAlert = useAlertStore((s) => s.upsertAlert);
  const resolveAlert = useAlertStore((s) => s.resolveAlert);
  const setAlerts = useAlertStore((s) => s.setAlerts);

  useEffect(() => {
    if (!enabled) return undefined;
    // A snapshot REPLACES the store, so an alert resolved while the stream was down drops out.
    // It runs again on every resync (server `resync`, or a reconnect — nothing is replayed).
    // ⚠️ A failed read leaves the store as it is: a transient 403 or a dropped connection must
    // not turn the bell to zero. The live stream still delivers events meanwhile.
    const seed = (): void => {
      api
        .listAlerts()
        .then(setAlerts)
        .catch(() => {
          /* transient / gated — keep what we have */
        });
    };
    seed();
    return subscribeAlerts(upsertAlert, (a) => resolveAlert(a), undefined, seed);
  }, [enabled, upsertAlert, resolveAlert, setAlerts]);
}
