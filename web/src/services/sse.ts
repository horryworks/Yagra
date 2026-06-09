// Live updates over Server-Sent Events (ADR-019). The northbound API exposes per-concern
// streams (e.g. `/api/v1/stream/alerts`); this wraps EventSource and resumes from the last
// event id. The event *parsing* is split out as a pure function so it can be tested without
// a browser EventSource.

import type { Alert } from '../types/api';

const BASE = (import.meta.env.VITE_API_BASE as string | undefined) ?? '/api/v1';

/** Parse one SSE `data:` payload into an Alert, or return null if malformed. */
export function parseAlertEvent(data: string): Alert | null {
  try {
    const obj = JSON.parse(data) as Partial<Alert>;
    if (typeof obj.node === 'string' && typeof obj.severity === 'string') {
      return obj as Alert;
    }
    return null;
  } catch {
    return null;
  }
}

/** Subscribe to the alert stream. Returns an unsubscribe function. */
export function subscribeAlerts(
  onAlert: (alert: Alert) => void,
  onError?: (err: Event) => void,
): () => void {
  const source = new EventSource(`${BASE}/stream/alerts`);
  source.onmessage = (ev: MessageEvent<string>) => {
    const alert = parseAlertEvent(ev.data);
    if (alert) onAlert(alert);
  };
  if (onError) source.onerror = onError;
  return () => source.close();
}
