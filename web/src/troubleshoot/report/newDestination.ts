// SPDX-License-Identifier: AGPL-3.0-only
// `new_destination` is the one analysis that emits TWO different row shapes in one job — a new
// destination AS and a new destination port — distinguished by `metric`. They share no field but
// bytes, and the backend caps port findings at score 74 so a port row can never reach `warn`, which
// is why the report splits them into separate sections instead of mixing them in one list (a shared
// severity filter across the two would be meaningless).
//
// Pure + tested: a malformed or future third shape returns `null` so the body skips that row rather
// than rendering garbage or crashing.

import type { AnalysisFinding } from '../../types/api';
import { detailNum, detailStr } from './format';

export interface NewAsDestination {
  kind: 'as';
  asn: number;
  /** Organization name when the IP→ASN table resolved it. */
  name: string | null;
  bytes: number;
}

export interface NewPortDestination {
  kind: 'port';
  port: number;
  bytes: number;
}

export type NewDestination = NewAsDestination | NewPortDestination;

/**
 * Classify a `new_destination` finding by its `metric` discriminator, validating the fields that
 * shape actually needs. Returns `null` for anything unrecognised or incomplete.
 */
export function classifyNewDestination(f: AnalysisFinding): NewDestination | null {
  const bytes = detailNum(f, 'bytes');
  if (bytes === undefined) return null;

  if (f.metric === 'dst_as') {
    const asn = detailNum(f, 'asn');
    // ASN 0 is the backend's "unknown" sentinel — not a destination worth naming.
    if (asn === undefined || asn === 0) return null;
    return { kind: 'as', asn, name: detailStr(f, 'as_name') ?? null, bytes };
  }

  if (f.metric === 'dst_port') {
    const port = detailNum(f, 'port');
    if (port === undefined) return null;
    return { kind: 'port', port, bytes };
  }

  return null;
}

/** Split findings into the two sections, dropping anything unclassifiable. */
export function splitDestinations(findings: AnalysisFinding[]): {
  as: { finding: AnalysisFinding; dest: NewAsDestination }[];
  ports: { finding: AnalysisFinding; dest: NewPortDestination }[];
} {
  const as: { finding: AnalysisFinding; dest: NewAsDestination }[] = [];
  const ports: { finding: AnalysisFinding; dest: NewPortDestination }[] = [];
  for (const finding of findings) {
    const dest = classifyNewDestination(finding);
    if (!dest) continue;
    if (dest.kind === 'as') as.push({ finding, dest });
    else ports.push({ finding, dest });
  }
  as.sort((a, b) => b.dest.bytes - a.dest.bytes);
  ports.sort((a, b) => b.dest.bytes - a.dest.bytes);
  return { as, ports };
}
