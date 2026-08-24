// SPDX-License-Identifier: AGPL-3.0-only
import { readFileSync, readdirSync } from 'node:fs';
import { join, sep } from 'node:path';
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

/**
 * Nothing but a builder composes `<node>:<ifindex>` by hand.
 *
 * The mirror this module warns about at the top has no mechanical link to Rust, so the only defence
 * against a fourth spelling is that there are exactly two, both of them functions whose whole job is
 * to be the one spelling. `InterfaceRulesModal` had composed its own — it read `scope_ids` and
 * compared against a literal — which is precisely the failure the module doc describes: a rule that
 * is stored, listed, and matches no port.
 *
 * The two allowed entries are *builders*, not exemptions. Adding a third means writing why the
 * concept it names is not one of these two.
 */
describe('the composite has exactly two builders', () => {
  // Assembled from fragments so this file cannot match itself — the trap `permissions.test.ts` and
  // `reports/guards.rs::the_run_state_sql_is_built_from_the_enum` both document.
  const COMPOSITE = new RegExp(
    ['[$][{][A-Za-z0-9_.]*', 'node', '_?id[}]:[$][{][A-Za-z0-9_.]*', 'if', '_?index[}]'].join(''),
    'i',
  );

  /** Each builder, and the concept it owns. They share a format and nothing else: change the wire
   *  contract and `interfaceScopeId` moves; change how the traffic widget keys its React rows and
   *  `linkId` moves. Folding them together would make a server-side format change silently rewrite
   *  a client-side key. */
  const BUILDERS = [
    'src/lib/interfaceScope.ts', // the `scope_id` an interface-scoped threshold rule is stored under
    'src/dashboard/widgets/interfaceTraffic.ts', // `linkId` — a React key and a fetch dependency
  ];

  function tsFiles(dir: string, out: string[] = []): string[] {
    for (const e of readdirSync(dir, { withFileTypes: true })) {
      const p = join(dir, e.name);
      if (e.isDirectory()) tsFiles(p, out);
      else if (/\.tsx?$/.test(e.name)) out.push(p);
    }
    return out;
  }

  const SRC = join(__dirname, '..');
  const files = tsFiles(SRC);

  it('reads the sources it thinks it is reading', () => {
    // Without this a wrong path makes the assertion below vacuously true, which looks like success.
    expect(files.length).toBeGreaterThan(400);
  });

  it('finds no third spelling', () => {
    const offenders = files
      .filter((f) => COMPOSITE.test(readFileSync(f, 'utf8')))
      .map((f) => 'src/' + f.slice(SRC.length + 1).split(sep).join('/'))
      .sort();
    expect(offenders).toEqual([...BUILDERS].sort());
  });
});
