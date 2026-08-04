// SPDX-License-Identifier: AGPL-3.0-only
// 04 · Dependency / root-cause view. A flat list of what is actually broken: each root cause with
// the alerts rolled up under it, biggest first, then any problem the engine could not attribute.
//
// It used to render an indented parent→child tree. ADR-043 Increment 2 made the dependency graph
// multi-parent — a node reached by two equal-length paths from a poller has two upstreams, which is
// exactly the redundant-pair case the suppression rule exists for — and a tree cannot show that
// without silently picking one parent. The widget's question was always "what is broken and what
// does it explain", so it now answers that directly.
//
// Data: GET /api/v1/topology (root_cause attribution), on a slow reconcile cadence, kept live via
// the node-state SSE stream (S14). All judgement is in `util.ts::rootCauseRows` — Vitest never runs
// a `.tsx`.

import { useMemo } from 'react';
import { useTranslation } from 'react-i18next';
import { StatusDot } from '../../components/ui/StatusDot';
import { api } from '../../services/api';
import { usePolled } from '../usePolled';
import { useNodeStates, LIVE_RECONCILE_MS } from '../useNodeStates';
import { rootCauseRows } from './util';
import './topology.css';

/** Max affected nodes named per cause before the rest are summarized. A cause explaining a whole
 *  site would otherwise fill the widget with names nobody reads. */
const NAMED_AFFECTED = 4;

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
  const rows = useMemo(() => rootCauseRows(nodes), [nodes]);

  if (error) return <p className="muted">{error}</p>;
  if (loading && !data) return <p className="muted">{t('common:loading')}</p>;
  if (nodes.length === 0) return <p className="muted">{t('widgets.dependency.empty')}</p>;
  // A healthy fleet has no causes. Distinct from "no inventory" above, and worth saying so.
  if (rows.length === 0) return <p className="muted">{t('widgets.dependency.allClear')}</p>;

  return (
    <ul className="topo">
      {rows.map((r) => {
        const named = r.affected.slice(0, NAMED_AFFECTED);
        const rest = r.affected.length - named.length;
        return (
          <li className="topo-item" key={r.node.id}>
            <div className="topo-row">
              <StatusDot state={r.node.state} withLabel={false} />
              <span className="topo-name">{r.node.name}</span>
              {r.affected.length > 0 && (
                <span className="topo-cause muted">
                  {t('widgets.dependency.explains', { count: r.affected.length })}
                </span>
              )}
            </div>
            {named.length > 0 && (
              <ul className="topo-children">
                {named.map((a) => (
                  <li className="topo-item" key={a.id}>
                    <div className="topo-row" style={{ paddingLeft: '16px' }}>
                      <StatusDot state={a.state} withLabel={false} />
                      <span className="topo-name muted">{a.name}</span>
                    </div>
                  </li>
                ))}
                {rest > 0 && (
                  <li className="topo-item">
                    <div className="topo-row" style={{ paddingLeft: '16px' }}>
                      <span className="topo-name muted">
                        {t('widgets.dependency.andMore', { count: rest })}
                      </span>
                    </div>
                  </li>
                )}
              </ul>
            )}
          </li>
        );
      })}
    </ul>
  );
}
