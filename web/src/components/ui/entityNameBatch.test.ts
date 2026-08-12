// SPDX-License-Identifier: AGPL-3.0-only
// Unit tests for the node-name batcher (no DOM — Vitest node env).
//
// Every case here is a way rows were showing a raw UUID instead of a name.

import { describe, expect, it, vi } from 'vitest';
import {
  createNameBatcher,
  NAME_BATCH_MAX,
  NAME_MAX_ATTEMPTS,
  type NameEntry,
} from './entityNameBatch';

/** A batcher whose scheduler is a manual queue, so a test drives the flush instead of racing it. */
function harness(
  respond: (ids: string[]) => Promise<NameEntry[]> = (ids) =>
    Promise.resolve(ids.map((id) => ({ id, name: `name-${id}` }))),
) {
  const scheduled: (() => void)[] = [];
  const resolved: NameEntry[][] = [];
  const fetchNames = vi.fn(respond);
  const batcher = createNameBatcher({
    fetchNames,
    schedule: (flush) => scheduled.push(flush),
    onResolved: (entries) => resolved.push(entries),
  });
  /** Run every flush the batcher has asked for, then let the promises settle. */
  const tick = async () => {
    const due = scheduled.splice(0);
    due.forEach((fn) => fn());
    await Promise.resolve();
    await Promise.resolve();
  };
  return { batcher, fetchNames, resolved, scheduled, tick };
}

