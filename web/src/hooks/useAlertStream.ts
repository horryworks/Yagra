// SPDX-License-Identifier: AGPL-3.0-only
// Seed active alerts once, then keep them live via SSE (ADR-019). Centralized so the
// dashboard and the Active alerts screen share one subscription path and the same store.

import { useEffect } from 'react';
import { api } from '../services/api';
import { subscribeAlerts } from '../services/sse';
import { useAlertStore } from '../store';

/** `enabled` is for the public board, which must not subscribe to a stream its widgets have not
 *  opened (`boardReadsAlerts`). Every other caller is signed in and leaves it on. */
export function useAlertStream(enabled = true): void {
  const upsertAlert = useAlertStore((s) => s.upsertAlert);
  const resolveAlert = useAlertStore((s) => s.resolveAlert);

  useEffect(() => {
    if (!enabled) return undefined;
    api
      .listAlerts()
      .then((list) => list.forEach(upsertAlert))
      .catch(() => {
        /* transient / gated — live SSE still delivers events */
      });
    return subscribeAlerts(upsertAlert, (a) => resolveAlert(a));
  }, [enabled, upsertAlert, resolveAlert]);
}
