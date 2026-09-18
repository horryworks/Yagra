// SPDX-License-Identifier: AGPL-3.0-only
// AP tab of the node detail (ADR-064 増分 B3): the access points a wireless controller reports, and
// the two writes that turn them into monitored nodes.
//
// **Why this tab exists at all.** Increments B1/B2 shipped the whole machine — the inventory walk,
// the AP node kind, the importer, the settings PUT and the per-AP POST — and no screen called any
// of it. Importing an AP was possible only with curl. Nothing here is new behaviour; it is the
// affordance for behaviour that already shipped.
//
// **One fetch, whole inventory.** `limit` is the per-controller cap itself, so the page *is* the
// list and there is no cursor to follow. That is what lets the filter row be client-side: filtering
// on the server would answer "the first page of the matches" while the toolbar counted the rows on
// screen, and neither number would say which one it was.
//
// All judgement lives in `apRows.ts` and `tabFilters.ts` beside it — Vitest never runs a `.tsx`
// (testing.md), so a helper written here is a helper no test executes.

import { useEffect, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { Link } from 'react-router-dom';
import { api, errMsg } from '../../services/api';
import { relativeTime } from '../../lib/format';
import { useRefreshTick } from '../../lib/refreshTick';
import { groupOptions } from '../../lib/nodeTree';
import { nodesPageHref } from '../../lib/treeSelection';
import { useCan } from '../../store';
import type {
  NodeDetail as NodeDetailData,
  NodeGroup,
  WirelessApPage,
  WirelessApRow,
} from '../../types/api';
import { DataTable, type Column } from '../ui/DataTable';
import { TableToolbar, TableSpacer } from '../ui/TableToolbar';
import { ClearFilters } from '../ui/ClearFilters';
import { FilterButton, MobileFilterSheet } from '../ui/MobileFilterSheet';
import { Button } from '../ui/Button';
import { Card } from '../ui/Card';
import { GroupPicker } from '../ui/GroupPicker';
import { TextInput } from '../ui/Field';
import { PermissionHint } from '../ui/PermissionHint';
import { EntityName } from '../ui/EntityName';
import { isEntityResolved, useEntityNames } from '../ui/entityNames';
import { useClientFilters } from '../../lib/useClientFilters';
import { apFilters } from './tabFilters';
import { nodeTabFilterPrefix } from './tabs';
import {
  apLabel,
  apStateKey,
  apsOverCap,
  awaitingFirstInventory,
  reportingControllers,
  MAX_APS_DEFAULT,
  MAX_APS_HARD,
  MAX_APS_MIN,
} from './apRows';
import './ApTab.css';

interface Props {
  node: NodeDetailData;
  groups: NodeGroup[];
  /** Importing an AP creates a node, which changes this page's folder and the tree beside it. */
  onChanged: () => void;
}

export function ApTab({ node, groups, onChanged }: Props) {
  const { t } = useTranslation('nodes');
  const tick = useRefreshTick();
  const canConfig = useCan('manage_config');
  const [page, setPage] = useState<WirelessApPage | null>(null);
  const [loaded, setLoaded] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [note, setNote] = useState<string | null>(null);
  const [busyId, setBusyId] = useState<string | null>(null);
  const [nonce, setNonce] = useState(0);
  const [sheet, setSheet] = useState(false);

  useEffect(() => {
    let cancelled = false;
    api
      .listWirelessAps({ controllerNodeId: node.id, limit: MAX_APS_HARD })
      .then((p) => {
        if (cancelled) return;
        setPage(p);
        setError(null);
      })
      .catch((e: unknown) => {
        if (!cancelled) setError(errMsg(e, t('ap.err.load')));
      })
      .finally(() => {
        if (!cancelled) setLoaded(true);
      });
    return () => {
      cancelled = true;
    };
  }, [node.id, tick, nonce, t]);

  // The settings the PUT reads back, or — before the first inventory and before anyone has saved —
  // the one the node read carried. Both are the same shape; the fresher one wins.
  const summary = page?.controller ?? node.wireless?.controller ?? null;
  const rows = page?.aps ?? [];

  const importOne = (row: WirelessApRow) => {
    setBusyId(row.ap_id);
    setNote(null);
    setError(null);
    api
      .importWirelessAp(row.ap_id)
      .then(() => {
        setNote(t('ap.imported', { ap: apLabel(row) }));
        setNonce((v) => v + 1);
        onChanged();
      })
      .catch((e: unknown) => setError(errMsg(e, t('ap.err.import'))))
      .finally(() => setBusyId(null));
  };

  const specs = apFilters(t);
  const columns: Column<WirelessApRow>[] = [
    {
      key: 'ap',
      header: t('ap.colAp'),
      width: '1.6fr',
      render: (r) => (
        <span className="nd-ap-name">
          <span>{apLabel(r)}</span>
          {r.name?.trim() ? <span className="mono nd-muted">{r.mac}</span> : null}
        </span>
      ),
    },
    {
      key: 'state',
      header: t('ap.colState'),
      width: '1.2fr',
      render: (r) => (
        <span className="nd-ap-state">
          <span className={`nd-ap-dot nd-ap-dot-${apStateKey(r)}`} aria-hidden="true" />
          <span>{t(`ap.state.${apStateKey(r)}`)}</span>
          {/* The vendor's own word, verbatim: `normal` / `fault` / `standby` on Huawei, and the
              list grows per flavour. Translating it would mean inventing a vocabulary the device
              does not have (決定 14). */}
          <span className="mono nd-muted">{r.run_state}</span>
        </span>
      ),
    },
    {
      key: 'ip',
      header: t('ap.colAddress'),
      width: '1.1fr',
      render: (r) => <span className="mono">{r.ip ?? '—'}</span>,
    },
    { key: 'model', header: t('ap.colModel'), width: '1.1fr', render: (r) => r.model ?? '—' },
    {
      key: 'clients',
      header: t('ap.colClients'),
      width: '90px',
      align: 'right',
      render: (r) => (r.clients == null ? '—' : r.clients),
    },
    {
      key: 'reported',
      header: t('ap.colReportedBy'),
      width: '1.3fr',
      render: (r) => <ReportedBy row={r} />,
    },
    {
      key: 'last_seen',
      header: t('ap.colLastSeen'),
      width: '1.1fr',
      render: (r) => <span title={r.last_seen}>{relativeTime(r.last_seen)}</span>,
    },
    {
      key: 'imported',
      header: t('ap.colImported'),
      width: '1.2fr',
      render: (r) =>
        r.node_id != null ? (
          <Link to={nodesPageHref({ kind: 'node', id: r.node_id })}>{t('ap.openNode')}</Link>
        ) : canConfig ? (
          <Button variant="primary" disabled={busyId != null} onClick={() => importOne(r)}>
            {t('ap.importAction')}
          </Button>
        ) : (
          // Not a disabled button: a control nobody here can press is not drawn (ADR-056). The
          // word still has to appear, or the column reads as "imported" for every row.
          <span className="nd-muted">{t('ap.import.not_imported')}</span>
        ),
    },
  ];
  for (const c of columns) c.filter = specs[c.key];

  const { filterCols, filters, setFilters, clear, shown, counts, anyFiltered } = useClientFilters(
    columns,
    rows,
    { prefix: nodeTabFilterPrefix('ap') },
  );

  const overCap = apsOverCap(summary);

  return (
    <div className="nd-ap">
      <ApSettings
        node={node}
        groups={groups}
        summary={summary}
        canConfig={canConfig}
        onSaved={() => {
          setNonce((v) => v + 1);
          onChanged();
        }}
      />

      {error && <p className="form-error nd-tabpad">{error}</p>}
      {note && <p className="nd-ap-ok nd-tabpad">✓ {note}</p>}
      {overCap > 0 && (
        <p className="form-error nd-tabpad">{t('ap.overCap', { count: overCap })}</p>
      )}
      {/* The cap is the page size, so this cannot happen by design — which is exactly why it is
          printed rather than dropped. A silent first page presented as the whole inventory is the
          failure this list could not otherwise show. */}
      {page?.next && <p className="form-error nd-tabpad">{t('ap.morePages')}</p>}

      <TableToolbar>
        <FilterButton columns={filterCols} filters={filters} onOpen={() => setSheet(true)} />
        <ClearFilters columns={filterCols} filters={filters} onClear={clear} />
        <TableSpacer />
        <span className="nd-muted">{t('ap.summary', { count: rows.length })}</span>
      </TableToolbar>
      <div className="nd-ap-table">
        <DataTable
          tableId="node.wirelessAps"
          rows={shown}
          columns={columns}
          filters={filters}
          onFiltersChange={setFilters}
          filterCounts={counts}
          rowKey={(r) => r.ap_id}
          loading={!loaded}
          empty={
            anyFiltered
              ? t('common:filter.noMatch')
              : awaitingFirstInventory(summary)
                ? t('ap.empty.noInventory')
                : t('ap.empty.none')
          }
        />
      </div>
      {sheet && (
        <MobileFilterSheet
          columns={filterCols}
          filters={filters}
          onChange={setFilters}
          counts={counts}
          labels={{
            ap: t('ap.colAp'),
            state: t('ap.colState'),
            imported: t('ap.colImported'),
          }}
          onClose={() => setSheet(false)}
        />
      )}
    </div>
  );
}

/** Which controllers see this AP. One name normally; an HA pair puts the serving member first and
 *  folds the rest behind `+N`, because that number is the answer to "whose numbers am I reading". */
function ReportedBy({ row }: { row: WirelessApRow }) {
  const { nodeName } = useEntityNames();
  const { serving, others } = reportingControllers(row);
  const all = serving ? [serving, ...others] : others;
  if (all.length === 0) return <span className="nd-muted">—</span>;
  const head = all[0];
  const name = nodeName(head);
  const title = all.map((id) => nodeName(id)).join(', ');
  return (
    <span className="nd-ap-reported" title={title}>
      {isEntityResolved(name, head) ? (
        <Link to={nodesPageHref({ kind: 'node', id: head })}>{name}</Link>
      ) : (
        <EntityName name={name} id={head} />
      )}
      {all.length > 1 && <span className="nd-muted">+{all.length - 1}</span>}
    </span>
  );
}

interface SettingsProps {
  node: NodeDetailData;
  groups: NodeGroup[];
  summary: NonNullable<NodeDetailData['wireless']>['controller'] | null;
  canConfig: boolean;
  onSaved: () => void;
}

/** The two knobs that decide whether APs become nodes at all, and where they land.
 *
 *  ⚠️ The form follows the server until the operator touches it. A refresh tick that re-read the
 *  settings mid-edit would otherwise put the stored value back under a half-typed cap. */
function ApSettings({ node, groups, summary, canConfig, onSaved }: SettingsProps) {
  const { t } = useTranslation('nodes');
  const [edited, setEdited] = useState(false);
  const [importAps, setImportAps] = useState(summary?.import_aps ?? false);
  const [maxAps, setMaxAps] = useState(String(summary?.max_aps ?? MAX_APS_DEFAULT));
  const [group, setGroup] = useState(summary?.ap_group_id ?? '');
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    if (edited) return;
    setImportAps(summary?.import_aps ?? false);
    setMaxAps(String(summary?.max_aps ?? MAX_APS_DEFAULT));
    setGroup(summary?.ap_group_id ?? '');
  }, [edited, summary?.import_aps, summary?.max_aps, summary?.ap_group_id]);

  const parsedMax = Number.parseInt(maxAps, 10);
  const maxValid = Number.isInteger(parsedMax) && parsedMax >= MAX_APS_MIN && parsedMax <= MAX_APS_HARD;

  const save = () => {
    if (!maxValid) return;
    setBusy(true);
    setError(null);
    api
      .setWirelessController(node.id, {
        import_aps: importAps,
        max_aps: parsedMax,
        ap_group_id: group === '' ? null : group,
      })
      .then(() => {
        setEdited(false);
        onSaved();
      })
      .catch((e: unknown) => setError(errMsg(e, t('ap.err.save'))))
      .finally(() => setBusy(false));
  };

  return (
    <Card className="nd-ap-settings" title={t('ap.settings.title')}>
      <p className="nd-muted nd-ap-stats">
        <span>{t('ap.stats.reported', { count: summary?.aps_reported ?? 0 })}</span>
        <span>
          {summary?.last_inventory_at
            ? t('ap.stats.lastInventory', { when: relativeTime(summary.last_inventory_at) })
            : t('ap.stats.neverInventoried')}
        </span>
      </p>

      {canConfig ? (
        <>
          <div className="nd-ap-form">
            <label className="form-label nd-ap-switch">
              <input
                type="checkbox"
                checked={importAps}
                disabled={busy}
                onChange={(e) => {
                  setEdited(true);
                  setImportAps(e.target.checked);
                }}
              />
              {t('ap.settings.importAps')}
            </label>
            <label className="form-label" htmlFor="ap-max">
              {t('ap.settings.maxAps')}
              <TextInput
                id="ap-max"
                type="number"
                min={MAX_APS_MIN}
                max={MAX_APS_HARD}
                value={maxAps}
                disabled={busy}
                onChange={(e) => {
                  setEdited(true);
                  setMaxAps(e.target.value);
                }}
              />
            </label>
            <label className="form-label" htmlFor="ap-group">
              {t('ap.settings.group')}
              <GroupPicker
                id="ap-group"
                options={groupOptions(groups)}
                value={group}
                onChange={(id) => {
                  setEdited(true);
                  setGroup(id);
                }}
                emptyOption={t('ap.settings.groupDefault')}
                disabled={busy}
              />
            </label>
          </div>
          <p className="form-hint">{t('ap.settings.hint')}</p>
          {!maxValid && (
            <p className="form-error">{t('ap.err.maxAps', { min: MAX_APS_MIN, max: MAX_APS_HARD })}</p>
          )}
          {error && <p className="form-error">{error}</p>}
          <div className="nd-ap-save">
            <Button variant="primary" disabled={busy || !edited || !maxValid} onClick={save}>
              {t('ap.settings.save')}
            </Button>
          </div>
        </>
      ) : (
        <PermissionHint permission="manage_config" signInHint={t('ap.signIn')} />
      )}
    </Card>
  );
}
