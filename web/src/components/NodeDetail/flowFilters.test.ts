// SPDX-License-Identifier: AGPL-3.0-only
import { describe, it, expect } from 'vitest';
import { buildFlowFilters, toggleFilterValue } from './flowFilters';

describe('buildFlowFilters', () => {
  it('omits blank inputs', () => {
    expect(buildFlowFilters({ proto: '', port: '', peer: '', asn: '' })).toEqual({});
  });

  it('includes a single set filter', () => {
    expect(buildFlowFilters({ proto: '6', port: '', peer: '', asn: '' })).toEqual({ proto: 6 });
    expect(buildFlowFilters({ proto: '', port: '443', peer: '', asn: '' })).toEqual({ port: 443 });
    expect(buildFlowFilters({ proto: '', port: '', peer: '8.8.8.8', asn: '' })).toEqual({
      peer: '8.8.8.8',
    });
    expect(buildFlowFilters({ proto: '', port: '', peer: '', asn: '15169' })).toEqual({
      asn: 15169,
    });
  });

  it('ANDs two or more set filters (cross-card selection)', () => {
    expect(buildFlowFilters({ proto: '6', port: '', peer: '8.8.8.8', asn: '' })).toEqual({
      proto: 6,
      peer: '8.8.8.8',
    });
    expect(buildFlowFilters({ proto: '17', port: '53', peer: '1.1.1.1', asn: '13335' })).toEqual({
      proto: 17,
      port: 53,
      peer: '1.1.1.1',
      asn: 13335,
    });
  });

  it('trims the peer and rejects out-of-range / non-integer ports', () => {
    expect(buildFlowFilters({ proto: '', port: '', peer: '  9.9.9.9  ', asn: '' })).toEqual({
      peer: '9.9.9.9',
    });
    expect(buildFlowFilters({ proto: '', port: '70000', peer: '', asn: '' })).toEqual({});
    expect(buildFlowFilters({ proto: '', port: 'abc', peer: '', asn: '' })).toEqual({});
  });

  it('keeps AS 0 (the "unknown AS" bucket) but drops a non-numeric asn', () => {
    expect(buildFlowFilters({ proto: '', port: '', peer: '', asn: '0' })).toEqual({ asn: 0 });
    expect(buildFlowFilters({ proto: '', port: '', peer: '', asn: 'x' })).toEqual({});
  });
});

describe('toggleFilterValue', () => {
  it('sets the clicked value when nothing (or something else) is active', () => {
    expect(toggleFilterValue('', '6')).toBe('6');
    expect(toggleFilterValue('6', '17')).toBe('17');
    expect(toggleFilterValue('', '8.8.8.8')).toBe('8.8.8.8');
    expect(toggleFilterValue('15169', '13335')).toBe('13335');
  });

  it('clears the filter when the active value is clicked again (toggle off)', () => {
    expect(toggleFilterValue('6', '6')).toBe('');
    expect(toggleFilterValue('8.8.8.8', '8.8.8.8')).toBe('');
    expect(toggleFilterValue('15169', '15169')).toBe('');
  });
});
