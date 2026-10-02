// SPDX-License-Identifier: AGPL-3.0-only
// Judgement behind the "send a test notification" dialog (ADR-192).
//
// In a `.ts` rather than the dialog because Vitest never runs a `.tsx` (testing.md). The dialog
// keeps the layout; what to warn about before sending and how to read the answer live here.

import type { ChannelKind, ChannelTestResult, NotificationChannel } from '../types/api';

/**
 * The kinds whose test opens a real incident and then closes it again, so the on-call is paged
 * once. A copy of the server's `test_close_delay` — `notify.rs` reads this line and fails when the
 * two disagree, so keep the array on ONE line.
 */
export const INCIDENT_KINDS = ['pagerduty', 'jsm'] as const satisfies readonly ChannelKind[];

/** What the dialog says before anything is sent. */
export interface TestWarnings {
  /** A test pages someone (PagerDuty, JSM). */
  pages: boolean;
  /** The body comes from the operator's template, so the payload carries no `"test"` flag. */
  unmarkedBody: boolean;
  /** Real alerts will not use this channel until it is enabled; the test is sent anyway. */
  disabled: boolean;
}

export function testWarnings(channel: NotificationChannel): TestWarnings {
  return {
    pages: (INCIDENT_KINDS as readonly ChannelKind[]).includes(channel.kind),
    unmarkedBody: (channel.body_template ?? '').trim() !== '',
    disabled: !channel.enabled,
  };
}

/** How a finished test reads. */
export type TestVerdict = 'delivered' | 'deliveredAndClosed' | 'deliveredButOpen' | 'failed';

export function testVerdict(r: ChannelTestResult): TestVerdict {
  if (!r.delivered) return 'failed';
  if (r.closed === true) return 'deliveredAndClosed';
  if (r.closed === false) return 'deliveredButOpen';
  return 'delivered';
}

/** The sentence for each verdict. Full keys, so the coverage test can look each one up. */
export const VERDICT_KEYS: Record<TestVerdict, string> = {
  delivered: 'routing.test.result.delivered',
  deliveredAndClosed: 'routing.test.result.deliveredAndClosed',
  deliveredButOpen: 'routing.test.result.deliveredButOpen',
  failed: 'routing.test.result.failed',
};

/** Whether the verdict is good news — decides the line's colour, never its wording. */
export function verdictOk(v: TestVerdict): boolean {
  return v === 'delivered' || v === 'deliveredAndClosed';
}
