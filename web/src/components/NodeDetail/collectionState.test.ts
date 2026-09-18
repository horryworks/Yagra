// SPDX-License-Identifier: AGPL-3.0-only
import { describe, expect, it } from 'vitest';
import type { NodeMetricEntry } from '../../types/api';
import { collectionState } from './collectionState';

const entry = (status: NodeMetricEntry['status']): NodeMetricEntry =>
  ({ metric: 'm', metric_kind: 'gauge', status }) as NodeMetricEntry;

describe('collectionState', () => {
  it('is none for a node that is not walked, whatever the inventory says', () => {
    expect(collectionState(false, [entry('ok')])).toBe('none');
    expect(collectionState(false, null)).toBe('none');
  });

  it('is ok as soon as one metric is flowing', () => {
    expect(collectionState(true, [entry('no_data'), entry('ok')])).toBe('ok');
  });

  it('is failing when the inventory was read and nothing in it is flowing', () => {
    expect(collectionState(true, [entry('no_data')])).toBe('failing');
    expect(collectionState(true, [])).toBe('failing');
  });

  it('is unknown — never failing — when the inventory could not be read', () => {
    // The defect this pins: a failed read used to arrive as `[]`, which is the line above.
    expect(collectionState(true, null)).toBe('unknown');
  });

  it('is loading — never failing — before the inventory has answered', () => {
    expect(collectionState(true, undefined)).toBe('loading');
    // A node that is not walked needs no inventory to say so.
    expect(collectionState(false, undefined)).toBe('none');
  });
});
