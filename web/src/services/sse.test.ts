// SPDX-License-Identifier: AGPL-3.0-only
import { afterEach, describe, expect, it, vi } from 'vitest';
import {
  dataFromEventBlock,
  isResyncBlock,
  parseConfigRevision,
  parseAlertEvent,
  parseAnalysisJob,
  parseNodeStateEvent,
  parseReportRun,
  subscribeAlerts,
  subscribeAnalysis,
  subscribeConfigChanges,
  subscribeReportRuns,
} from './sse';
import { setToken } from './api';

describe('parseNodeStateEvent', () => {
  it('parses a well-formed node-state payload', () => {
    const data = JSON.stringify({ node_id: 'n1', state: 'unreachable', at_unix_ms: 1000 });
    const ev = parseNodeStateEvent(data);
    expect(ev?.node_id).toBe('n1');
    expect(ev?.state).toBe('unreachable');
  });

  it('rejects an unknown state value', () => {
    expect(parseNodeStateEvent(JSON.stringify({ node_id: 'n1', state: 'exploded' }))).toBeNull();
  });

  it('rejects the resync hint (a bare number) and malformed JSON', () => {
    expect(parseNodeStateEvent('5')).toBeNull();
    expect(parseNodeStateEvent('{nope')).toBeNull();
  });
});

describe('parseAlertEvent', () => {
  it('parses a well-formed alert payload', () => {
    const data = JSON.stringify({
      node: 'n1',
      check: 'c1',
      severity: 'critical',
      state: 'unreachable',
      at_unix_ms: 1000,
      root_cause: null,
      flapping: false,
    });
    const alert = parseAlertEvent(data);
    expect(alert?.node).toBe('n1');
    expect(alert?.severity).toBe('critical');
  });

  it('returns null on malformed JSON', () => {
    expect(parseAlertEvent('{not json')).toBeNull();
  });

  it('returns null when required fields are missing', () => {
    expect(parseAlertEvent(JSON.stringify({ state: 'ok' }))).toBeNull();
  });

  it('parses an inbound ack event (acked present, no resolved flag)', () => {
    const data = JSON.stringify({
      node: 'n1',
      check: 'c1',
      severity: 'critical',
      state: 'unreachable',
      at_unix_ms: 1000,
      root_cause: null,
      flapping: false,
      acked: { at_unix_ms: 1200, by: 'pd-user', source: 'pagerduty' },
    });
    const event = parseAlertEvent(data);
    expect(event?.acked?.source).toBe('pagerduty');
    // No `resolved` flag ⇒ subscribeAlerts treats it as an upsert, not a recovery.
    expect(event?.resolved).toBeUndefined();
  });
});

describe('isResyncBlock', () => {
  it('recognises the hint the server sends a lagged subscriber', () => {
    // `Event::default().event("resync").data(n)` — api/alerts.rs::sse_with_resync.
    expect(isResyncBlock('event: resync\ndata: 12')).toBe(true);
    expect(isResyncBlock('event:resync\r\ndata: 3')).toBe(true);
  });

  it('is false for an ordinary frame, a keep-alive, and a payload that merely mentions the word', () => {
    expect(isResyncBlock('data: {"node_id":"n1","state":"ok"}')).toBe(false);
    expect(isResyncBlock(': keep-alive')).toBe(false);
    expect(isResyncBlock('data: resync')).toBe(false);
    expect(isResyncBlock('event: resynced\ndata: 1')).toBe(false);
  });
});

describe('dataFromEventBlock', () => {
  it('extracts a single data line (stripping one leading space)', () => {
    expect(dataFromEventBlock('data: {"id":"j1"}')).toBe('{"id":"j1"}');
  });

  it('joins multi-line data fields with newlines', () => {
    expect(dataFromEventBlock('data: line1\ndata: line2')).toBe('line1\nline2');
  });

  it('ignores keep-alive comment blocks', () => {
    expect(dataFromEventBlock(':')).toBeNull();
    expect(dataFromEventBlock(': keep-alive')).toBeNull();
  });

  it('reads the data of a typed (event:) control frame, ignoring the type line', () => {
    expect(dataFromEventBlock('event: resync\ndata: 7')).toBe('7');
  });

  it('returns null for a block with no data field', () => {
    expect(dataFromEventBlock('id: 42')).toBeNull();
  });
});

