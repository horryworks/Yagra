import { describe, expect, it } from 'vitest';
import { expandCidr, expandTargets } from './cidr';

describe('expandCidr', () => {
  it('expands a /24 to its 254 usable hosts', () => {
    const ips = expandCidr('192.168.1.0/24');
    expect(ips).toHaveLength(254);
    expect(ips[0]).toBe('192.168.1.1');
    expect(ips[ips.length - 1]).toBe('192.168.1.254');
  });

  it('returns a single bare IP unchanged', () => {
    expect(expandCidr('192.168.1.5')).toEqual(['192.168.1.5']);
  });

  it('includes both addresses of a /31', () => {
    expect(expandCidr('10.0.0.0/31')).toEqual(['10.0.0.0', '10.0.0.1']);
  });

  it('rejects ranges larger than the cap', () => {
    expect(expandCidr('10.0.0.0/8')).toEqual([]);
    expect(expandCidr('172.16.0.0/16')).toEqual([]);
  });

  it('rejects malformed input', () => {
    expect(expandCidr('')).toEqual([]);
    expect(expandCidr('192.168.1.0/33')).toEqual([]);
    expect(expandCidr('999.1.1.1/24')).toEqual([]);
  });
});

describe('expandTargets', () => {
  it('expands a single CIDR like expandCidr', () => {
    expect(expandTargets('192.168.1.0/24')).toHaveLength(254);
  });

  it('validates a single bare IP', () => {
    expect(expandTargets('10.0.0.5')).toEqual(['10.0.0.5']);
    expect(expandTargets('10.0.0.999')).toEqual([]);
    expect(expandTargets('not-an-ip')).toEqual([]);
  });

  it('expands a full inclusive range', () => {
    expect(expandTargets('192.168.1.10-192.168.1.12')).toEqual([
      '192.168.1.10',
      '192.168.1.11',
      '192.168.1.12',
    ]);
  });

  it('expands last-octet shorthand range', () => {
    expect(expandTargets('192.168.1.10-12')).toEqual([
      '192.168.1.10',
      '192.168.1.11',
      '192.168.1.12',
    ]);
  });

  it('rejects a reversed or oversized range', () => {
    expect(expandTargets('192.168.1.20-10')).toEqual([]);
    expect(expandTargets('10.0.0.0-10.0.255.255')).toEqual([]);
  });

  it('combines a comma- and whitespace-separated list and de-duplicates', () => {
    expect(expandTargets('10.0.0.1, 10.0.0.3-4  10.0.0.1')).toEqual([
      '10.0.0.1',
      '10.0.0.3',
      '10.0.0.4',
    ]);
  });

  it('rejects the whole spec if any token is malformed', () => {
    expect(expandTargets('10.0.0.1, garbage')).toEqual([]);
  });

  it('rejects when the combined total exceeds the cap (5×/24 = 1270 > 1024)', () => {
    const spec = [0, 1, 2, 3, 4].map((n) => `192.168.${n}.0/24`).join(', ');
    expect(expandTargets(spec)).toEqual([]);
  });

  it('returns [] for empty input', () => {
    expect(expandTargets('')).toEqual([]);
    expect(expandTargets('   ')).toEqual([]);
  });
});
