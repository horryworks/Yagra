// SPDX-License-Identifier: AGPL-3.0-only
import { describe, it, expect } from 'vitest';
import { buildSankey, SANKEY_MAX_LINKS, MIN_NODE_H } from './FlowSankey';
import type { FlowConversation } from '../../types/api';

const convo = (
  src: string,
  dst: string,
  bytes: number,
  as?: { srcAsn?: number; srcName?: string; dstAsn?: number; dstName?: string },
): FlowConversation => ({
  src,
  dst,
  src_asn: as?.srcAsn,
  dst_asn: as?.dstAsn,
  src_as_name: as?.srcName,
  dst_as_name: as?.dstName,
  bytes,
  packets: Math.round(bytes / 100),
  flows: 1,
});

describe('buildSankey', () => {
  it('returns null when there is nothing to draw', () => {
    expect(buildSankey([])).toBeNull();
    // A zero-byte conversation contributes nothing.
    expect(buildSankey([convo('10.0.0.1', '8.8.8.8', 0)])).toBeNull();
  });

  it('produces one ribbon per conversation, carrying its endpoints and bytes', () => {
    const model = buildSankey([
      convo('10.0.0.1', '8.8.8.8', 1000),
      convo('10.0.0.2', '1.1.1.1', 500),
    ]);
    expect(model).not.toBeNull();
    expect(model!.bands).toHaveLength(2);
    const google = model!.bands.find((b) => b.dst === '8.8.8.8')!;
    expect(google.src).toBe('10.0.0.1');
    expect(google.bytes).toBe(1000);
    expect(google.path.startsWith('M ')).toBe(true);
    // Two distinct sources + two distinct destinations = four column nodes.
    expect(model!.nodes).toHaveLength(4);
  });

  it('carries each host AS onto its column node, omitting it when unknown', () => {
    const model = buildSankey([
      convo('10.0.0.1', '17.248.221.6', 1000, { dstAsn: 15169, dstName: 'GOOGLE' }),
    ]);
    const dst = model!.nodes.find((n) => n.label === '17.248.221.6')!;
    expect(dst.sub).toBe('AS15169 · GOOGLE');
    // The internal source has no AS ⇒ no sub-line.
    const src = model!.nodes.find((n) => n.label === '10.0.0.1')!;
    expect(src.sub).toBeUndefined();
  });

  it('gives every endpoint a non-overlapping label slot, even for byte-tiny flows', () => {
    // One dominant flow plus several near-zero ones from the same source — the classic case where
    // strict byte-proportional heights would crush the small destinations' labels together.
    const model = buildSankey([
      convo('10.0.0.1', '8.8.8.8', 1_000_000),
      convo('10.0.0.1', '1.1.1.1', 20),
      convo('10.0.0.1', '9.9.9.9', 15),
      convo('10.0.0.1', '4.4.4.4', 10),
      convo('10.0.0.1', '2.2.2.2', 5),
    ]);
    expect(model).not.toBeNull();

    // Destination labels sit at y + h/2 (the slot centre). Sorted, consecutive centres must stay at
    // least MIN_NODE_H apart so the two-line labels never overlap.
    const dstCentres = model!.nodes
      .filter((n) => n.anchor === 'start')
      .map((n) => n.y + n.h / 2)
      .sort((a, b) => a - b);
    for (let i = 1; i < dstCentres.length; i += 1) {
      expect(dstCentres[i] - dstCentres[i - 1]).toBeGreaterThanOrEqual(MIN_NODE_H);
    }

    // The dominant flow's bar is still visibly larger than a tiny one (proportionality preserved).
    const big = model!.nodes.find((n) => n.label === '8.8.8.8')!;
    const small = model!.nodes.find((n) => n.label === '2.2.2.2')!;
    expect(big.h).toBeGreaterThan(small.h);
  });

  it('caps the number of ribbons drawn to keep the diagram legible', () => {
    const many = Array.from({ length: 30 }, (_, i) =>
      convo(`10.0.0.${i}`, `9.9.9.${i}`, 100 + i),
    );
    const model = buildSankey(many);
    expect(model!.bands.length).toBeLessThanOrEqual(SANKEY_MAX_LINKS);
  });
});
