// SPDX-License-Identifier: AGPL-3.0-only
import { describe, expect, it } from 'vitest';
import type { NameFromDevicePreview } from '../../types/api';
import { applyBody, NAME_FROM_DEVICE_SHOWN, shownRows, unchangedCounts } from './nameFromDevice';

const row = (i: number) => ({ node_id: `n-${i}`, name: `Switch ${i}`, sys_name: `sw-${i}` });

const preview = (over: Partial<NameFromDevicePreview> = {}): NameFromDevicePreview => ({
  requested: 12,
  changes: [row(1), row(2), row(3)],
  no_device_name: 2,
  already_same: 1,
  ...over,
});

describe('shownRows', () => {
  it('lists a few rows and folds the rest into a count', () => {
    const many = preview({ changes: Array.from({ length: 11 }, (_, i) => row(i)) });
    const { rows, more } = shownRows(many);
    expect(rows).toHaveLength(NAME_FROM_DEVICE_SHOWN);
    expect(more).toBe(11 - NAME_FROM_DEVICE_SHOWN);
    expect(shownRows(preview()).more).toBe(0);
  });
});

describe('unchangedCounts', () => {
  it('counts what the server left out of both buckets as other', () => {
    expect(unchangedCounts(preview())).toEqual({
      total: 9,
      noDeviceName: 2,
      alreadySame: 1,
      other: 6,
    });
  });

  it('never goes negative', () => {
    expect(unchangedCounts(preview({ requested: 1 })).other).toBe(0);
  });
});

describe('applyBody', () => {
  it('echoes every row as it was shown', () => {
    expect(applyBody(preview({ changes: [row(1)] }))).toEqual({
      changes: [{ node_id: 'n-1', from: 'Switch 1', to: 'sw-1' }],
    });
  });

  it('sends nothing when nothing would change', () => {
    expect(applyBody(preview({ changes: [] }))).toBeNull();
  });
});
