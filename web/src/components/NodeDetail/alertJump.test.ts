// SPDX-License-Identifier: AGPL-3.0-only
import { describe, expect, it } from 'vitest';
import { alertJumpOf, offersJump, portToSelect } from './alertJump';

describe('alertJumpOf', () => {
  it('sends a port alert to the Interfaces tab with its ifindex', () => {
    expect(alertJumpOf({ ifindex: 12 })).toEqual({ tab: 'interfaces', ifindex: 12 });
  });

  it('keeps ifindex 0, which is a real index on some devices', () => {
    expect(alertJumpOf({ ifindex: 0 })).toEqual({ tab: 'interfaces', ifindex: 0 });
  });

  it('sends everything else to Overview', () => {
    expect(alertJumpOf({})).toEqual({ tab: 'overview' });
    expect(alertJumpOf({ ifindex: null })).toEqual({ tab: 'overview' });
  });
});

describe('offersJump', () => {
  it('always offers a port jump, the Interfaces tab included', () => {
    const j = alertJumpOf({ ifindex: 3 });
    expect(offersJump(j, 'interfaces')).toBe(true);
    expect(offersJump(j, 'events')).toBe(true);
  });

  it('offers Overview only away from it', () => {
    const j = alertJumpOf({});
    expect(offersJump(j, 'overview')).toBe(false);
    expect(offersJump(j, 'interfaces')).toBe(true);
  });
});

describe('portToSelect', () => {
  it('selects the port once the list has it', () => {
    expect(portToSelect({ ifindex: 5, seq: 1 }, 0, [])).toBeNull();
    expect(portToSelect({ ifindex: 5, seq: 1 }, 0, [4, 5])).toBe(5);
  });

  it('does not apply the same request twice', () => {
    expect(portToSelect({ ifindex: 5, seq: 1 }, 1, [5])).toBeNull();
  });

  it('applies a second press on the same port', () => {
    expect(portToSelect({ ifindex: 5, seq: 2 }, 1, [5])).toBe(5);
  });

  it('does nothing without a request', () => {
    expect(portToSelect(null, 0, [5])).toBeNull();
    expect(portToSelect(undefined, 0, [5])).toBeNull();
  });
});
