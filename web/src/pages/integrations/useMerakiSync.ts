// SPDX-License-Identifier: AGPL-3.0-only
// "Sync now" for one Meraki organization: the request, its busy flag and its failure (ADR-164).
//
// A hook because two screens press the same button — the organization's row on the Meraki page and
// the organization's own page — and a sync is the one write here that takes seconds. A second copy
// of this would drift on the part that is easy to get wrong: what happens after a sync that *ran
// and failed*.

import { useCallback, useState } from 'react';
import { api, errMsg } from '../../services/api';

export interface MerakiSync {
  busy: boolean;
  /** The request's own failure (a 409 for a paused organization, a 502 for a sync that failed). */
  error: string | null;
  run: () => void;
}

/**
 * @param onSynced called when the request settles, **whichever way**. A sync that ran and failed
 *   has written its reason on the organization's row, and that reason is the useful half of the
 *   answer — so the caller reloads on failure too.
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
