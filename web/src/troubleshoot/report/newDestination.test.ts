// SPDX-License-Identifier: AGPL-3.0-only
import { describe, expect, it } from 'vitest';
import { classifyNewDestination, splitDestinations } from './newDestination';
import type { AnalysisFinding } from '../../types/api';

function f(metric: string, detail: Record<string, unknown>): AnalysisFinding {
  return {
    id: Math.random().toString(36).slice(2),
    score: 82,
    severity: 'warn',
    node_id: 'n1',
    node_name: 'edge-1',
    metric,
    kind: 'new_destination',
    when_label: 'AS15169',
    duration: 'GOOGLE',
    detail,
  };
}

describe('classifyNewDestination', () => {
  it('reads an AS row, keeping a resolved org name', () => {
    expect(classifyNewDestination(f('dst_as', { asn: 15169, as_name: 'GOOGLE', bytes: 900000 }))).toEqual(
      { kind: 'as', asn: 15169, name: 'GOOGLE', bytes: 900000 },
    );
  });

  it('reads an AS row with no resolved name (IP→ASN table absent or missing the org)', () => {
    expect(classifyNewDestination(f('dst_as', { asn: 64999, bytes: 700000 }))).toEqual({
      kind: 'as',
      asn: 64999,
      name: null,
      bytes: 700000,
    });
  });

  it('reads a port row', () => {
    expect(classifyNewDestination(f('dst_port', { port: 443, bytes: 600000 }))).toEqual({
      kind: 'port',
      port: 443,
      bytes: 600000,
    });
  });

  it('rejects ASN 0 — the backend sentinel for unknown, not a real destination', () => {
    expect(classifyNewDestination(f('dst_as', { asn: 0, bytes: 900000 }))).toBeNull();
  });

  it('rejects rows missing the field their shape needs', () => {
    expect(classifyNewDestination(f('dst_as', { bytes: 900000 }))).toBeNull(); // no asn
    expect(classifyNewDestination(f('dst_port', { bytes: 900000 }))).toBeNull(); // no port
    expect(classifyNewDestination(f('dst_as', { asn: 15169 }))).toBeNull(); // no bytes
  });

  it('degrades to null on an unrecognised shape rather than guessing', () => {
    // If a future increment adds a third destination kind, existing clients skip it safely.
    expect(classifyNewDestination(f('dst_country', { country: 'JP', bytes: 1 }))).toBeNull();
  });
});

describe('splitDestinations', () => {
  it('separates the two shapes and sorts each by bytes, dropping unclassifiable rows', () => {
    const rows = [
      f('dst_as', { asn: 15169, as_name: 'GOOGLE', bytes: 500 }),
      f('dst_port', { port: 443, bytes: 900 }),
      f('dst_as', { asn: 13335, bytes: 1500 }),
      f('dst_port', { port: 53, bytes: 100 }),
      f('dst_as', { asn: 0, bytes: 9999 }), // unknown AS — dropped
      f('junk', { bytes: 1 }), // unrecognised — dropped
    ];
    const { as, ports } = splitDestinations(rows);
    expect(as.map((x) => x.dest.asn)).toEqual([13335, 15169]);
    expect(ports.map((x) => (x.dest as { port: number }).port)).toEqual([443, 53]);
  });

  it('handles an empty input', () => {
    expect(splitDestinations([])).toEqual({ as: [], ports: [] });
  });
});
