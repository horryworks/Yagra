// SPDX-License-Identifier: AGPL-3.0-only
// Data for the scope picker's Group mode: node groups (loaded eagerly — cheap, bounded). The Node
// mode does NOT load the inventory into the browser — it queries the server search endpoint per
// keystroke (see ScopePicker), so this no longer full-loads up to N nodes client-side (was S/ADR-022;
// the dedicated /nodes/search endpoint now exists and NodePicker already uses it).

import { useEffect, useState } from 'react';
import { api } from '../../services/api';
import type { NodeGroup } from '../../types/api';

export interface ScopeData {
  groups: NodeGroup[];
}

export function useScopeData(): ScopeData {
  const [groups, setGroups] = useState<NodeGroup[]>([]);

  useEffect(() => {
    api
      .listNodeGroups()
      .then(setGroups)
      .catch(() => {
        /* groups unavailable — All + Node modes still work */
      });
  }, []);

  return { groups };
}
