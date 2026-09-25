// SPDX-License-Identifier: AGPL-3.0-only
// Keep an integration page current while a sync it asked for runs in the background.
//
// "Sync now" answers at once on both integrations — Meraki since ADR-164 決定 32, NetBox since
// ADR-172 決定 1 — and the run it asks for happens in the leader's loop. A page that loaded once
// and after an action would show "requested" until a reload. Shared by the three pages that show
// it (the Meraki list, one Meraki organization, NetBox), because three copies of "when does the
// page stop re-reading" would drift. It was `useMerakiReadWatch` until NetBox needed it too.

import { useEffect, useRef } from 'react';

/** How often a page re-reads while a sync is asked for or running, so the state moves without a
 *  reload. A Meraki read of 350 networks takes three to six minutes; a NetBox sync, seconds. */
export const SYNC_WATCH_POLL_MS = 5_000;

/**
 * While `active`, call `poll` every {@link SYNC_WATCH_POLL_MS}; when it stops being active, call
 * `settled` once — the run has ended and what it wrote is worth a full reload.
 *
 * @param active whether anything on the page has a sync asked for or running.
 * @param poll the cheap re-read that moves the state.
 * @param settled the reload after the run, which may have written hundreds of rows.
 */
export function useSyncWatch(active: boolean, poll: () => void, settled: () => void): void {
  const wasActive = useRef(active);
  useEffect(() => {
    if (wasActive.current && !active) settled();
    wasActive.current = active;
  }, [active, settled]);

  useEffect(() => {
    if (!active) return undefined;
    const timer = window.setInterval(poll, SYNC_WATCH_POLL_MS);
    return () => window.clearInterval(timer);
  }, [active, poll]);
}
