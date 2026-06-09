// Live updates over Server-Sent Events (ADR-019). The northbound API exposes per-concern
// streams (e.g. `/api/v1/stream/alerts`); this wraps EventSource and resumes from the last
// event id. The event *parsing* is split out as a pure function so it can be tested without
// a browser EventSource.

import type { Alert } from '../types/api';

const BASE = (import.meta.env.VITE_API_BASE as string | undefined) ?? '/api/v1';

/** An alert event off the wire: an alert plus a `resolved` flag (fire vs recovery). */
export type AlertEvent = Alert & { resolved?: boolean };

/** Parse one SSE `data:` payload into an alert event, or return null if malformed. */
export function parseAlertEvent(data: string): AlertEvent | null {
  try {
    const obj = JSON.parse(data) as Partial<AlertEvent>;
    if (typeof obj.node === 'string' && typeof obj.severity === 'string') {
      return obj as AlertEvent;
    }
    return null;
  } catch {
    return null;
  }
}

/**
 * Subscribe to the alert stream. Fires call `onAlert`; resolutions call `onResolve`.
 * Returns an unsubscribe function.
 */
export function subscribeAlerts(
  onAlert: (alert: Alert) => void,
  onResolve?: (alert: Alert) => void,
  onError?: (err: Event) => void,
): () => void {
  const source = new EventSource(`${BASE}/stream/alerts`);
  source.onmessage = (ev: MessageEvent<string>) => {
    const event = parseAlertEvent(ev.data);
    if (!event) return;
    if (event.resolved) onResolve?.(event);
    else onAlert(event);
  };
  if (onError) source.onerror = onError;
  return () => source.close();
}