describe('parseAnalysisJob', () => {
  it('parses a job row with id + state', () => {
    const job = parseAnalysisJob(JSON.stringify({ id: 'j1', state: 'running', pct: 15 }));
    expect(job?.id).toBe('j1');
    expect(job?.state).toBe('running');
  });

  it('returns null when id/state are missing', () => {
    expect(parseAnalysisJob(JSON.stringify({ pct: 50 }))).toBeNull();
  });
});

describe('parseReportRun', () => {
  it('parses a run row with id + state', () => {
    const run = parseReportRun(JSON.stringify({ id: 'r1', state: 'running', pct: 40 }));
    expect(run?.id).toBe('r1');
    expect(run?.state).toBe('running');
  });

  it('returns null when id/state are missing', () => {
    expect(parseReportRun(JSON.stringify({ pct: 10 }))).toBeNull();
    expect(parseReportRun('{not json')).toBeNull();
  });
});

/** A one-shot ReadableStream of UTF-8 SSE text (a fake streaming response body). */
function streamOf(text: string): ReadableStream<Uint8Array> {
  const bytes = new TextEncoder().encode(text);
  return new ReadableStream({
    start(ctrl) {
      ctrl.enqueue(bytes);
      ctrl.close();
    },
  });
}

describe('subscribeAnalysis over fetch', () => {
  afterEach(() => {
    setToken(null);
    vi.restoreAllMocks();
  });

  it('attaches the bearer token and dispatches job events', async () => {
    setToken('tok-123');
    const fetchMock = vi.fn(() =>
      Promise.resolve(
        new Response(streamOf('data: {"id":"j1","state":"done","pct":100}\n\n'), {
          status: 200,
          headers: { 'content-type': 'text/event-stream' },
        }),
      ),
    );
    vi.stubGlobal('fetch', fetchMock);

    const jobs: { id: string; state: string }[] = [];
    const unsubscribe = subscribeAnalysis((j) => jobs.push({ id: j.id, state: j.state }));

    // Let the async reader drain the (already-closed) stream.
    await vi.waitFor(() => expect(jobs.length).toBe(1));
    unsubscribe();

    expect(fetchMock).toHaveBeenCalledWith(
      '/api/v1/stream/analysis',
      expect.objectContaining({ headers: expect.objectContaining({ Authorization: 'Bearer tok-123' }) }),
    );
    expect(jobs[0]).toEqual({ id: 'j1', state: 'done' });
  });

  it('routes an inbound ack event to onAlert (upsert), not onResolve', async () => {
    const fetchMock = vi.fn(() =>
      Promise.resolve(
        new Response(
          streamOf(
            'data: {"node":"n1","check":"c1","severity":"critical","state":"unreachable","at_unix_ms":1,"root_cause":null,"flapping":false,"acked":{"at_unix_ms":2,"by":"pd","source":"pagerduty"}}\n\n',
          ),
          { status: 200, headers: { 'content-type': 'text/event-stream' } },
        ),
      ),
    );
    vi.stubGlobal('fetch', fetchMock);

    const upserts: { acked?: { source: string } | null }[] = [];
    const resolves: unknown[] = [];
    const unsubscribe = subscribeAlerts(
      (a) => upserts.push({ acked: a.acked }),
      (a) => resolves.push(a),
    );

    await vi.waitFor(() => expect(upserts.length).toBe(1));
    unsubscribe();

    expect(resolves).toHaveLength(0);
    expect(upserts[0].acked?.source).toBe('pagerduty');
  });

  it('does not reconnect-loop on a 401 with a token (drops the stale session)', async () => {
    setToken('stale');
    const fetchMock = vi.fn(() =>
      Promise.resolve(new Response('{"error":{"code":"unauthorized"}}', { status: 401 })),
    );
    vi.stubGlobal('fetch', fetchMock);

    const unsubscribe = subscribeAnalysis(() => {});
    await vi.waitFor(() => expect(fetchMock).toHaveBeenCalledTimes(1));
    // Stale token cleared by the shared auth-failure path; the stream stopped (no retry).
    await new Promise((r) => setTimeout(r, 20));
    expect(fetchMock).toHaveBeenCalledTimes(1);
    unsubscribe();
  });
});

