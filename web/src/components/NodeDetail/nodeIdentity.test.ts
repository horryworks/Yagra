// SPDX-License-Identifier: AGPL-3.0-only
import { describe, it, expect } from 'vitest';
import type { TFunction } from 'i18next';
import { nodeSubLineParts } from './nodeIdentity';
import { NODE_KINDS, type NodeDetail, type NodeKind } from '../../types/api';
import enNodes from '../../locales/en/nodes.json';

// Resolve a `nodes` key against the bundled English strings, the way `profileCategories.test.ts`
// does — enough to assert on rendered text without booting i18next.
const t = ((key: string): string => {
  let cur: unknown = enNodes;
  for (const seg of key.split('.')) cur = (cur as Record<string, unknown> | undefined)?.[seg];
  return typeof cur === 'string' ? cur : key;
}) as unknown as TFunction;

const node = (over: Partial<NodeDetail> & { kind: NodeKind }): NodeDetail =>
  ({
    id: '00000000-0000-0000-0000-000000000001',
    name: 'n',
    address: '0.0.0.0',
    ...over,
  }) as NodeDetail;

const text = (n: NodeDetail) =>
  nodeSubLineParts(n, t)
    .map((p) => p.text)
    .join(' · ');

describe('node header sub line', () => {
  // The actual regression: a DNS monitor using the system resolver has address 0.0.0.0 and no
  // vendor, so the line read `0.0.0.0 · unknown device` — two facts that are not just missing but
  // meaningless for the kind, while the name it resolves appeared nowhere on it.
  it('never shows a DNS monitor a meaningless address and vendor', () => {
    const line = text(
      node({
        kind: 'dns',
        dns_check: { name: 'wg.horryworks.net', record_type: 'A' },
      } as Partial<NodeDetail> & { kind: NodeKind }),
    );
    expect(line).toBe('wg.horryworks.net · A');
    expect(line).not.toContain('0.0.0.0');
    expect(line).not.toContain(enNodes.detail.unknownDevice);
  });

  it('leads a URL monitor with its URL, not the host it happened to resolve to', () => {
    const parts = nodeSubLineParts(
      node({
        kind: 'url',
        address: '93.184.216.34',
        url_check: { url: 'https://status.example.com/health' },
      } as Partial<NodeDetail> & { kind: NodeKind }),
      t,
    );
    expect(parts.map((p) => p.text)).toEqual(['https://status.example.com/health']);
    expect(parts[0].mono).toBe(true);
  });

  it('leaves the device line unchanged', () => {
    expect(text(node({ kind: 'device', address: '10.0.0.1', vendor: 'Cisco', model: 'C2960' }))).toBe(
      '10.0.0.1 · Cisco C2960',
    );
    expect(text(node({ kind: 'device', address: '10.0.0.1' }))).toBe('10.0.0.1 · unknown device');
    // A Meraki node has a real management address and an org-reported maker/model.
    expect(text(node({ kind: 'meraki', address: '10.0.0.2', vendor: 'Cisco Meraki' }))).toBe(
      '10.0.0.2 · Cisco Meraki',
    );
  });

  // A monitor whose config row failed to load still has to say something, and the address is the
  // only fact left. Also guards the switch staying total over NodeKind.
  it('gives every kind a non-empty line, with distinct part ids', () => {
    for (const kind of NODE_KINDS) {
      const parts = nodeSubLineParts(node({ kind }), t);
      expect(parts.length, kind).toBeGreaterThan(0);
      expect(new Set(parts.map((p) => p.id)).size, kind).toBe(parts.length);
      for (const p of parts) expect(p.text, `${kind}/${p.id}`).not.toBe('');
    }
  });
});
