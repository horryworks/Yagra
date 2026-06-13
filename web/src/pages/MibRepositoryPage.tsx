// MIB repository (Nodes ▸ MIB repository). A curated, searchable OID catalog: metric_name →
// (OID, kind, vendor). Seeded from the built-in standard + vendor OID sets; admins can add
// their own. The collection editor picks from this so operators choose metrics by name instead
// of typing raw OIDs.

import { useCallback, useEffect, useState } from 'react';
import { api, ApiError } from '../services/api';
import { useAuthStore } from '../store';
import type { CollectionKind, MetricKind, MibCatalogEntry } from '../types/api';
import { PageHeader } from '../components/ui/PageHeader';
import { Card } from '../components/ui/Card';
import { Button } from '../components/ui/Button';
import { TextInput, Select } from '../components/ui/Field';
import { Badge } from '../components/ui/Badge';
import './CrudList.css';
import './MibRepositoryPage.css';

const errMsg = (e: unknown, fallback: string) =>
  e instanceof ApiError ? e.message : fallback;

const OID_RE = /^[0-9]+(\.[0-9]+)*$/;

export function MibRepositoryPage() {
  const authed = useAuthStore((s) => s.authed);
  const [rows, setRows] = useState<MibCatalogEntry[]>([]);
  const [query, setQuery] = useState('');
  const [unavailable, setUnavailable] = useState(false);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  // Add-entry form.
  const [metricName, setMetricName] = useState('');
  const [oid, setOid] = useState('');
  const [collection, setCollection] = useState<CollectionKind>('scalar');
  const [metricKind, setMetricKind] = useState<MetricKind>('gauge');
  const [vendor, setVendor] = useState('');

  const load = useCallback((q: string) => {
    api
      .listMibCatalog(q.trim() || undefined)
      .then((list) => {
        setRows(list);
        setUnavailable(false);
      })
      .catch((e: unknown) => {
        if (e instanceof ApiError && e.code === 'admin_unavailable') setUnavailable(true);
      })
      .finally(() => setLoading(false));
  }, []);

  useEffect(() => {
    const t = setTimeout(() => load(query), 200);
    return () => clearTimeout(t);
  }, [load, query]);

  const valid = metricName.trim().length > 0 && OID_RE.test(oid.trim());

  const add = () => {
    setError(null);
    api
      .createMibEntry({
        metric_name: metricName.trim(),
        oid: oid.trim(),
        collection,
        metric_kind: metricKind,
        vendor: vendor.trim() || undefined,
      })
      .then(() => {
        setMetricName('');
        setOid('');
        setVendor('');
        load(query);
      })
      .catch((e: unknown) => setError(errMsg(e, 'failed to add entry')));
  };

  const remove = (id: string) =>
    api
      .deleteMibEntry(id)
      .then(() => load(query))
      .catch((e: unknown) => setError(errMsg(e, 'failed to delete entry')));

  return (
    <div>
      <PageHeader
        title="MIB repository"
        trail={[{ label: 'Nodes' }, { label: 'MIB repository' }]}
        note="A curated OID catalog. Pick metrics by name in the collection editor instead of typing OIDs."
      />

      {unavailable ? (
        <Card>
          <p className="muted">The MIB catalog is unavailable in skeleton mode.</p>
        </Card>
      ) : (
        <Card title="OID catalog">
          <div className="mib-toolbar">
            <TextInput
              className="mib-search"
              placeholder="Search metric / OID / vendor…"
              value={query}
              onChange={(e) => setQuery(e.target.value)}
            />
          </div>

          {authed && (
            <div className="mib-add form-row">
              <TextInput
                placeholder="metric_name"
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
                <option value="table">table</option>
              </Select>
              <Select
                value={metricKind}
                onChange={(e) => setMetricKind(e.target.value as MetricKind)}
                aria-label="Metric kind"
              >
                <option value="gauge">gauge</option>
                <option value="counter">counter</option>
              </Select>
              <TextInput
                placeholder="vendor (optional)"
                value={vendor}
                onChange={(e) => setVendor(e.target.value)}
              />
              <Button variant="primary" onClick={add} disabled={!valid}>
                Add
              </Button>
            </div>
          )}
          {error && <p className="form-error">{error}</p>}

          {rows.length === 0 ? (
            <p className="muted">{loading ? 'Loading…' : 'No catalog entries match.'}</p>
          ) : (
            <div className="mib-table">
              <div className="mib-head">
                <div className="mib-h">Metric</div>
                <div className="mib-h">OID</div>
                <div className="mib-h">Type</div>
                <div className="mib-h">Vendor</div>
                <div className="mib-h right">Actions</div>
              </div>
              {rows.map((e) => (
                <div className="mib-row" key={e.id}>
                  <span className="mib-metric">{e.metric_name}</span>
                  <span className="mib-oid mono">{e.oid}</span>
                  <span>
                    {e.collection} · {e.metric_kind}
                  </span>
                  <span>{e.vendor ? <Badge tone="neutral">{e.vendor}</Badge> : <span className="muted">standard</span>}</span>
                  <div className="mib-actions">
                    {authed && (
                      <Button variant="ghost" onClick={() => remove(e.id)}>
                        Delete
                      </Button>
                    )}
                  </div>
                </div>
              ))}
            </div>
          )}
        </Card>
      )}
    </div>
  );
}
