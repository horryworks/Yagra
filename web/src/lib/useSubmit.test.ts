// SPDX-License-Identifier: AGPL-3.0-only
// @vitest-environment jsdom
// The wiring half of the shared dialog save. What an answer does to the dialog is `submitReducer`,
// tested in `submitState.test.ts` without a DOM; this pins who is told, and how often it sends.
import { act, renderHook, waitFor } from '@testing-library/react';
import { describe, expect, it, vi } from 'vitest';
import { ApiError } from '../services/api';
import { done, WordedFailure } from './submitState';
import { useSubmit } from './useSubmit';

function deferred<T>() {
  let resolve!: (v: T) => void;
  const promise = new Promise<T>((r) => (resolve = r));
  return { promise, resolve };
}

describe('useSubmit', () => {
  it('sends once however often it is asked while in flight (F2)', async () => {
    const onDone = vi.fn();
    const { result } = renderHook(() => useSubmit({ errorFallback: 'x', onDone }));
    const answer = deferred<ReturnType<typeof done>>();
    const call = vi.fn(() => answer.promise);
    act(() => {
      result.current.submit(call);
      result.current.submit(call);
    });
    expect(call).toHaveBeenCalledTimes(1);
    expect(result.current.busy).toBe(true);

    await act(async () => answer.resolve(done()));
    expect(onDone).toHaveBeenCalledTimes(1);
    // Still busy: the dialog is closing (F1). And still refusing a second send.
    expect(result.current.busy).toBe(true);
    act(() => result.current.submit(call));
    expect(call).toHaveBeenCalledTimes(1);
  });

  it("shows the server's message, or the fallback, and lets the operator try again", async () => {
    const { result } = renderHook(() =>
      useSubmit({ errorFallback: 'save failed', onDone: vi.fn() }),
    );
    act(() =>
      result.current.submit(() => Promise.reject(new ApiError('conflict', 'name taken', 409))),
    );
    await waitFor(() => expect(result.current.error).toBe('name taken'));
    expect(result.current.busy).toBe(false);

    act(() => result.current.submit(() => Promise.reject(new TypeError('network'))));
    await waitFor(() => expect(result.current.error).toBe('save failed'));
  });

  it('lets a dialog word a failure itself, and falls through when it declines', async () => {
    const describeError = (e: unknown) =>
      e instanceof ApiError && e.code === 'last_admin' ? 'keep one admin' : null;
    const { result } = renderHook(() =>
      useSubmit({ errorFallback: 'x', describeError, onDone: vi.fn() }),
    );
    act(() =>
      result.current.submit(() => Promise.reject(new ApiError('last_admin', 'server words', 409))),
    );
    await waitFor(() => expect(result.current.error).toBe('keep one admin'));
    act(() => result.current.submit(() => Promise.reject(new ApiError('internal', 'boom', 500))));
    await waitFor(() => expect(result.current.error).toBe('boom'));
  });

  it('shows a failure the dialog already worded, as it is', async () => {
    const { result } = renderHook(() =>
      useSubmit({ errorFallback: 'x', describeError: () => 'not me', onDone: vi.fn() }),
    );
    act(() => result.current.submit(() => Promise.reject(new WordedFailure('stage two failed'))));
    await waitFor(() => expect(result.current.error).toBe('stage two failed'));
  });

  it('a partial batch refreshes the list only when asked, and keeps the dialog', async () => {
    const onDone = vi.fn();
    const onSaved = vi.fn();
    const { result } = renderHook(() => useSubmit({ errorFallback: 'x', onDone, onSaved }));
    act(() =>
      result.current.submit(() =>
        Promise.resolve({ kind: 'keepOpen' as const, message: '1 of 3', refresh: false }),
      ),
    );
    await waitFor(() => expect(result.current.settled).toBe(true));
    expect(onSaved).not.toHaveBeenCalled();

    act(() =>
      result.current.submit(() =>
        Promise.resolve({ kind: 'keepOpen' as const, message: '2 of 3', refresh: true }),
      ),
    );
    await waitFor(() => expect(result.current.error).toBe('2 of 3'));
    expect(onSaved).toHaveBeenCalledTimes(1);
    expect(onDone).not.toHaveBeenCalled();
  });

  it('a call that throws before it returns a promise is a failure, not a stuck dialog', async () => {
    const { result } = renderHook(() => useSubmit({ errorFallback: 'fallback', onDone: vi.fn() }));
    act(() =>
      result.current.submit(() => {
        throw new Error('bad input');
      }),
    );
    await waitFor(() => expect(result.current.error).toBe('fallback'));
    expect(result.current.busy).toBe(false);
  });
});
