// SPDX-License-Identifier: AGPL-3.0-only
import { describe, expect, it } from 'vitest';
import { geoBodyFrom, geoChanged, geoDraftFrom } from './geoFields';
import type { NodeGroup } from '../../types/api';

const group = (latitude: number | null, longitude: number | null): NodeGroup =>
  ({ id: 'g1', name: 'Tokyo', group_type: 'site', latitude, longitude }) as NodeGroup;

describe('geoDraftFrom', () => {
  it('renders an existing pin as text', () => {
    expect(geoDraftFrom(group(35.68, 139.76))).toEqual({ latitude: '35.68', longitude: '139.76' });
  });

  it('renders a cleared pin, an absent group, and zero coordinates distinguishably', () => {
    expect(geoDraftFrom(group(null, null))).toEqual({ latitude: '', longitude: '' });
    expect(geoDraftFrom(undefined)).toEqual({ latitude: '', longitude: '' });
    // 0,0 is a real location (the Null Island buoy is a real station); it must not read as cleared.
    expect(geoDraftFrom(group(0, 0))).toEqual({ latitude: '0', longitude: '0' });
  });
});

describe('geoBodyFrom', () => {
  it('accepts a complete pair', () => {
    expect(geoBodyFrom({ latitude: '35.68', longitude: '139.76' })).toEqual({
      body: { latitude: 35.68, longitude: 139.76 },
    });
  });

  it('treats both-blank as clearing the pin, not as an error', () => {
    expect(geoBodyFrom({ latitude: '', longitude: '' })).toEqual({
      body: { latitude: null, longitude: null },
    });
    expect(geoBodyFrom({ latitude: '  ', longitude: '\t' })).toEqual({
      body: { latitude: null, longitude: null },
    });
  });

  it('rejects half a pair in either direction', () => {
    expect(geoBodyFrom({ latitude: '35.68', longitude: '' })).toEqual({ error: 'geoPair' });
    expect(geoBodyFrom({ latitude: '', longitude: '139.76' })).toEqual({ error: 'geoPair' });
  });

  it('rejects text that is not a finite number', () => {
    expect(geoBodyFrom({ latitude: '12abc', longitude: '0' })).toEqual({ error: 'geoNumber' });
    expect(geoBodyFrom({ latitude: 'Infinity', longitude: '0' })).toEqual({ error: 'geoNumber' });
  });

  it('enforces the same ranges the server does', () => {
    // Boundaries are inclusive on both sides, matching -90.0..=90.0 / -180.0..=180.0 in Rust.
    expect(geoBodyFrom({ latitude: '90', longitude: '180' })).toEqual({
      body: { latitude: 90, longitude: 180 },
    });
    expect(geoBodyFrom({ latitude: '-90', longitude: '-180' })).toEqual({
      body: { latitude: -90, longitude: -180 },
    });
    expect(geoBodyFrom({ latitude: '90.1', longitude: '0' })).toEqual({ error: 'geoRange' });
    expect(geoBodyFrom({ latitude: '0', longitude: '-180.5' })).toEqual({ error: 'geoRange' });
  });
});

describe('geoChanged', () => {
  it('is false when the draft still matches the group', () => {
    expect(geoChanged({ latitude: '35.68', longitude: '139.76' }, group(35.68, 139.76))).toBe(false);
    expect(geoChanged({ latitude: '', longitude: '' }, group(null, null))).toBe(false);
    // Whitespace the operator left behind is not a change.
    expect(geoChanged({ latitude: ' 35.68 ', longitude: '139.76' }, group(35.68, 139.76))).toBe(
      false,
    );
  });

  it('is true when a pin is set, moved, or cleared', () => {
    expect(geoChanged({ latitude: '35.68', longitude: '139.76' }, group(null, null))).toBe(true);
    expect(geoChanged({ latitude: '35.69', longitude: '139.76' }, group(35.68, 139.76))).toBe(true);
    expect(geoChanged({ latitude: '', longitude: '' }, group(35.68, 139.76))).toBe(true);
  });
});
