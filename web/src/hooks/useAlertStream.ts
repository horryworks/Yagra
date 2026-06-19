// Seed active alerts once, then keep them live via SSE (ADR-019). Centralized so the
// dashboard and the Active alerts screen share one subscription path and the same store.

import { useEffect } from 'react';
import { api } from '../services/api';
import { subscribeAlerts } from '../services/sse';
import { useAlertStore } from '../store';

export function useAlertStream(): void {
  const upsertAlert = useAlertStore((s) => s.upsertAlert);
  const resolveAlert = useAlertStore((s) => s.resolveAlert);

  useEffect(() => {
    api
      .listAlerts()
      .then((list) => list.forEach(upsertAlert))
      .catch(() => {
        /* transient / gated — live SSE still delivers events */
      });
    return subscribeAlerts(upsertAlert, (a) => resolveAlert(a));
  }, [upsertAlert, resolveAlert]);
}
