// Data for the scope picker: node groups (loaded eagerly — cheap, needed for Group mode) and the
// node inventory (loaded LAZILY only when the user first opens Node mode, so the drawer doesn't
// pull the whole inventory on every open). Nodes load via the keyset-paginated loop (capped),
// the same approach NodesPage uses — `api.listNodes()` returns only the first page.

import { useCallback, useEffect, useRef, useState } from 'react';
import { api } from '../services/api';
import type { NodeGroup, NodeSummary } from '../types/api';

const PAGE = 500;
const NODE_CAP = 5000;

/** Load up to NODE_CAP nodes by following the keyset cursor (matches NodesPage's loader). */
async function loadAllNodes(): Promise<NodeSummary[]> {
  const out: NodeSummary[] = [];
  let cursor: string | undefined;
  for (let i = 0; i < NODE_CAP / PAGE; i++) {
    const page = await api.listNodesPage({ cursor, limit: PAGE });
    out.push(...page.nodes);
    if (!page.next_cursor) break;
    cursor = page.next_cursor;
  }
  return out;
}

export interface ScopeData {
  groups: NodeGroup[];
  nodes: NodeSummary[];
  nodesLoaded: boolean;
  /** Kick off the (one-time) node load — called when Node mode first opens. */
  loadNodes: () => void;
}

export function useScopeData(): ScopeData {
  const [groups, setGroups] = useState<NodeGroup[]>([]);
  const [nodes, setNodes] = useState<NodeSummary[]>([]);
  const [nodesLoaded, setNodesLoaded] = useState(false);
  const loading = useRef(false);

  useEffect(() => {
    api
      .listNodeGroups()
      .then(setGroups)
      .catch(() => {
        /* groups unavailable — All + Node modes still work */
      });
  }, []);

  const loadNodes = useCallback(() => {
    if (nodesLoaded || loading.current) return;
    loading.current = true;
    loadAllNodes()
      .then((ns) => {
        setNodes(ns);
        setNodesLoaded(true);
      })
      .catch(() => {
        /* leave nodesLoaded false; Node mode shows an empty/loading message */
      })
      .finally(() => {
        loading.current = false;
      });
  }, [nodesLoaded]);

  return { groups, nodes, nodesLoaded, loadNodes };
}
