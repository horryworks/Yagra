// SPDX-License-Identifier: AGPL-3.0-only
import { describe, it, expect } from 'vitest';
import { buildSankey, SANKEY_MAX_LINKS } from './FlowSankey';
import type { FlowConversation } from '../../types/api';

const convo = (src: string, dst: string, bytes: number): FlowConversation => ({
  src,
  dst,
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

  it('caps the number of ribbons drawn to keep the diagram legible', () => {
    const many = Array.from({ length: 30 }, (_, i) =>
      convo(`10.0.0.${i}`, `9.9.9.${i}`, 100 + i),
    );
    const model = buildSankey(many);
    expect(model!.bands.length).toBeLessThanOrEqual(SANKEY_MAX_LINKS);
  });
});
