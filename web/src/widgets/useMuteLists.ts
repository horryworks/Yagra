// SPDX-License-Identifier: AGPL-3.0-only
// The mutes and the mute releases in force, for the "muted until" mark on an active alert's row
// (ADR-209 decision 3). A mute stops notifications and leaves the alert open, so without the mark a
// silenced row and a paging one looked the same. Re-read on any configuration change (a mute
// written anywhere reaches every open list) and once more when the next one runs out.

import { useCallback, useEffect, useState } from 'react';
import { api } from '../services/api';
import { useOnConfigChange } from '../lib/configChanges';
import { nextSuppressionExpiry, suppressionRefreshDelayMs } from '../lib/suppression';
import type { Mute, SuppressionExemption } from '../types/api';

export function useMuteLists(): { mutes: Mute[]; exemptions: SuppressionExemption[] } {
  const [mutes, setMutes] = useState<Mute[]>([]);
  const [exemptions, setExemptions] = useState<SuppressionExemption[]>([]);
  // A refusal (a viewer of the public board, a core that is down) leaves the lists as they were:
  // the mark is a convenience, and an alert drawn without it is still drawn.
  const reload = useCallback(() => {
    api.listMutes().then(setMutes).catch(() => undefined);
    api.listSuppressionExemptions().then(setExemptions).catch(() => undefined);
  }, []);
  useEffect(() => {
    reload();
  }, [reload]);
  useOnConfigChange(reload);
  useEffect(() => {
    const at = nextSuppressionExpiry([], mutes, exemptions);
    if (at === null) return undefined;
    const h = setTimeout(reload, suppressionRefreshDelayMs(at));
    return () => clearTimeout(h);
  }, [mutes, exemptions, reload]);
  return { mutes, exemptions };
}
