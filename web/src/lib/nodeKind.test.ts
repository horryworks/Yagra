// SPDX-License-Identifier: AGPL-3.0-only
import { describe, it, expect } from 'vitest';
import { NODE_KIND_SPEC } from './nodeKind';
import { NODE_KINDS } from '../types/api';

describe('NODE_KIND_SPEC', () => {
  it('covers exactly the backend node kinds', () => {
    expect(Object.keys(NODE_KIND_SPEC).sort()).toEqual([...NODE_KINDS].sort());
  });

  // The unmarked default is the point: an inventory tree is overwhelmingly ordinary devices, and a
  // badge on every one of 50k rows would carry no information. A badge means "not a normal device".
  it('leaves the ordinary device unbadged and badges every other kind', () => {
    for (const kind of NODE_KINDS) {
      const badge = NODE_KIND_SPEC[kind].badge;
      if (kind === 'device') expect(badge).toBeNull();
      else expect(badge, kind).toBeTruthy();
    }
  });

  it('gives each kind a distinct badge and a distinct label key', () => {
    const badges = NODE_KINDS.map((k) => NODE_KIND_SPEC[k].badge).filter((b) => b !== null);
    expect(new Set(badges).size).toBe(badges.length);
    const labels = NODE_KINDS.map((k) => NODE_KIND_SPEC[k].labelKey);
    expect(new Set(labels).size).toBe(labels.length);
    for (const k of labels) expect(k).toMatch(/^kind\./);
  });

  // The tree row is a single 30px flex line (`ROW_H` in NodeTree.tsx) that already carries a status
  // dot, the name and up to two suppression marks. A long badge pushes the marks out of view.
  it('keeps badges short enough for a tree row', () => {
    for (const kind of NODE_KINDS) {
      const badge = NODE_KIND_SPEC[kind].badge;
      if (badge) expect(badge.length, kind).toBeLessThanOrEqual(8);
    }
  });

  // Each kind is polled over a different protocol, so no single metric answers "did we hear from
  // it". Asking every kind for icmp_rtt_ms is what left three of the four with no "seen" line.
  it('gives each kind its own liveness metric', () => {
    const metrics = NODE_KINDS.map((k) => NODE_KIND_SPEC[k].livenessMetric);
    expect(new Set(metrics).size).toBe(metrics.length);
    for (const m of metrics) expect(m).toMatch(/^[a-z][a-z0-9_]*$/);
    expect(NODE_KIND_SPEC.device.livenessMetric).toBe('icmp_rtt_ms');
    expect(NODE_KIND_SPEC.url.livenessMetric).toBe('http_up');
    expect(NODE_KIND_SPEC.dns.livenessMetric).toBe('dns_up');
    expect(NODE_KIND_SPEC.meraki.livenessMetric).toBe('meraki_device_up');
  });
});
