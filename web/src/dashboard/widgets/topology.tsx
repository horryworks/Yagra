// 04 · Dependency / root-cause view. Renders the parent→child dependency forest as a compact
// indented tree (no graph library): each node shows a status dot, its name, and — when the alert
// engine has attributed an upstream root cause — a muted "← caused by <name>" so downstream
// alerts read as collapsed under the cause. Data: GET /api/v1/topology (parent links + root_cause),
// fetched on a slow reconcile cadence and kept live via the node-state SSE stream (S14).

import { useMemo } from 'react';
import { useTranslation } from 'react-i18next';
import { StatusDot } from '../../components/ui/StatusDot';
import { api } from '../../services/api';
import { usePolled } from '../usePolled';
import { useNodeStates, LIVE_RECONCILE_MS } from '../useNodeStates';
import { buildForest, type TopoTreeNode } from './util';
import './topology.css';

/** Max render depth — a backstop against a parent cycle (server guards, but be safe). */
const MAX_DEPTH = 8;

function TreeRow({
  node,
  depth,
  names,
}: {
  node: TopoTreeNode;
  depth: number;
  names: Map<string, string>;
}) {
  const { t } = useTranslation('dashboard');
  // Don't silently drop the subtree at the depth backstop — show why it stopped.
  if (depth > MAX_DEPTH) {
    return (
      <li className="topo-item">
        <div className="topo-row" style={{ paddingLeft: `${depth * 16}px` }}>
          <span className="topo-name muted">{t('widgets.dependency.depthLimit')}</span>
        </div>
      </li>
    );
  }
  const cause = node.node.root_cause ? names.get(node.node.root_cause) : null;
  return (
    <li className="topo-item">
      <div className="topo-row" style={{ paddingLeft: `${depth * 16}px` }}>
        <StatusDot state={node.node.state} withLabel={false} />
        <span className="topo-name">{node.node.name}</span>
        {cause && <span className="topo-cause muted">← {cause}</span>}
      </div>
      {node.children.length > 0 && (
        <ul className="topo-children">
          {node.children.map((c) => (
            <TreeRow key={c.node.id} node={c} depth={depth + 1} names={names} />
          ))}
        </ul>
      )}
    </li>
  );
}

export function DependencyWidget() {
  const { t } = useTranslation('dashboard');
  const { data, loading, error } = usePolled(() => api.getTopology(), [], LIVE_RECONCILE_MS);
  const live = useNodeStates();
  // Overlay the live SSE state on top of the fetched graph (S14).
  const nodes = useMemo(() => {
    const base = data?.nodes ?? [];
    return base.map((n) => {
      const s = live.get(n.id);
      return s && s !== n.state ? { ...n, state: s } : n;
    });
  }, [data, live]);
  if (error) return <p className="muted">{error}</p>;
  if (loading && !data) return <p className="muted">{t('common:loading')}</p>;
  if (nodes.length === 0) return <p className="muted">{t('widgets.dependency.empty')}</p>;
  const names = new Map(nodes.map((n) => [n.id, n.name]));
  const forest = buildForest(nodes);
  // A flat inventory (no parent links) still renders — just a single-level list.
  return (
    <ul className="topo">
      {forest.map((r) => (
        <TreeRow key={r.node.id} node={r} depth={0} names={names} />
      ))}
    </ul>
  );
}
