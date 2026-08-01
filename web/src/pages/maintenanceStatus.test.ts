// SPDX-License-Identifier: AGPL-3.0-only
// A maintenance window's status precedence. The disabled-but-in-window case is the one that
// matters: showing "active" there tells an operator their alerts are suppressed when the server
// is still paging them.

import { describe, expect, it } from 'vitest';
import { windowStatus } from './maintenanceStatus';
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
});
