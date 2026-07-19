import { describe, it, expect } from 'vitest';
import { buildFlowFilters, toggleFilterValue } from './flowFilters';

describe('buildFlowFilters', () => {
  it('omits blank inputs', () => {
    expect(buildFlowFilters({ proto: '', port: '', peer: '' })).toEqual({});
  });

  it('includes a single set filter', () => {
    expect(buildFlowFilters({ proto: '6', port: '', peer: '' })).toEqual({ proto: 6 });
    expect(buildFlowFilters({ proto: '', port: '443', peer: '' })).toEqual({ port: 443 });
    expect(buildFlowFilters({ proto: '', port: '', peer: '8.8.8.8' })).toEqual({ peer: '8.8.8.8' });
  });

  it('ANDs two or more set filters (cross-card selection)', () => {
    expect(buildFlowFilters({ proto: '6', port: '', peer: '8.8.8.8' })).toEqual({
      proto: 6,
      peer: '8.8.8.8',
    });
    expect(buildFlowFilters({ proto: '17', port: '53', peer: '1.1.1.1' })).toEqual({
      proto: 17,
      port: 53,
      peer: '1.1.1.1',
    });
  });

  it('trims the peer and rejects out-of-range / non-integer ports', () => {
    expect(buildFlowFilters({ proto: '', port: '', peer: '  9.9.9.9  ' })).toEqual({
      peer: '9.9.9.9',
    });
    expect(buildFlowFilters({ proto: '', port: '70000', peer: '' })).toEqual({});
    expect(buildFlowFilters({ proto: '', port: 'abc', peer: '' })).toEqual({});
  });
});

describe('toggleFilterValue', () => {
  it('sets the clicked value when nothing (or something else) is active', () => {
    expect(toggleFilterValue('', '6')).toBe('6');
    expect(toggleFilterValue('6', '17')).toBe('17');
    expect(toggleFilterValue('', '8.8.8.8')).toBe('8.8.8.8');
  });

  it('clears the filter when the active value is clicked again (toggle off)', () => {
    expect(toggleFilterValue('6', '6')).toBe('');
    expect(toggleFilterValue('8.8.8.8', '8.8.8.8')).toBe('');
  });
});
