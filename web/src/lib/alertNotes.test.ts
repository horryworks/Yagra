// SPDX-License-Identifier: AGPL-3.0-only
import { describe, expect, it } from 'vitest';
import { alertNotes, noteKey, type NotedAlert } from './alertNotes';

const NODE = '6f1c9d2a-0b3e-4a71-9c8d-2e5f7a1b4c60';
const UPSTREAM = '0a8e3b71-5d2c-4f69-8b10-7c4d9e2f1a35';

const alert = (over: Partial<NotedAlert> = {}): NotedAlert => ({
  node: NODE,
  subject_kind: 'node',
  ...over,
});

describe('alertNotes', () => {
  it('says nothing about a plain alert', () => {
    expect(alertNotes(alert(), null)).toEqual([]);
  });

  it('names the upstream node when another node’s outage is the cause', () => {
    expect(alertNotes(alert({ root_cause: UPSTREAM }), null)).toEqual([
      { kind: 'upstream', nodeId: UPSTREAM },
    ]);
  });

  it('says "part of this node’s outage" rather than pointing the row at itself', () => {
    // ADR-087 lets the cause be the alert's own node; an arrow to itself would read `X ← X`.
    expect(alertNotes(alert({ root_cause: NODE }), null)).toEqual([{ kind: 'self' }]);
  });

  it('carries the delivery-hours hold with its release time, or none', () => {
    expect(alertNotes(alert(), { release_at: '2026-11-07T10:00:00Z' })).toEqual([
      { kind: 'held', releaseAt: '2026-11-07T10:00:00Z' },
    ]);
    expect(alertNotes(alert(), {})).toEqual([{ kind: 'held', releaseAt: null }]);
  });

  it('stacks every note that applies, cause first and holds last', () => {
    const notes = alertNotes(
      alert({ root_cause: UPSTREAM, flapping: true, notify_held_until: 1_700_000_000_000 }),
      { release_at: null },
    );
    expect(notes.map((n) => n.kind)).toEqual(['upstream', 'flapping', 'waitingNeighbour', 'held']);
  });

  it('treats a neighbour hold of zero as a hold, not as absent', () => {
    expect(alertNotes(alert({ notify_held_until: 0 }), null)).toEqual([
      { kind: 'waitingNeighbour' },
    ]);
  });

  it('gives each note in a row its own key', () => {
    const notes = alertNotes(
      alert({ root_cause: UPSTREAM, flapping: true, notify_held_until: 1 }),
      { release_at: null },
    );
    expect(new Set(notes.map(noteKey)).size).toBe(notes.length);
  });
});
