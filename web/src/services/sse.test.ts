import { afterEach, describe, expect, it, vi } from 'vitest';
import {
  dataFromEventBlock,
  parseAlertEvent,
  parseAnalysisJob,
  parseReportRun,
  subscribeAnalysis,
} from './sse';
import { setToken } from './api';

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
