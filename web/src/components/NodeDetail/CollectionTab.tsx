// Collection tab of the unified node detail. A read-first status view over what this node collects:
// a status card (collecting / failing / ICMP-only), the named collection sets it inherits from its
// device profile, the latest scalar values, and — for admins — the existing CollectionEditor to add
// or override node-level metrics. "Sets" map to the templates the node's profile attaches
// (api.listProfileTemplates); per-set interval and per-metric thresholds aren't exposed by the API,
// so those columns are intentionally omitted rather than faked.

import { useEffect, useState } from 'react';
import { api } from '../../services/api';
import { relativeTime, scalarDisplay } from '../../lib/format';
import type { CollectionTemplate, NodeDetail } from '../../types/api';
import { CollectionEditor } from '../CollectionEditor/CollectionEditor';

type CollState = 'ok' | 'failing' | 'none';

interface ScalarValue {
  name: string;
  display: string;
  updatedSec: number | null;
}

interface Loaded {
  sets: CollectionTemplate[];
  scalars: ScalarValue[];
  profileName: string | null;
  credentialName: string | null;
  lastPollSec: number | null;
}

const agoSec = (sec: number | null): string =>
  sec == null ? '—' : relativeTime(new Date(sec * 1000).toISOString());

export function CollectionTab({ node, canEdit }: { node: NodeDetail; canEdit: boolean }) {
  const [data, setData] = useState<Loaded | null>(null);

  useEffect(() => {
    let cancelled = false;
    (async () => {
      const [sets, profileName, credentialName] = await Promise.all([
        node.profile_id
          ? api.listProfileTemplates(node.profile_id).catch(() => [] as CollectionTemplate[])
          : Promise.resolve([] as CollectionTemplate[]),
        node.profile_id
          ? api
              .listProfiles()
              .then((ps) => ps.find((p) => p.id === node.profile_id)?.name ?? null)
              .catch(() => null)
          : Promise.resolve(null),
        node.credential_id
          ? api
              .listCredentials()
              .then((cs) => cs.find((c) => c.id === node.credential_id)?.name ?? null)
              .catch(() => null)
          : Promise.resolve(null),
      ]);

      // Latest scalar values (table metrics are per-interface — shown on the Interfaces tab).
      let scalarNames: string[] = [];
      try {
        const items = await api.listNodeCollection(node.id, true);
        scalarNames = items.filter((i) => i.kind === 'scalar').map((i) => i.metric_name);
      } catch {
        // admin-only endpoint not permitted → no latest-values table
      }
      const scalars: ScalarValue[] = [];
      let lastPollSec: number | null = null;
      for (const name of scalarNames) {
        try {
          const to = Math.floor(Date.now() / 1000);
          const [latest, range] = await Promise.all([
            api.getNodeMetric(node.id, name),
            api.getNodeMetricRange(node.id, name, { from: to - 3600, to }).catch(() => null),
          ]);
          const last = range?.points.at(-1)?.t ?? null;
          if (last != null) lastPollSec = Math.max(lastPollSec ?? 0, last);
          scalars.push({ name, display: scalarDisplay(name, latest.value).value, updatedSec: last });
        } catch {
          // no reading for this metric yet
        }
      }
      if (!cancelled) setData({ sets, scalars, profileName, credentialName, lastPollSec });
    })();
    return () => {
      cancelled = true;
    };
  }, [node.id, node.profile_id, node.credential_id]);

  const hasSnmp = !!node.credential_id;
  const state: CollState = !hasSnmp ? 'none' : (data && data.scalars.length > 0 ? 'ok' : 'failing');

  return (
    <div className="nd-coll">
      <div className={`nd-coll-status ${state}`}>
        <span className={`nd-coll-icon ${state}`} aria-hidden>
          {state === 'ok' ? '✓' : state === 'failing' ? '!' : '○'}
        </span>
        <div>
          <div className="nd-coll-status-t">
            {state === 'ok'
              ? 'Collecting normally'
              : state === 'failing'
                ? 'Collection failing'
                : 'ICMP-only node'}
          </div>
          <div className="nd-coll-status-s">
            {state === 'ok'
              ? `${data?.sets.length ?? 0} set${(data?.sets.length ?? 0) === 1 ? '' : 's'} active · polling on schedule`
              : state === 'failing'
                ? 'No recent SNMP values — the device may be unreachable or denying SNMP'
                : 'No SNMP credential bound; only ICMP availability is collected'}
          </div>
        </div>
        <div className="nd-coll-status-r">
          <div>
            <div className="nd-coll-stat-k">Profile</div>
            <div className="nd-coll-stat-v">{data?.profileName ?? '—'}</div>
          </div>
          <div>
            <div className="nd-coll-stat-k">Credential</div>
            <div className="nd-coll-stat-v">{data?.credentialName ?? '—'}</div>
          </div>
          <div>
            <div className="nd-coll-stat-k">Last poll</div>
            <div className="nd-coll-stat-v">{agoSec(data?.lastPollSec ?? null)}</div>
          </div>
        </div>
      </div>

      {data && data.sets.length > 0 && (
        <section>
          <div className="nd-section-t">Metric sets</div>
          <div className="nd-coll-sets">
            {data.sets.map((s) => (
              <div className="nd-coll-set" key={s.id}>
                <span className={`nd-coll-set-dot ${state}`} />
                <div>
                  <div className="nd-coll-set-name mono">{s.name}</div>
                  <div className="nd-coll-set-meta">
                    {s.item_count} metric{s.item_count === 1 ? '' : 's'}
                  </div>
                </div>
              </div>
            ))}
          </div>
        </section>
      )}

      {data && data.scalars.length > 0 && (
        <section>
          <div className="nd-section-t">Latest values</div>
          <div className="nd-coll-metrics">
            <div className="nd-coll-mhead">
              <div className="nd-coll-mh">Metric</div>
              <div className="nd-coll-mh right">Last value</div>
              <div className="nd-coll-mh right">Updated</div>
            </div>
            {data.scalars.map((m) => (
              <div className="nd-coll-mrow" key={m.name}>
                <div className="nd-coll-mcell mono nd-coll-mname">{m.name}</div>
                <div className="nd-coll-mcell right nd-coll-mval">{m.display}</div>
                <div className="nd-coll-mcell right nd-coll-mage">{agoSec(m.updatedSec)}</div>
              </div>
            ))}
          </div>
        </section>
      )}

      <section>
        <div className="nd-section-t">Node metrics</div>
        <p className="nd-muted nd-coll-editnote">
          SNMP metrics polled from this node. Node-level entries override the device profile; with
          none set, the profile / built-in defaults apply.
        </p>
        <CollectionEditor scope="node" scopeId={node.id} canEdit={canEdit} />
      </section>
    </div>
  );
}
