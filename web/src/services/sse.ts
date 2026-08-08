// SPDX-License-Identifier: AGPL-3.0-only
// Live updates over Server-Sent Events (ADR-019). The northbound API exposes per-concern
// streams (e.g. `/api/v1/stream/alerts`); this consumes them over `fetch` rather than the native
// `EventSource`. Why: `EventSource` cannot attach request headers, so it can't send the
// `Authorization: Bearer <token>` the API requires — on an auth-enabled deployment
// (`YAGRA_PUBLIC_DASHBOARD=false`) a native EventSource stream is rejected 401 and no events ever
// arrive (the live progress bar / alert feed silently freezes). A `fetch` reader carries the
// token and streams the response body. The event *parsing* stays split out as pure functions so
// it can be tested without a network stream.

import type { Alert, AnalysisJob, NodeState, ReportRun } from '../types/api';
import { isNodeState } from '../lib/nodeState';
import { isRunState } from '../reports/runStatus';
import { getToken, notifyAuthFailure } from './api';

// Origin prefix, empty by default — the same meaning `services/api.ts` gives it, with `/api/v1`
// spelled at the call site. It used to default to `/api/v1` and be used as a path prefix here while
// the API client treated it as a base, so setting it pointed the two at different URLs.
const ORIGIN = (import.meta.env.VITE_API_BASE as string | undefined) ?? '';
/** Backoff before reconnecting after a stream ends or errors (mirrors EventSource's retry). */
const RECONNECT_MS = 3000;

/** An alert event off the wire: an alert plus a `resolved` flag (fire vs recovery). */
export type AlertEvent = Alert & { resolved?: boolean };

/** Parse one SSE `data:` payload into an alert event, or return null if malformed.
 *
 *  `node` carries the alert's *subject* — a node UUID, or `pool:<name>` — and is a string for every
 *  subject, which is what keeps this validity gate correct. `subject_kind` says which; a frame
 *  without it came from a core older than the subject split, when every alert was about a node, so
 *  it is normalized here rather than left for each consumer to default. */
export function parseAlertEvent(data: string): AlertEvent | null {
  try {
    const obj = JSON.parse(data) as Partial<AlertEvent>;
    if (typeof obj.node === 'string' && typeof obj.severity === 'string') {
      return { ...obj, subject_kind: obj.subject_kind ?? 'node' } as AlertEvent;
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

/** An incremental node-state change off the wire (S14): a node's new rolled-up display state. */
export interface NodeStateEvent {
  node_id: string;
  state: NodeState;
  at_unix_ms: number;
}

/** Parse one node-state SSE payload, or null if malformed (also rejects the `resync` hint, whose
 *  `data` is a bare number). */
export function parseNodeStateEvent(data: string): NodeStateEvent | null {
  try {
    const obj = JSON.parse(data) as Partial<NodeStateEvent>;
    if (
      typeof obj.node_id === 'string' &&
      typeof obj.state === 'string' &&
      isNodeState(obj.state)
    ) {
      return obj as NodeStateEvent;
    }
    return null;
  } catch {
    return null;
  }
}

/** Parse one report-run SSE payload, or null if malformed. Narrows `state` against the real set
 *  rather than accepting any string, matching `parseNodeStateEvent` above — a payload carrying a
 *  state this build cannot render would otherwise reach a `Record` lookup and read as undefined. */
export function parseReportRun(data: string): ReportRun | null {
  try {
    const obj = JSON.parse(data) as Partial<ReportRun>;
    if (typeof obj.id === 'string' && isRunState(obj.state)) {
      return obj as ReportRun;
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
        const res = await fetch(`${ORIGIN}${path}`, {
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
    '/api/v1/stream/alerts',
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
 * Subscribe to the node-state stream (S14): each event is one node's new rolled-up display state.
 * The inventory/topology views seed from REST once and then patch individual nodes live off this
 * stream instead of re-fetching the whole fleet every 15s. Returns an unsubscribe function.
 */
export function subscribeNodeStates(
  onState: (ev: NodeStateEvent) => void,
  onError?: (err: unknown) => void,
): () => void {
  return subscribeSSE(
    '/api/v1/stream/node-states',
    (data) => {
      const ev = parseNodeStateEvent(data);
      if (ev) onState(ev);
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
    '/api/v1/stream/analysis',
    (data) => {
      const job = parseAnalysisJob(data);
      if (job) onJob(job);
    },
    onError,
  );
}

/**
 * Subscribe to the report-run status stream. Each event is a run's current row; `onRun` upserts
 * it into the runs store so generation progress shows live. Returns an unsubscribe function.
 */
export function subscribeReportRuns(
  onRun: (run: ReportRun) => void,
  onError?: (err: unknown) => void,
): () => void {
  return subscribeSSE(
    '/api/v1/stream/report-runs',
    (data) => {
      const run = parseReportRun(data);
      if (run) onRun(run);
    },
    onError,
  );
}