// ADR-019 増分 1: the server's `resync` hint (and a reconnect) reached only the node-state stream —
// the other three dropped it, so a lagged alert list stayed stale until a reload. Each must hand it
// to its caller's onResync, and must not dispatch it as data.
describe('the resync hint reaches every stream that asks for it', () => {
  afterEach(() => {
    vi.restoreAllMocks();
  });

  const resyncResponse = () =>
    Promise.resolve(
      new Response(streamOf('event: resync\ndata: 3\n\n'), {
        status: 200,
        headers: { 'content-type': 'text/event-stream' },
      }),
    );

  it('alerts', async () => {
    vi.stubGlobal('fetch', vi.fn(resyncResponse));
    const onAlert = vi.fn();
    const onResync = vi.fn();
    const unsubscribe = subscribeAlerts(onAlert, undefined, undefined, onResync);
    await vi.waitFor(() => expect(onResync).toHaveBeenCalled());
    unsubscribe();
    expect(onAlert).not.toHaveBeenCalled();
  });

  it('analysis', async () => {
    vi.stubGlobal('fetch', vi.fn(resyncResponse));
    const onJob = vi.fn();
    const onResync = vi.fn();
    const unsubscribe = subscribeAnalysis(onJob, undefined, onResync);
    await vi.waitFor(() => expect(onResync).toHaveBeenCalled());
    unsubscribe();
    expect(onJob).not.toHaveBeenCalled();
  });

  it('report runs', async () => {
    vi.stubGlobal('fetch', vi.fn(resyncResponse));
    const onRun = vi.fn();
    const onResync = vi.fn();
    const unsubscribe = subscribeReportRuns(onRun, undefined, onResync);
    await vi.waitFor(() => expect(onResync).toHaveBeenCalled());
    unsubscribe();
    expect(onRun).not.toHaveBeenCalled();
  });
});

describe('parseConfigRevision', () => {
  it('reads the revision off the change feed', () => {
    expect(parseConfigRevision('{"revision":42}')).toBe(42);
    expect(parseConfigRevision('{"revision":0}')).toBe(0);
  });

  it('refuses anything that is not a non-negative integer, rather than comparing it', () => {
    for (const bad of ['{}', '{"revision":"42"}', '{"revision":-1}', '{"revision":1.5}', '12', 'null', 'nope']) {
      expect(parseConfigRevision(bad)).toBeNull();
    }
  });
});

describe('the change feed over fetch (ADR-019 増分 2)', () => {
  afterEach(() => {
    setToken(null);
    vi.restoreAllMocks();
  });

  it('hands each revision to the caller', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn(() =>
        Promise.resolve(
          new Response(streamOf('data: {"revision":7}\n\n'), {
            status: 200,
            headers: { 'content-type': 'text/event-stream' },
          }),
        ),
      ),
    );
    const revs: number[] = [];
    const unsubscribe = subscribeConfigChanges((r) => revs.push(r));
    await vi.waitFor(() => expect(revs).toEqual([7]));
    unsubscribe();
  });

  it('stops on a 403 instead of asking again every few seconds (a folder-scoped account)', async () => {
    setToken('scoped');
    const fetchMock = vi.fn(() =>
      Promise.resolve(
        new Response('{"error":{"code":"scope_unsupported"}}', { status: 403 }),
      ),
    );
    vi.stubGlobal('fetch', fetchMock);
    const onError = vi.fn();
    const unsubscribe = subscribeConfigChanges(() => {}, onError);
    await vi.waitFor(() => expect(onError).toHaveBeenCalledTimes(1));
    // The reconnect delay is 3 s; a retry would already be scheduled. Give it room to show.
    await new Promise((r) => setTimeout(r, 50));
    expect(fetchMock).toHaveBeenCalledTimes(1);
    unsubscribe();
  });
});
