// SPDX-License-Identifier: AGPL-3.0-only
// The notifications held outside their node's delivery hours, for the "held until" mark on an
// active alert's row (ADR-226 decision 11). Without it a row nobody is being paged about looks the
// same as one that paged. Held rows are written by the notifier, not by an API call, so no
// configuration change announces them: re-read on the shared visible-tab cadence instead.

import { useEffect, useState } from 'react';
import { api } from '../services/api';
import { POLL_INTERVAL_MS, pollWhileVisible } from '../lib/sharedPoll';
import type { HeldNotification } from '../types/api';

export function useHeldNotifications(): HeldNotification[] {
  const [held, setHeld] = useState<HeldNotification[]>([]);
  useEffect(() => {
    let alive = true;
    // A refusal (a viewer of the public board, a core that is down) leaves the list as it was: the
    // mark is a convenience, and an alert drawn without it is still drawn.
    const read = () => {
      api
        .listHeldNotifications()
        .then((h) => {
          if (alive) setHeld(h);
        })
        .catch(() => undefined);
    };
    read();
    const stop = pollWhileVisible(read, POLL_INTERVAL_MS);
    return () => {
      alive = false;
      stop();
    };
  }, []);
  return held;
}
