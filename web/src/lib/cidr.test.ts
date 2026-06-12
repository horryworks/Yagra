import { describe, expect, it } from 'vitest';
import { expandCidr } from './cidr';

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
