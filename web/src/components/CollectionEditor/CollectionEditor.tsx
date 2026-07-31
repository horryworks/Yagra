// SPDX-License-Identifier: AGPL-3.0-only
// Collection-set editor (what SNMP metrics to poll at a scope). Reused for a device profile
// (defaults for the class) and for a single node (overrides). Lists the items defined at the
// scope, lets an admin add a scalar/table metric (metric_name + OID + kind), and delete one.
// Resolution (node overrides profile) and the built-in fallback happen server-side; this only
// shows what's explicitly configured at this scope.

import { useCallback, useEffect, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { api, errMsg, type CollectionItemInput } from '../../services/api';
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
  const { t } = useTranslation('monitoring');
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
      .catch((e: unknown) => setError(errMsg(e, t('editor.err.load'))))
      .finally(() => setLoading(false));
  }, [scope, scopeId, t]);

  useEffect(() => {
    load();
  }, [load]);

  // Debounced catalog search while the picker is open.
  useEffect(() => {
    if (!picking) return;
    const timer = setTimeout(() => {
      api
        .listMibCatalog(pickQuery.trim() || undefined)
        .then(setPicks)
        .catch(() => setPicks([]));
    }, 200);
    return () => clearTimeout(timer);
  }, [picking, pickQuery]);

  // A catalog row types `collection`/`metric_kind` as bare strings on the wire, unlike the stored
  // collection item this form writes, so the picker narrows rather than adopting the row's value.
  const pick = (e: MibCatalogEntry) => {
    setMetricName(e.metric_name);
    setOid(e.oid);
    setCollection(e.collection === 'table' ? 'table' : 'scalar');
    setMetricKind(e.metric_kind === 'counter' ? 'counter' : 'gauge');
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
      .catch((e: unknown) => setError(errMsg(e, t('editor.err.add'))))
      .finally(() => setBusy(false));
  };

  const remove = (id: string) => {
    const p =
      scope === 'template'
        ? api.deleteTemplateItem(scopeId, id)
        : api.deleteCollectionItem(id);
    p.then(load).catch((e: unknown) => setError(errMsg(e, t('editor.err.delete'))));
  };

  return (
    <div className="ce">
      {canEdit && (
        <div className="ce-add form-row">
          <TextInput
            placeholder={t('editor.metricNamePlaceholder')}
            value={metricName}
            onChange={(e) => setMetricName(e.target.value)}
          />
          <TextInput
            className="mono"
            placeholder={t('editor.oidPlaceholder')}
            value={oid}
            onChange={(e) => setOid(e.target.value)}
          />
          <Select
            value={collection}
            onChange={(e) => setCollection(e.target.value as CollectionKind)}
            aria-label={t('editor.collectionKindAria')}
          >
            <option value="scalar">{t('enum.scalar')}</option>
            <option value="table">{t('enum.tablePerInterface')}</option>
          </Select>
          <Select
            value={metricKind}
            onChange={(e) => setMetricKind(e.target.value as MetricKind)}
            aria-label={t('editor.metricKindAria')}
          >
            <option value="gauge">{t('enum.gauge')}</option>
            <option value="counter">{t('enum.counter')}</option>
          </Select>
          <Button variant="ghost" onClick={() => setPicking((p) => !p)}>
            {picking ? t('editor.closeCatalog') : t('editor.browseCatalog')}
          </Button>
          <Button variant="primary" onClick={add} disabled={!valid || busy}>
            {t('editor.addMetric')}
          </Button>
        </div>
      )}
      {canEdit && picking && (
        <div className="ce-picker">
          <TextInput
            className="ce-picker-search"
            placeholder={t('editor.pickerSearchPlaceholder')}
            value={pickQuery}
            onChange={(e) => setPickQuery(e.target.value)}
          />
          {picks.length === 0 ? (
            <p className="muted">{t('editor.noCatalogMatch')}</p>
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
            ? t('common:loading')
            : scope === 'template'
              ? t('editor.emptyTemplate')
              : t('editor.emptyNode')}
        </p>
      ) : (
        <div className="ytable ce-table">
          <div className="ytable-head">
            <div className="ytable-h">{t('editor.cols.metric')}</div>
            <div className="ytable-h">{t('editor.cols.oid')}</div>
            <div className="ytable-h">{t('editor.cols.type')}</div>
            <div className="ytable-h">{t('editor.cols.kind')}</div>
            <div className="ytable-h right">{t('shared.colActions')}</div>
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
                    <IconButton title={t('editor.deleteMetric')} danger onClick={() => remove(it.id)}>
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
