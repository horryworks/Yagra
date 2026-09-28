// SPDX-License-Identifier: AGPL-3.0-only
// @vitest-environment jsdom
import { act, renderHook } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { readSources } from '../testSupport/sources';
import { COPY_FLASH_MS, useCopy } from './useCopy';

describe('useCopy', () => {
  const writeText = vi.fn().mockResolvedValue(undefined);

  beforeEach(() => {
    vi.useFakeTimers();
    Object.defineProperty(navigator, 'clipboard', { value: { writeText }, configurable: true });
    writeText.mockClear();
  });
  afterEach(() => {
    vi.useRealTimers();
  });

  it('writes the text and flashes the mark, then clears it', () => {
    const { result } = renderHook(() => useCopy());
    act(() => result.current.copy('secret', 'token'));
    expect(writeText).toHaveBeenCalledWith('secret');
    expect(result.current.copied).toBe('token');
    act(() => vi.advanceTimersByTime(COPY_FLASH_MS));
    expect(result.current.copied).toBeNull();
  });

  it('marks with the text itself when no mark is given', () => {
    const { result } = renderHook(() => useCopy());
    act(() => result.current.copy('0b0e6a9e'));
    expect(result.current.copied).toBe('0b0e6a9e');
  });

  it('a second copy restarts the flash rather than being cut short by the first timer', () => {
    const { result } = renderHook(() => useCopy(1000));
    act(() => result.current.copy('a', 'url'));
    act(() => vi.advanceTimersByTime(800));
    act(() => result.current.copy('b', 'token'));
    act(() => vi.advanceTimersByTime(800));
    expect(result.current.copied).toBe('token');
    act(() => vi.advanceTimersByTime(200));
    expect(result.current.copied).toBeNull();
  });

  it('leaves no timer behind when the dialog closes within the flash', () => {
    const { result, unmount } = renderHook(() => useCopy());
    act(() => result.current.copy('x'));
    unmount();
    expect(vi.getTimerCount()).toBe(0);
  });
});

/**
 * ADR-184: six components copied to the clipboard with their own flag-and-timer closure. Reading
 * the clipboard (a paste handler's `clipboardData`) is not a copy and is not matched.
 *
 * ⚠️ Assembled at runtime, or it would match this file.
 */
describe('no component copies to the clipboard by hand', () => {
  const COPY = new RegExp(`clipboard\\??\\.${'writeText'}\\(`);

  it('every copy goes through useCopy', () => {
    const offenders = readSources()
      .filter(([p, src]) => p !== 'lib/useCopy.ts' && COPY.test(src))
      .map(([p]) => p);
    expect(offenders, `use useCopy from lib/useCopy:\n  ${offenders.join('\n  ')}`).toEqual([]);
  });

  it('finds the sources it is supposed to be reading', () => {
    const files = readSources();
    expect(files.length).toBeGreaterThan(300);
    expect(COPY.test(files.find(([p]) => p === 'lib/useCopy.ts')?.[1] ?? '')).toBe(true);
    expect(files.filter(([, src]) => src.includes('useCopy(')).length).toBeGreaterThanOrEqual(7);
  });
});
