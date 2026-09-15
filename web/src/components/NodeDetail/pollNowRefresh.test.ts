// SPDX-License-Identifier: AGPL-3.0-only
import { describe, expect, it } from 'vitest';
import { POLL_NOW_REFRESH_MS } from './pollNowRefresh';

// How long the poller's row-name walk may run — `NAME_WALK_BUDGET` in
// `crates/yagra-poller/src/worker/row_names.rs`. Written again here, and nothing compares the two:
// raising that budget past the last read below means changing this number too.
const ROW_NAME_WALK_BUDGET_MS = 20_000;

describe('POLL_NOW_REFRESH_MS', () => {
  it('re-reads more than once, in ascending order', () => {
    expect(POLL_NOW_REFRESH_MS.length).toBeGreaterThan(1);
    const sorted = [...POLL_NOW_REFRESH_MS].sort((a, b) => a - b);
    expect([...POLL_NOW_REFRESH_MS]).toEqual(sorted);
  });

  it('keeps the first read quick, for what a poll answers within seconds', () => {
    expect(POLL_NOW_REFRESH_MS[0]).toBeLessThanOrEqual(5_000);
  });

  // ADR-149: the press also walks the row names, after the table job and for up to the walk's budget.
  // A last read at or before that budget would re-read a page the names cannot have reached yet.
  it("waits past the poller's row-name walk before its last read", () => {
    const last = POLL_NOW_REFRESH_MS[POLL_NOW_REFRESH_MS.length - 1];
    expect(last).toBeGreaterThan(ROW_NAME_WALK_BUDGET_MS);
  });
});
