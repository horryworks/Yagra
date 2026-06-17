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
