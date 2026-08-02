// SPDX-License-Identifier: AGPL-3.0-only
import { describe, it, expect } from 'vitest';
import {
  canHoldScope,
  isScoped,
  sameScope,
  scopeFromSelection,
  scopeGroupIds,
  scopeLabelKey,
} from './userScope';

const A = '11111111-1111-1111-1111-111111111111';
const B = '22222222-2222-2222-2222-222222222222';

describe('userScope', () => {
  it('never reads an empty group set as unrestricted', () => {
    // The two states this whole module exists to keep apart. Collapsing them in the widening
    // direction hands out the fleet; the other way hides it. Neither is detectable at a glance.
    expect(isScoped('All')).toBe(false);
    expect(isScoped({ Groups: [] })).toBe(true);
    expect(scopeLabelKey('All')).toEqual({ key: 'all', n: 0 });
    expect(scopeLabelKey({ Groups: [] })).toEqual({ key: 'none', n: 0 });
    expect(scopeLabelKey({ Groups: [A, B] })).toEqual({ key: 'groups', n: 2 });
  });

  it('turns an empty selection into "All", not into an empty group set', () => {
    // Ticking nothing in the picker means "the whole fleet". Sending `{Groups: []}` would be the
    // same gesture meaning "nothing", and the API would reject it — visibly, but for the wrong
    // reason, which is worse than either answer.
    expect(scopeFromSelection([])).toBe('All');
    expect(scopeFromSelection([A])).toEqual({ Groups: [A] });
  });

  it('deduplicates and orders a selection so the same choice produces the same body', () => {
    expect(scopeFromSelection([B, A, B])).toEqual({ Groups: [A, B] });
  });

  it('reads back the ids it was given', () => {
    expect(scopeGroupIds('All')).toEqual([]);
    expect(scopeGroupIds({ Groups: [A, B] })).toEqual([A, B]);
  });

  it('treats a re-ordered set as unchanged so a no-op save cannot revoke sessions', () => {
    // Saving calls PUT, and PUT revokes every session the account holds. An admin who opens the
    // dialog and closes it via Save must not sign that person out.
    expect(sameScope({ Groups: [A, B] }, { Groups: [B, A] })).toBe(true);
    expect(sameScope('All', 'All')).toBe(true);
    expect(sameScope('All', { Groups: [A] })).toBe(false);
    expect(sameScope({ Groups: [] }, 'All')).toBe(false);
    expect(sameScope({ Groups: [A] }, { Groups: [A, B] })).toBe(false);
  });

  it('refuses to offer a scope for an admin', () => {
    expect(canHoldScope('admin')).toBe(false);
    expect(canHoldScope('operator')).toBe(true);
    expect(canHoldScope('viewer')).toBe(true);
  });
});
