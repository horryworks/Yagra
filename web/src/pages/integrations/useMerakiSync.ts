// SPDX-License-Identifier: AGPL-3.0-only
// "Sync now" for one Meraki organization: the request, its busy flag and its failure (ADR-164).
//
// A hook because two screens press the same button — the organization's row on the Meraki page and
// the organization's own page. A second copy of this would drift on the part that is easy to get
// wrong: what happens after the request, whichever way it went.
//
// Since ADR-164 決定 32 the request only asks: the server answers 202 at once and the read it asked
// for runs in the background for minutes, shown by the organization's `full_sync`
// (`orgFullRead`, `useSyncWatch`). `busy` covers the request, not the read.

import { useCallback, useState } from 'react';
import { api, errMsg } from '../../services/api';

export interface MerakiSync {
  busy: boolean;
  /** The request's own failure (a 409 for a paused organization or while Meraki polling is off). */
  error: string | null;
  run: () => void;
}

/**
 * @param onSynced called when the request settles, **whichever way** — on success the organization
 *   now carries the request (`full_sync`), and on failure the reload shows what the row says.
 */
export function useMerakiSync(
  orgId: string,
  errorFallback: string,
  onSynced: () => void,
): MerakiSync {
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const run = useCallback(() => {
    setBusy(true);
    setError(null);
    api
      .syncMerakiOrg(orgId)
      .catch((e: unknown) => setError(errMsg(e, errorFallback)))
      .finally(() => {
        setBusy(false);
        onSynced();
      });
  }, [orgId, errorFallback, onSynced]);

  return { busy, error, run };
}
