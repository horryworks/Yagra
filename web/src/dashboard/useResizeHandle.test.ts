// SPDX-License-Identifier: AGPL-3.0-only
// @vitest-environment jsdom
import { renderHook } from '@testing-library/react';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import type { RowSpan, Span, WidgetDefinition, WidgetInstance } from './types';
import { useResizeHandle } from './useResizeHandle';

// The pointer-drag geometry is pure and already covered in `resize.test.ts`. What is only reachable
// through the hook is the **keyboard** path — the accessibility route that replaced the old size
// dropdowns — plus the idle contract (`preview` is null until a drag starts). Both are asserted
// here without synthesising a real pointer-capture sequence, which jsdom does not implement.

const SPANS: Span[] = [4, 6, 8, 12];
const ROWS: RowSpan[] = [1, 2, 3];

const instance = (over: Partial<WidgetInstance> = {}): WidgetInstance => ({
  instanceId: 'w1',
  type: 'status-summary',
  span: 6,
  ...over,
});

const def = (allowedRowSpans?: RowSpan[]): WidgetDefinition =>
  ({ type: 'status-summary', allowedSpans: SPANS, allowedRowSpans }) as WidgetDefinition;

/** A minimal React.KeyboardEvent stand-in — the hook only reads `key` and calls `preventDefault`. */
const keyEvent = (key: string) => {
  const preventDefault = vi.fn();
  return { event: { key, preventDefault } as never, preventDefault };
};

describe('useResizeHandle', () => {
  let setSize: ReturnType<typeof vi.fn<(id: string, span: number, rowSpan: number) => void>>;
  beforeEach(() => {
    setSize = vi.fn<(id: string, span: number, rowSpan: number) => void>();
  });

  const mount = (inst: WidgetInstance, d: WidgetDefinition) =>
    renderHook(() => useResizeHandle(inst, d, setSize));

  it('exposes no preview while idle', () => {
    const { result } = mount(instance(), def(ROWS));
    expect(result.current.preview).toBeNull();
  });

  it('widens and narrows through the allowed spans with Arrow Right/Left', () => {
    const { result } = mount(instance({ span: 6 }), def(ROWS));

    result.current.handleProps.onKeyDown(keyEvent('ArrowRight').event);
    expect(setSize).toHaveBeenLastCalledWith('w1', 8, 1);

    setSize.mockClear();
    result.current.handleProps.onKeyDown(keyEvent('ArrowLeft').event);
    expect(setSize).toHaveBeenLastCalledWith('w1', 4, 1);
  });

  it('grows and shrinks height with Arrow Up/Down', () => {
    const { result } = mount(instance({ span: 6, rowSpan: 2 }), def(ROWS));

    result.current.handleProps.onKeyDown(keyEvent('ArrowUp').event);
    expect(setSize).toHaveBeenLastCalledWith('w1', 6, 3);

    setSize.mockClear();
    result.current.handleProps.onKeyDown(keyEvent('ArrowDown').event);
    expect(setSize).toHaveBeenLastCalledWith('w1', 6, 1);
  });

  it('treats a missing rowSpan as 1', () => {
    const { result } = mount(instance({ span: 6 }), def(ROWS));
    result.current.handleProps.onKeyDown(keyEvent('ArrowUp').event);
    expect(setSize).toHaveBeenLastCalledWith('w1', 6, 2);
  });

  it('ignores height keys for a fixed-height widget', () => {
    const { result } = mount(instance({ span: 6 }), def(undefined));

    result.current.handleProps.onKeyDown(keyEvent('ArrowUp').event);
    result.current.handleProps.onKeyDown(keyEvent('ArrowDown').event);
    expect(setSize).not.toHaveBeenCalled();
  });

  it('does not commit at the ends of the allowed range', () => {
    const wide = mount(instance({ span: 12 }), def(ROWS));
    wide.result.current.handleProps.onKeyDown(keyEvent('ArrowRight').event);
    expect(setSize).not.toHaveBeenCalled();

    const narrow = mount(instance({ span: 4 }), def(ROWS));
    narrow.result.current.handleProps.onKeyDown(keyEvent('ArrowLeft').event);
    expect(setSize).not.toHaveBeenCalled();

    const tall = mount(instance({ span: 6, rowSpan: 3 }), def(ROWS));
    tall.result.current.handleProps.onKeyDown(keyEvent('ArrowUp').event);
    expect(setSize).not.toHaveBeenCalled();
  });

  it('claims the arrow keys but leaves every other key to the page', () => {
    const { result } = mount(instance({ span: 6 }), def(ROWS));

    const arrow = keyEvent('ArrowRight');
    result.current.handleProps.onKeyDown(arrow.event);
    expect(arrow.preventDefault).toHaveBeenCalled();

    // Tab/Enter must keep working — the grip sits in the normal focus order.
    for (const key of ['Tab', 'Enter', ' ', 'Escape', 'a']) {
      const other = keyEvent(key);
      result.current.handleProps.onKeyDown(other.event);
      expect(other.preventDefault, `${key} must not be swallowed`).not.toHaveBeenCalled();
    }
    expect(setSize).toHaveBeenCalledTimes(1);
  });

  it('ignores pointer move and up when no drag is in progress', () => {
    const { result } = mount(instance(), def(ROWS));
    const evt = { clientX: 10, clientY: 10, pointerId: 1 } as never;

    expect(() => result.current.handleProps.onPointerMove(evt)).not.toThrow();
    expect(() => result.current.handleProps.onPointerUp(evt)).not.toThrow();
    expect(setSize).not.toHaveBeenCalled();
    expect(result.current.preview).toBeNull();
  });
});
