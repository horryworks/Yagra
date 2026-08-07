// SPDX-License-Identifier: AGPL-3.0-only
import { describe, expect, it } from 'vitest';
import {
  buildIncidentTimeline,
  signalLabel,
  signalTone,
  type TimelineSignal,
} from './IncidentTimeline';

const s = (at: number, kind: string, severity = 50): TimelineSignal => ({
  at,
  kind,
  label: `${kind}@${at}`,
  severity,
});

describe('signalTone', () => {
  it('uses the backend severity_for thresholds exactly', () => {
    // Boundaries matter: the backend buckets at >= 90 and >= 75, so the marker tone must agree with
    // the score the same finding was given.
    expect(signalTone(90)).toBe('crit');
    expect(signalTone(89.9)).toBe('warn');
    expect(signalTone(75)).toBe('warn');
    expect(signalTone(74.9)).toBe('info');
    expect(signalTone(0)).toBe('info');
  });
});

describe('buildIncidentTimeline', () => {
  it('returns null when there is nothing to draw', () => {
    expect(buildIncidentTimeline(undefined)).toBeNull();
    expect(buildIncidentTimeline([])).toBeNull();
    // A signal with a non-finite timestamp is not plottable.
    expect(buildIncidentTimeline([s(NaN, 'metric')])).toBeNull();
  });

  it('shows only the lanes the incident actually has, in canonical order', () => {
    const m = buildIncidentTimeline([s(100, 'flow'), s(50, 'metric')])!;
    // metric before flow regardless of input order; no empty `event` lane.
    expect(m.lanes.map((l) => l.lane)).toEqual(['metric', 'flow']);
  });

  it('never divides by a zero span when every signal shares a timestamp', () => {
    // The live sample had signals days apart, but a burst can land in the same second.
    const m = buildIncidentTimeline([s(1000, 'metric'), s(1000, 'event'), s(1000, 'flow')])!;
    for (const sig of m.signals) {
      expect(Number.isFinite(sig.x)).toBe(true);
      expect(Number.isFinite(sig.y)).toBe(true);
    }
    // Spread out rather than stacked on one pixel.
    const xs = m.signals.map((x) => x.x);
    expect(new Set(xs).size).toBeGreaterThan(1);
  });

  it('keeps an unknown kind instead of silently dropping the signal', () => {
    const m = buildIncidentTimeline([s(10, 'metric'), s(20, 'quantum-telepathy')])!;
    expect(m.signals).toHaveLength(2);
    expect(m.lanes.map((l) => l.lane)).toContain('other');
    expect(m.signals.find((x) => x.kind === 'quantum-telepathy')!.lane).toBe('other');
  });

  it('sorts input and reports the true window ends', () => {
    const m = buildIncidentTimeline([s(300, 'event'), s(100, 'event'), s(200, 'event')])!;
    expect(m.from).toBe(100);
    expect(m.to).toBe(300);
    // x is monotonic in time after sorting.
    const xs = m.signals.map((x) => x.x);
    expect(xs).toEqual([...xs].sort((a, b) => a - b));
  });

  it('nudges colliding markers apart so a burst stays countable', () => {
    // Four events within a 5-day span but seconds apart would otherwise overlap into one blob.
    const base = 1_700_000_000;
    const m = buildIncidentTimeline([
      s(base, 'event'),
      s(base + 1, 'event'),
      s(base + 2, 'event'),
      s(base + 432_000, 'event'),
    ])!;
    const xs = m.signals.map((x) => x.x);
    for (let i = 1; i < xs.length; i++) expect(xs[i] - xs[i - 1]).toBeGreaterThan(1);
    // Nudging must not push a marker past the right edge.
    expect(Math.max(...xs)).toBeLessThanOrEqual(m.plot.x + m.plot.w + 0.001);
  });

  it('sizes and tones markers by severity', () => {
    const m = buildIncidentTimeline([s(1, 'metric', 100), s(2, 'event', 50)])!;
    const [crit, info] = m.signals;
    expect(crit.tone).toBe('crit');
    expect(info.tone).toBe('info');
    expect(crit.r).toBeGreaterThan(info.r);
  });

  it('grows in height with the number of lanes', () => {
    const one = buildIncidentTimeline([s(1, 'metric')])!;
    const three = buildIncidentTimeline([s(1, 'metric'), s(2, 'event'), s(3, 'flow')])!;
    expect(three.h).toBeGreaterThan(one.h);
  });
});

describe('signalLabel', () => {
  // A finding written before the neighbour expansion (ADR-022 Increment 2) carries no node on any
  // timeline entry, and must read exactly as it did — the payload is additive, not versioned.
  it('leaves the subject own signals untouched', () => {
    expect(signalLabel({ label: 'icmp_rtt_ms spike' })).toBe('icmp_rtt_ms spike');
    expect(signalLabel({ label: 'icmp_rtt_ms spike', node_name: undefined })).toBe(
      'icmp_rtt_ms spike',
    );
  });

  // The misleading case this exists to prevent: an unattributed neighbour signal reads as the
  // subject own activity, so an operator sees another device traffic shift as this one.
  it('attributes a corroborating neighbour signal to its node', () => {
    expect(signalLabel({ label: 'linkDown', node_name: 'core-sw-01' })).toBe('core-sw-01: linkDown');
  });
});
