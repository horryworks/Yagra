// SPDX-License-Identifier: AGPL-3.0-only
// A maintenance window's status precedence. The disabled-but-in-window case is the one that
// matters: showing "active" there tells an operator their alerts are suppressed when the server
// is still paging them.

import { describe, expect, it } from 'vitest';
import { isEnded, windowStatus } from './maintenanceStatus';
import type { MaintenanceWindow } from '../types/api';

const NOW = 1_700_000_000_000;
const iso = (ms: number) => new Date(ms).toISOString();

const win = (over: Partial<MaintenanceWindow> = {}): MaintenanceWindow =>
  ({
    id: 'w1',
    name: 'patching',
    enabled: true,
    active: false,
    ends_at: iso(NOW + 3_600_000),
    ...over,
  }) as unknown as MaintenanceWindow;

describe('windowStatus', () => {
  it('reads a disabled window as disabled even while its clock says it is running', () => {
    // The server does not suppress alerts for a disabled window, so "active" would be a lie about
    // whether anyone is being paged.
    expect(windowStatus(win({ enabled: false, active: true }), NOW)).toEqual({
      labelKey: 'disabled',
      tone: 'neutral',
    });
  });

  it('trusts the server’s active flag over the local clock', () => {
    // `active` is computed server-side; a browser with a skewed clock must not override it.
    expect(windowStatus(win({ active: true, ends_at: iso(NOW - 1) }), NOW).labelKey).toBe('active');
  });

  it('separates a finished window from an upcoming one', () => {
    expect(windowStatus(win({ ends_at: iso(NOW - 1) }), NOW).labelKey).toBe('ended');
    expect(windowStatus(win({ ends_at: iso(NOW + 1) }), NOW).labelKey).toBe('scheduled');
  });

  it('tones only the active window as informational', () => {
    // Only a window actually suppressing alerts earns the coloured badge.
    expect(windowStatus(win({ active: true }), NOW).tone).toBe('info');
    for (const w of [win({ enabled: false }), win({ ends_at: iso(NOW - 1) }), win()]) {
      expect(windowStatus(w, NOW).tone).toBe('neutral');
    }
  });

  it('reads a disabled window whose end time has passed as ended', () => {
    // It used to read "disabled" forever, indistinguishable from a window waiting to be switched
    // on — and the bulk clear would then have offered to delete a row the badge called disabled.
    expect(windowStatus(win({ enabled: false, ends_at: iso(NOW - 1) }), NOW).labelKey).toBe('ended');
  });

  it('still reads a disabled window that has not ended as disabled', () => {
    // The precedence must not over-rotate: `ended` wins only once the clock has actually passed.
    expect(windowStatus(win({ enabled: false, ends_at: iso(NOW + 1) }), NOW).labelKey).toBe(
      'disabled',
    );
  });

  it('puts the ended boundary where the server puts it', () => {
    // The server computes `active` as `ends_at > now()` and deletes on `ends_at <= now()`, so a
    // window ending exactly now is ended on both sides. A `<` here would disagree by one tick.
    expect(windowStatus(win({ ends_at: iso(NOW) }), NOW).labelKey).toBe('ended');
  });
});

describe('isEnded', () => {
  it('never counts a window the server says is active', () => {
    // `active` is the server's statement that alerts are suppressed right now. A skewed browser
    // clock may under-count; it must never offer to delete a live suppression.
    expect(isEnded(win({ active: true, ends_at: iso(NOW - 3_600_000) }), NOW)).toBe(false);
  });

  it('is exactly the set the bulk clear removes', () => {
    // The button's count and the server's `ends_at <= now()` have to name the same rows, or the
    // confirmation promises a number the operator does not get.
    const rows = [
      win({ id: 'active', active: true, ends_at: iso(NOW + 60_000) }),
      win({ id: 'scheduled', ends_at: iso(NOW + 60_000) }),
      win({ id: 'ended', ends_at: iso(NOW - 60_000) }),
      win({ id: 'disabled-past', enabled: false, ends_at: iso(NOW - 60_000) }),
      win({ id: 'disabled-future', enabled: false, ends_at: iso(NOW + 60_000) }),
    ];
    expect(rows.filter((w) => isEnded(w, NOW)).map((w) => w.id)).toEqual([
      'ended',
      'disabled-past',
    ]);
  });
});
