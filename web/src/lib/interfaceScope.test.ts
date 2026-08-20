// SPDX-License-Identifier: AGPL-3.0-only
import { describe, expect, it } from 'vitest';
import { interfaceScopeId, isInterfaceScopeId, splitInterfaceScopeId } from './interfaceScope';

const NODE = '6f1c9d2a-0b3e-4a71-9c8d-2e5f7a1b4c60';

describe('interface scope ids (ADR-076)', () => {
  it('round-trips every port a device can name', () => {
    // 0 is a real ifIndex on some agents, and 2^32-1 is the top of the range — both must survive
    // as values rather than collapsing to "no port" through a falsy or overflow check.
    for (const port of [0, 1, 7, 65535, 4294967295]) {
      const id = interfaceScopeId(NODE, port);
      expect(splitInterfaceScopeId(id)).toEqual([NODE, port]);
      expect(isInterfaceScopeId(id)).toBe(true);
    }
  });

  it('refuses every spelling the server would refuse', () => {
    // This list is the same table as the Rust side's
    // `an_interface_scope_id_round_trips_and_rejects_everything_else`. The two are a mirror with
    // no mechanical link, so they are kept identical on purpose.
    for (const bad of [
      '',
      NODE, // no port
      `${NODE}:`, // empty port
      `${NODE}:-1`, // negative
      `${NODE}:+7`, // signed — `Number('+7')` is 7, which is exactly the trap
      `${NODE}: 7`, // padded — `Number(' 7')` is 7 too
      `${NODE}:x`,
      `${NODE}:4294967296`, // one past the range
      'not-a-uuid:7',
      ':7',
    ]) {
      expect(isInterfaceScopeId(bad)).toBe(false);
    }
  });

  it('keeps the node half readable even when the port half is not', () => {
    // A malformed id still names a node, and the caller can resolve that much rather than
    // rendering the whole raw string at the operator.
    expect(splitInterfaceScopeId(`${NODE}:x`)).toEqual([NODE, null]);
    expect(splitInterfaceScopeId(NODE)).toEqual([NODE, null]);
  });

  it('splits on the FIRST colon, so the node half is never truncated', () => {
    // A uuid contains no colon, so this only matters for malformed input — but splitting on the
    // last one would hand back a node id with a colon in it, which resolves to nothing at all.
    expect(splitInterfaceScopeId(`${NODE}:7:9`)).toEqual([NODE, null]);
  });
});
