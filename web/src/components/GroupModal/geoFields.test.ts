// SPDX-License-Identifier: AGPL-3.0-only
import { describe, expect, it } from 'vitest';
import { geoBodyFrom, geoChanged, geoDraftFrom, inheritedPin } from './geoFields';
import type { NodeGroup } from '../../types/api';

const group = (latitude: number | null, longitude: number | null): NodeGroup =>
  ({ id: 'g1', name: 'Tokyo', group_type: 'site', latitude, longitude }) as NodeGroup;

/** A folder the server resolved onto an ancestor's pin. */
const inheriting = (parent_id: string | null, geo_group: string | null): NodeGroup =>
  ({
    id: 'rack',
    name: 'Rack A',
    group_type: 'generic',
    parent_id,
    latitude: null,
    longitude: null,
    geo_source: geo_group ? 'inherited' : 'unset',
    geo_group,
  }) as NodeGroup;

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

describe('inheritedPin', () => {
  const blank = { latitude: '', longitude: '' };

  it('names the ancestor whose pin this folder already sits on', () => {
    expect(inheritedPin(inheriting('tokyo', 'tokyo'), blank, 'tokyo')).toBe('tokyo');
    // The supplying group need not be the immediate parent — the server walked to the nearest
    // placed one, and the dialog reports that answer rather than re-deriving it.
    expect(inheritedPin(inheriting('floor2', 'tokyo'), blank, 'floor2')).toBe('tokyo');
  });

  it('says nothing when the folder is not inheriting a position', () => {
    expect(inheritedPin(inheriting('tokyo', null), blank, 'tokyo')).toBeNull();
    expect(inheritedPin(undefined, blank, null)).toBeNull();
    // A folder that carries its own pin is `own`, not `inherited`, so there is nothing to explain.
    expect(inheritedPin(group(35.68, 139.76), blank, null)).toBeNull();
  });

  it('goes quiet once the operator types coordinates of their own', () => {
    // Half-typed counts: the moment there is anything in either box the folder is on its way to
    // being its own pin, and "already on the map at Tokyo" would be describing the old state.
    expect(inheritedPin(inheriting('tokyo', 'tokyo'), { latitude: '3', longitude: '' }, 'tokyo')).toBeNull();
    expect(inheritedPin(inheriting('tokyo', 'tokyo'), { latitude: '', longitude: '13' }, 'tokyo')).toBeNull();
  });

  it('goes quiet while the folder is being moved', () => {
    // The server answered for the stored parent. Mid-move to another site that answer is stale,
    // and a confidently wrong "inherited from Tokyo" is worse than no hint at all.
    expect(inheritedPin(inheriting('tokyo', 'tokyo'), blank, 'osaka')).toBeNull();
    expect(inheritedPin(inheriting('tokyo', 'tokyo'), blank, null)).toBeNull();
    // Moving to root and back again restores it.
    expect(inheritedPin(inheriting(null, 'tokyo'), blank, null)).toBe('tokyo');
  });
});
