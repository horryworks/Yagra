// SPDX-License-Identifier: AGPL-3.0-only
// The Disk card headline's two branches.
//
// The capacity-unknown branch is the one that matters: the PostgreSQL `database` proxy reports its
// own size and no capacity, so any code path that divides by it produces Infinity/NaN — or, worse,
// a plausible-looking 0% that reads as "this store is empty".

import { describe, expect, it } from 'vitest';
import { diskHeadline } from './diskHeadline';
import type { DiskUsage } from '../types/api';

const GiB = 1024 * 1024 * 1024;
const disk = (mount: string, used: number, size: number): DiskUsage => ({
  mount,
  used_bytes: used,
  size_bytes: size,
});

describe('a filesystem with a known capacity', () => {
  it('reads as used / size · percentage', () => {
    expect(diskHeadline(disk('root', 10 * GiB, 20 * GiB))).toBe('10 GB / 20 GB · 50%');
  });

  it('shows the extremes without rounding them away', () => {
    expect(diskHeadline(disk('root', 0, 20 * GiB))).toBe('0 B / 20 GB · 0%');
    expect(diskHeadline(disk('root', 20 * GiB, 20 * GiB))).toBe('20 GB / 20 GB · 100%');
  });

  it('keeps a decimal place only for a small fraction', () => {
    // `formatUtil`'s rule: whole numbers and anything ≥ 10 show no decimal, so a nearly-empty
    // volume still reads as more than "0%".
    expect(diskHeadline(disk('metrics', 1 * GiB, 3 * GiB))).toBe('1 GB / 3 GB · 33%');
    expect(diskHeadline(disk('metrics', 1 * GiB, 30 * GiB))).toBe('1 GB / 30 GB · 3.3%');
  });
});

describe('a store with no measurable capacity', () => {
  it('shows the bare size rather than a percentage of zero', () => {
    // `size_bytes == 0` is the PostgreSQL `database` proxy. Dividing here would yield Infinity, and
    // treating the missing capacity as "full" or "empty" would both be inventions.
    expect(diskHeadline(disk('database', 3.5 * GiB, 0))).toBe('3.5 GB');
    expect(diskHeadline(disk('database', 0, 0))).toBe('0 B');
  });
});

describe('an instance with no reading for the mount', () => {
  it('renders an em dash', () => {
    // The chart has history for a filesystem the currently-selected host no longer reports.
    expect(diskHeadline(undefined)).toBe('—');
  });
});