describe('createNameBatcher', () => {
  it('sends one request for everything asked for before the flush', async () => {
    // The reason the batch exists: a table cell asks per row, and a page of rows must not become a
    // page of requests.
    const h = harness();
    h.batcher.request('a');
    h.batcher.request('b');
    h.batcher.request('c');
    expect(h.scheduled).toHaveLength(1);
    await h.tick();
    expect(h.fetchNames).toHaveBeenCalledTimes(1);
    expect(h.fetchNames).toHaveBeenCalledWith(['a', 'b', 'c']);
    expect(h.resolved).toEqual([
      [
        { id: 'a', name: 'name-a' },
        { id: 'b', name: 'name-b' },
        { id: 'c', name: 'name-c' },
      ],
    ]);
  });

  it('schedules from the request, so it does not depend on who re-rendered', async () => {
    // The bug this replaces: the request was fired from an effect on the component that owns the
    // hook, while the ids are enqueued by its children. A virtualized list renders its rows in a
    // commit the owner takes no part in, so the effect never ran and the batch was never sent. A
    // request must be enough on its own.
    const h = harness();
    h.batcher.request('a');
    expect(h.scheduled).toHaveLength(1);
    await h.tick();
    expect(h.fetchNames).toHaveBeenCalledWith(['a']);
  });

  it('asks again for an id whose request failed', async () => {
    // One network blip used to pin those rows to a raw UUID for the life of the page: the id was
    // marked "asked" before the request and never unmarked.
    let attempt = 0;
    const h = harness((ids) => {
      attempt += 1;
      return attempt === 1
        ? Promise.reject(new Error('offline'))
        : Promise.resolve(ids.map((id) => ({ id, name: `name-${id}` })));
    });
    h.batcher.request('a');
    await h.tick();
    expect(h.resolved).toEqual([]);

    h.batcher.request('a');
    await h.tick();
    expect(h.fetchNames).toHaveBeenCalledTimes(2);
    expect(h.resolved).toEqual([[{ id: 'a', name: 'name-a' }]]);
  });

  it('never asks twice for an id the server answered about', async () => {
    const h = harness();
    h.batcher.request('a');
    await h.tick();
    h.batcher.request('a');
    expect(h.scheduled).toHaveLength(0);
    await h.tick();
    expect(h.fetchNames).toHaveBeenCalledTimes(1);
  });

  it('never asks twice for an id the server had no name for', async () => {
    // A deleted or out-of-scope node is omitted from the response rather than reported. Retrying
    // that would be a fetch loop on every render, which is why "asked" is kept on success even
    // when nothing came back — the cell falls back to the id, which is the only handle there is.
    const h = harness(() => Promise.resolve([]));
    h.batcher.request('ghost');
    await h.tick();
    expect(h.resolved).toEqual([]);
    h.batcher.request('ghost');
    await h.tick();
    expect(h.fetchNames).toHaveBeenCalledTimes(1);
  });

  it('does not re-enqueue an id that is already in flight', async () => {
    const h = harness();
    h.batcher.request('a');
    const due = h.scheduled.splice(0);
    due.forEach((fn) => fn()); // in flight, response not settled
    h.batcher.request('a');
    expect(h.scheduled).toHaveLength(0);
    await h.tick();
    expect(h.fetchNames).toHaveBeenCalledTimes(1);
  });

  it('starts a second batch for ids asked about after the first flush', async () => {
    const h = harness();
    h.batcher.request('a');
    await h.tick();
    h.batcher.request('b');
    expect(h.scheduled).toHaveLength(1);
    await h.tick();
    expect(h.fetchNames).toHaveBeenNthCalledWith(1, ['a']);
    expect(h.fetchNames).toHaveBeenNthCalledWith(2, ['b']);
  });

  it('ignores an empty id', async () => {
    // `alertSubject` yields `''` for a malformed subject; asking the server about it would be a
    // request that can only 400 or come back empty.
    const h = harness();
    h.batcher.request('');
    expect(h.scheduled).toHaveLength(0);
    await h.tick();
    expect(h.fetchNames).not.toHaveBeenCalled();
  });

  it('does not report an empty response as resolved names', async () => {
    const h = harness(() => Promise.resolve([]));
    h.batcher.request('a');
    await h.tick();
    expect(h.resolved).toEqual([]);
  });

  describe('chunking', () => {
    it('stays under the cap the server silently truncates at', async () => {
      // `POST /node-names` truncates rather than refusing, so an over-long batch returns 200 having
      // dropped its tail — which reads exactly like "those nodes have no name", and the ids are
      // marked asked, so they never recover. The Active-alerts search reaches this by asking about
      // every open alert's subject at once.
      const h = harness();
      const ids = Array.from({ length: NAME_BATCH_MAX * 2 + 3 }, (_, i) => `id-${i}`);
      ids.forEach((id) => h.batcher.request(id));
      await h.tick();
      expect(h.fetchNames).toHaveBeenCalledTimes(3);
      const sent = h.fetchNames.mock.calls.map((c) => c[0]);
      expect(sent.every((c) => c.length <= NAME_BATCH_MAX)).toBe(true);
      // Every id goes out exactly once, across the chunks.
      expect(sent.flat().sort()).toEqual([...ids].sort());
    });

    it('gives up on an id that keeps failing, instead of retrying forever', async () => {
      // Not every failure is transient. A caller that passes something the endpoint cannot parse
      // as a UUID gets a 400 for the whole batch, every time — and a live page re-renders often
      // enough (SSE) to re-ask on each one, so an unbounded retry is a request storm.
      const h = harness(() => Promise.reject(new Error('400')));
      for (let i = 0; i < NAME_MAX_ATTEMPTS + 3; i++) {
        h.batcher.request('bad');
        await h.tick();
      }
      expect(h.fetchNames).toHaveBeenCalledTimes(NAME_MAX_ATTEMPTS);
    });

    it('re-asks only the chunk that failed', async () => {
      const h = harness((batch) =>
        batch.includes('id-0')
          ? Promise.reject(new Error('offline'))
          : Promise.resolve(batch.map((id) => ({ id, name: `name-${id}` }))),
      );
      const ids = Array.from({ length: NAME_BATCH_MAX + 1 }, (_, i) => `id-${i}`);
      ids.forEach((id) => h.batcher.request(id));
      await h.tick();
      expect(h.fetchNames).toHaveBeenCalledTimes(2);

      // Everything is asked for again; only the failed chunk's ids are still unknown.
      ids.forEach((id) => h.batcher.request(id));
      await h.tick();
      const retried = h.fetchNames.mock.calls[2]?.[0] ?? [];
      expect(retried).toHaveLength(NAME_BATCH_MAX);
      expect(retried).toContain('id-0');
      expect(retried).not.toContain(`id-${NAME_BATCH_MAX}`);
    });
  });
});
