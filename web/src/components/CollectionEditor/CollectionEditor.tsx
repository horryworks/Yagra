// Collection-set editor (what SNMP metrics to poll at a scope). Reused for a device profile
// (defaults for the class) and for a single node (overrides). Lists the items defined at the
// scope, lets an admin add a scalar/table metric (metric_name + OID + kind), and delete one.
// Resolution (node overrides profile) and the built-in fallback happen server-side; this only
// shows what's explicitly configured at this scope.

import { useCallback, useEffect, useState } from 'react';
import { api, ApiError, type CollectionItemInput } from '../../services/api';
import type { CollectionKind, MetricKind, StoredCollectionItem } from '../../types/api';
import { Button } from '../ui/Button';
import { TextInput, Select } from '../ui/Field';
import './CollectionEditor.css';

const errMsg = (e: unknown, fallback: string) => (e instanceof ApiError ? e.message : fallback);

// Mirrors the server's is_valid_oid (dotted digits only).
const OID_RE = /^[0-9]+(\.[0-9]+)*$/;

export function CollectionEditor({
  scope,
  scopeId,
  canEdit,
}: {
  scope: 'profile' | 'node';
  scopeId: string;
  canEdit: boolean;
}) {
  const [items, setItems] = useState<StoredCollectionItem[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [metricName, setMetricName] = useState('');
  const [oid, setOid] = useState('');
  const [collection, setCollection] = useState<CollectionKind>('scalar');
  const [metricKind, setMetricKind] = useState<MetricKind>('gauge');
  const [busy, setBusy] = useState(false);

  const load = useCallback(() => {
    const p =
      scope === 'profile'
        ? api.listProfileCollection(scopeId)
        : api.listNodeCollection(scopeId);
    p.then((list) => {
      setItems(list);
      setError(null);
    }).catch((e: unknown) => setError(errMsg(e, 'failed to load collection set')));
  }, [scope, scopeId]);

  useEffect(() => {
    load();
  }, [load]);

  const valid = metricName.trim().length > 0 && OID_RE.test(oid.trim());

  const add = () => {
    if (!valid) return;
    setBusy(true);
    setError(null);
    const body: CollectionItemInput = {
      metric_name: metricName.trim(),
      oid: oid.trim(),
      collection,
      metric_kind: metricKind,
    };
    const p =
      scope === 'profile'
        ? api.addProfileCollection(scopeId, body)
        : api.addNodeCollection(scopeId, body);
    p.then(() => {
      setMetricName('');
      setOid('');
      load();
    })
      .catch((e: unknown) => setError(errMsg(e, 'failed to add metric')))
      .finally(() => setBusy(false));
  };

  const remove = (id: string) =>
    api
      .deleteCollectionItem(id)
      .then(load)
      .catch((e: unknown) => setError(errMsg(e, 'failed to delete metric')));

  return (
    <div className="ce">
      {canEdit && (
        <div className="ce-add form-row">
          <TextInput
            placeholder="metric_name (e.g. cpu_util)"
            value={metricName}
            onChange={(e) => setMetricName(e.target.value)}
          />
          <TextInput
            className="mono"
            placeholder="OID (e.g. 1.3.6.1.2.1.1.3.0)"
            value={oid}
            onChange={(e) => setOid(e.target.value)}
          />
          <Select
            value={collection}
            onChange={(e) => setCollection(e.target.value as CollectionKind)}
            aria-label="Collection kind"
          >
            <option value="scalar">scalar</option>
            <option value="table">table (per-interface)</option>
          </Select>
          <Select
            value={metricKind}
            onChange={(e) => setMetricKind(e.target.value as MetricKind)}
            aria-label="Metric kind"
          >
            <option value="gauge">gauge</option>
            <option value="counter">counter</option>
          </Select>
          <Button variant="primary" onClick={add} disabled={!valid || busy}>
            Add metric
          </Button>
        </div>
      )}
      {error && <p className="form-error">{error}</p>}
      {items.length === 0 ? (
        <p className="muted">
          No metrics configured at this scope.
          {scope === 'node' ? ' The profile / built-in defaults still apply.' : ''}
        </p>
      ) : (
        <div className="ce-table">
          <div className="ce-head">
            <div className="ce-h">Metric</div>
            <div className="ce-h">OID</div>
            <div className="ce-h">Type</div>
            <div className="ce-h">Kind</div>
            <div className="ce-h right">Actions</div>
          </div>
          {items.map((it) => (
            <div className="ce-row" key={it.id}>
              <span className="ce-metric">{it.metric_name}</span>
              <span className="ce-oid mono">{it.oid}</span>
              <span>{it.kind}</span>
              <span>{it.metric_kind}</span>
              <div className="ce-actions">
                {canEdit && (
                  <Button variant="ghost" onClick={() => remove(it.id)}>
                    Delete
                  </Button>
                )}
              </div>
            </div>
          ))}
        </div>
      )}
    </div>
  );
}
