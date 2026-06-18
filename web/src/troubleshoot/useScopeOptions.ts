// Real scope choices for the launch drawer / report config bar: "All nodes" plus every node
// group (so analyses run against actual inventory, not hardcoded labels).

import { useEffect, useState } from 'react';
import { api } from '../services/api';

export interface ScopeOption {
  kind: 'all' | 'group' | 'node';
  id: string | null;
  label: string;
}

const ALL: ScopeOption = { kind: 'all', id: null, label: 'All nodes' };

export function useScopeOptions(): ScopeOption[] {
  const [opts, setOpts] = useState<ScopeOption[]>([ALL]);
  useEffect(() => {
    api
      .listNodeGroups()
      .then((groups) => {
        setOpts([
          ALL,
          ...groups.map((g) => ({
            kind: 'group' as const,
            id: g.id,
            label: `group: ${g.name}`,
          })),
        ]);
      })
      .catch(() => {
        /* keep just "All nodes" if groups can't be loaded */
      });
  }, []);
  return opts;
}
