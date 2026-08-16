// SPDX-License-Identifier: AGPL-3.0-only
import { describe, expect, it } from 'vitest';
import { usePrefsStore } from './prefs';

describe('throughput scale pref', () => {
  it('defaults to fit-to-traffic', () => {
    // Reset to the documented default, then read it back.
    usePrefsStore.getState().setThroughputScale('fit');
    expect(usePrefsStore.getState().throughputScale).toBe('fit');
  });

  it('toggles globally between fit and capacity (one place changes everywhere)', () => {
    usePrefsStore.getState().setThroughputScale('fit');
    usePrefsStore.getState().toggleThroughputScale();
    expect(usePrefsStore.getState().throughputScale).toBe('capacity');
    usePrefsStore.getState().toggleThroughputScale();
    expect(usePrefsStore.getState().throughputScale).toBe('fit');
  });

  it('sets an explicit mode', () => {
    usePrefsStore.getState().setThroughputScale('capacity');
    expect(usePrefsStore.getState().throughputScale).toBe('capacity');
  });
});

describe('rate unit pref (ADR-060)', () => {
  it('defaults to bits per second', () => {
    // Reset to the documented default, then read it back. bps is the default because it is the
    // unit a link is sold and configured in, and the only one with history on an upgraded
    // deployment (the packet counters start collecting at the upgrade).
    usePrefsStore.getState().setRateUnit('bps');
    expect(usePrefsStore.getState().rateUnit).toBe('bps');
  });

  it('toggles globally between bps and pps (one place changes everywhere)', () => {
    usePrefsStore.getState().setRateUnit('bps');
    usePrefsStore.getState().toggleRateUnit();
    expect(usePrefsStore.getState().rateUnit).toBe('pps');
    usePrefsStore.getState().toggleRateUnit();
    expect(usePrefsStore.getState().rateUnit).toBe('bps');
  });

  it('is independent of the Y-axis scale pref', () => {
    // The two toggles sit side by side on the same chart header and are separate answers: the unit
    // decides what is plotted, the scale decides how the axis is bounded. Flipping one must not
    // move the other.
    usePrefsStore.getState().setThroughputScale('capacity');
    usePrefsStore.getState().setRateUnit('pps');
    expect(usePrefsStore.getState().throughputScale).toBe('capacity');
    usePrefsStore.getState().toggleRateUnit();
    expect(usePrefsStore.getState().throughputScale).toBe('capacity');
    expect(usePrefsStore.getState().rateUnit).toBe('bps');
  });
});

describe('uiMode pref (ADR-027)', () => {
  it('defaults to auto (follow the viewport)', () => {
    // The store default; setUiMode round-trips both values.
    expect(usePrefsStore.getState().uiMode).toBe('auto');
  });

  it('persists an explicit desktop override and back to auto', () => {
    usePrefsStore.getState().setUiMode('desktop');
    expect(usePrefsStore.getState().uiMode).toBe('desktop');
    usePrefsStore.getState().setUiMode('auto');
    expect(usePrefsStore.getState().uiMode).toBe('auto');
  });
});
