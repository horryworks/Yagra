// Live updates over Server-Sent Events (ADR-019). The northbound API exposes per-concern
// streams (e.g. `/api/v1/stream/alerts`); this consumes them over `fetch` rather than the native
// `EventSource`. Why: `EventSource` cannot attach request headers, so it can't send the
// `Authorization: Bearer <token>` the API requires — on an auth-enabled deployment
// (`YAGRA_PUBLIC_DASHBOARD=false`) a native EventSource stream is rejected 401 and no events ever
// arrive (the live progress bar / alert feed silently freezes). A `fetch` reader carries the
// token and streams the response body. The event *parsing* stays split out as pure functions so
// it can be tested without a network stream.

import type { Alert, AnalysisJob } from '../types/api';
import { getToken, notifyAuthFailure } from './api';

const BASE = (import.meta.env.VITE_API_BASE as string | undefined) ?? '/api/v1';
/** Backoff before reconnecting after a stream ends or errors (mirrors EventSource's retry). */
const RECONNECT_MS = 3000;

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

/** Parse one analysis-job SSE payload, or null if malformed. */
export function parseAnalysisJob(data: string): AnalysisJob | null {
  try {
    const obj = JSON.parse(data) as Partial<AnalysisJob>;
    if (typeof obj.id === 'string' && typeof obj.state === 'string') {
      return obj as AnalysisJob;
    }
    return null;
  } catch {
    return null;
  }
}

/**
 * Extract the joined `data:` payload from one SSE event block (its fields separated by newlines),
 * or null when the block carries no data — a keep-alive comment (`:`...) or a control event such
 * as the streams' lagged-subscriber `resync` hint, which our parsers reject anyway.
 */
export function dataFromEventBlock(block: string): string | null {
  const dataLines: string[] = [];
  for (const line of block.split(/\r?\n/)) {
    if (line.startsWith(':')) continue; // comment / keep-alive
    if (line.startsWith('data:')) {
      // Per the SSE spec a single leading space after the colon is stripped.
      dataLines.push(line.slice(5).replace(/^ /, ''));
    }
    // `event:` / `id:` fields are ignored here — both streams carry their payload as `data`.
  }
  return dataLines.length > 0 ? dataLines.join('\n') : null;
}

/** Resolve after `ms`, or immediately when `signal` aborts (so an unsubscribe isn't delayed). */
function delay(ms: number, signal: AbortSignal): Promise<void> {
  return new Promise((resolve) => {
    if (signal.aborted) {
      resolve();
      return;
    }
    const t = setTimeout(resolve, ms);
    signal.addEventListener(
      'abort',
      () => {
        clearTimeout(t);
        resolve();
      },
      { once: true },
    );
  });
}

/**
 * Open an SSE stream over `fetch` (so the bearer token rides on the request) and invoke `onData`
 * with each event's `data` payload. Auto-reconnects after a drop until the returned unsubscribe
 * function is called. A 401 with a token attached drops the stale session (via the api client) and
 * stops, so the app re-prompts for sign-in instead of reconnect-looping against a 401.
 */
function subscribeSSE(
  path: string,
  onData: (data: string) => void,
  onError?: (err: unknown) => void,
): () => void {
  if (typeof fetch === 'undefined') return () => {};
  const controller = new AbortController();
  let closed = false;

  const run = async (): Promise<void> => {
    while (!closed) {
      try {
        const token = getToken();
        const headers: Record<string, string> = { Accept: 'text/event-stream' };
        if (token) headers.Authorization = `Bearer ${token}`;
        const res = await fetch(`${BASE}${path}`, {
          headers,
          cache: 'no-store',
          signal: controller.signal,
        });
        if (!res.ok || !res.body) {
          if (res.status === 401 && token) {
            notifyAuthFailure(); // stale/dropped session — let the app re-prompt; don't retry
            return;
          }
          throw new Error(`stream ${path} failed with status ${res.status}`);
        }
        const reader = res.body.getReader();
        const decoder = new TextDecoder();
        let buffer = '';
        for (;;) {
          const { value, done } = await reader.read();
          if (done) break;
          buffer += decoder.decode(value, { stream: true });
          // Dispatch each complete event (blocks are delimited by a blank line).
          let boundary = buffer.search(/\r?\n\r?\n/);
          while (boundary !== -1) {
            const block = buffer.slice(0, boundary);
            const sep = buffer.slice(boundary).match(/^\r?\n\r?\n/);
            buffer = buffer.slice(boundary + (sep ? sep[0].length : 2));
            const data = dataFromEventBlock(block);
            if (data !== null) onData(data);
            boundary = buffer.search(/\r?\n\r?\n/);
          }
        }
      } catch (err) {
        if (closed) return; // aborted by unsubscribe — not a real error
        onError?.(err);
      }
      if (closed) return;
      await delay(RECONNECT_MS, controller.signal);
    }
  };
  void run();

  return () => {
    closed = true;
    controller.abort();
  };
}

/**
 * Subscribe to the alert stream. Fires call `onAlert`; resolutions call `onResolve`.
 * Returns an unsubscribe function.
 */
export function subscribeAlerts(
  onAlert: (alert: Alert) => void,
  onResolve?: (alert: Alert) => void,
  onError?: (err: unknown) => void,
): () => void {
  return subscribeSSE(
    '/stream/alerts',
    (data) => {
      const event = parseAlertEvent(data);
      if (!event) return;
      if (event.resolved) onResolve?.(event);
      else onAlert(event);
    },
    onError,
  );
}

/**
 * Subscribe to the analysis-job status stream (ADR-022). Each event is a job's current row;
 * `onJob` upserts it into the runs store. Returns an unsubscribe function.
 */
export function subscribeAnalysis(
  onJob: (job: AnalysisJob) => void,
  onError?: (err: unknown) => void,
): () => void {
  return subscribeSSE(
    '/stream/analysis',
    (data) => {
      const job = parseAnalysisJob(data);
      if (job) onJob(job);
    },
    onError,
  );
}
