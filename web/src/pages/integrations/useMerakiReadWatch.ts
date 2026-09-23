// SPDX-License-Identifier: AGPL-3.0-only
// Keep a Meraki page current while an organization is being read (ADR-164 決定 32).
//
// "Sync now" answers at once and the read it asks for runs in the background for minutes, as does
// an organization's first read. Neither page polled before — both loaded once and after an action —
// so the progress would stand still until a reload. Shared by the two pages that show it, for the
// reason `useMerakiSync` is: two copies of "when does the page stop re-reading" would drift.

import { useEffect, useRef } from 'react';
import { MERAKI_READ_POLL_MS } from './merakiOrgRow';

/**
 * While `active`, call `poll` every {@link MERAKI_READ_POLL_MS}; when it stops being active, call
 * `settled` once — the read has ended and what it imported is worth a full reload.
 *
 * @param active whether any organization on the page has a read asked for or running
 *   (`orgFullRead(…).kind !== 'none'`).
 * @param poll the cheap re-read that moves the progress: the organizations, not their devices.
 * @param settled the reload after the read, which may have imported hundreds of devices.
 */
export function useMerakiReadWatch(active: boolean, poll: () => void, settled: () => void): void {
  const wasActive = useRef(active);
  useEffect(() => {
    if (wasActive.current && !active) settled();
    wasActive.current = active;
  }, [active, settled]);

  useEffect(() => {
    if (!active) return undefined;
    const timer = window.setInterval(poll, MERAKI_READ_POLL_MS);
    return () => window.clearInterval(timer);
  }, [active, poll]);
}
