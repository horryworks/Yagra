// Collection-set editor (what SNMP metrics to poll at a scope). Reused for a device profile
// (defaults for the class) and for a single node (overrides). Lists the items defined at the
// scope, lets an admin add a scalar/table metric (metric_name + OID + kind), and delete one.
// Resolution (node overrides profile) and the built-in fallback happen server-side; this only
// shows what's explicitly configured at this scope.

import { useCallback, useEffect, useState } from 'react';
import { api, ApiError, type CollectionItemInput } from '../../services/api';
import type {
  CollectionKind,
  MetricKind,
  MibCatalogEntry,
  StoredCollectionItem,
} from '../../types/api';
import { Button } from '../ui/Button';
import { TextInput, Select } from '../ui/Field';
import { IconButton } from '../ui/IconButton';
import { TrashIcon } from '../ui/icons';
import './CollectionEditor.css';

const errMsg = (e: unknown, fallback: string) => (e instanceof ApiError ? e.message : fallback);

// Mirrors the server's is_valid_oid (dotted digits only).
const OID_RE = /^[0-9]+(\.[0-9]+)*$/;

export function CollectionEditor({
  scope,
  scopeId,
  canEdit,
}: {
  scope: 'node' | 'template';
  scopeId: string;
  canEdit: boolean;
}) {
  const [items, setItems] = useState<StoredCollectionItem[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [metricName, setMetricName] = useState('');
  const [oid, setOid] = useState('');
  const [collection, setCollection] = useState<CollectionKind>('scalar');
  const [metricKind, setMetricKind] = useState<MetricKind>('gauge');
  const [busy, setBusy] = useState(false);
  // "Browse catalog" picker (MIB repository) — fill the form by name instead of typing OIDs.
  const [picking, setPicking] = useState(false);
  const [pickQuery, setPickQuery] = useState('');
  const [picks, setPicks] = useState<MibCatalogEntry[]>([]);

  const load = useCallback(() => {
    const p =
      scope === 'template'
        ? api.listTemplateItems(scopeId)
        : api.listNodeCollection(scopeId);
    p.then((list) => {
      setItems(list);
      setError(null);
    })
      .catch((e: unknown) => setError(errMsg(e, 'failed to load metrics')))
      .finally(() => setLoading(false));
  }, [scope, scopeId]);

  useEffect(() => {
    load();
  }, [load]);

  // Debounced catalog search while the picker is open.
  useEffect(() => {
    if (!picking) return;
    const t = setTimeout(() => {
      api
        .listMibCatalog(pickQuery.trim() || undefined)
        .then(setPicks)
        .catch(() => setPicks([]));
    }, 200);
    return () => clearTimeout(t);
  }, [picking, pickQuery]);

  const pick = (e: MibCatalogEntry) => {
    setMetricName(e.metric_name);
    setOid(e.oid);
    setCollection(e.collection);
    setMetricKind(e.metric_kind);
    setPicking(false);
  };

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
      scope === 'template'
        ? api.addTemplateItem(scopeId, body)
        : api.addNodeCollection(scopeId, body);
    p.then(() => {
      setMetricName('');
      setOid('');
      load();
    })
      .catch((e: unknown) => setError(errMsg(e, 'failed to add metric')))
      .finally(() => setBusy(false));
  };

  const remove = (id: string) => {
    const p =
      scope === 'template'
        ? api.deleteTemplateItem(scopeId, id)
        : api.deleteCollectionItem(id);
    p.then(load).catch((e: unknown) => setError(errMsg(e, 'failed to delete metric')));
  };

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
          <Button variant="ghost" onClick={() => setPicking((p) => !p)}>
            {picking ? 'Close catalog' : 'Browse catalog'}
          </Button>
          <Button variant="primary" onClick={add} disabled={!valid || busy}>
            Add metric
          </Button>
        </div>
      )}
      {canEdit && picking && (
        <div className="ce-picker">
          <TextInput
            className="ce-picker-search"
            placeholder="Search the MIB catalog by name / OID / vendor…"
            value={pickQuery}
            onChange={(e) => setPickQuery(e.target.value)}
          />
          {picks.length === 0 ? (
            <p className="muted">No catalog entries match.</p>
          ) : (
            <div className="ce-picker-list">
              {picks.slice(0, 30).map((e) => (
                <button type="button" className="ce-picker-row" key={e.id} onClick={() => pick(e)}>
                  <span className="ce-picker-metric">{e.metric_name}</span>
                  <span className="ce-picker-oid mono">{e.oid}</span>
                  <span className="muted">
                    {e.collection} · {e.metric_kind}
                    {e.vendor ? ` · ${e.vendor}` : ''}
                  </span>
                </button>
              ))}
            </div>
          )}
        </div>
      )}
      {error && <p className="form-error">{error}</p>}
      {items.length === 0 ? (
        <p className="muted">
          {loading
            ? 'Loading…'
            : scope === 'template'
              ? 'No metrics in this template yet.'
              : 'No node-level metrics. The profile templates / built-in defaults still apply.'}
        </p>
      ) : (
        <div className="ytable ce-table">
          <div className="ytable-head">
            <div className="ytable-h">Metric</div>
            <div className="ytable-h">OID</div>
            <div className="ytable-h">Type</div>
            <div className="ytable-h">Kind</div>
            <div className="ytable-h right">Actions</div>
          </div>
          {items.map((it) => (
            <div className="ytable-row" key={it.id}>
              <div className="ytable-cell ellipsis ce-metric">{it.metric_name}</div>
              <div className="ytable-cell ellipsis ce-oid mono">{it.oid}</div>
              <div className="ytable-cell">{it.kind}</div>
              <div className="ytable-cell">{it.metric_kind}</div>
              <div className="ytable-cell right ce-actions">
                {canEdit && (
                  <span className="ytable-actions">
                    <IconButton title="Delete metric" danger onClick={() => remove(it.id)}>
                      <TrashIcon />
                    </IconButton>
                  </span>
                )}
              </div>
            </div>
          ))}
        </div>
      )}
    </div>
  );
}
