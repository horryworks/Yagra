// SPDX-License-Identifier: AGPL-3.0-only
// The node's VLANs by name (ADR-257): the ones its VLAN table defines and the ones its ports use,
// with how many ports carry each and where each is routed. A count opens Interfaces filtered to
// exactly those ports; the arithmetic is `vlanRows.ts`, where a test reaches it.

import { useMemo } from 'react';
import { useTranslation } from 'react-i18next';
import { Link, useSearchParams } from 'react-router-dom';
import type { InterfaceRow, NodeVlan } from '../../types/api';
import { DataTable, type Column } from '../ui/DataTable';
import { ListToolbar } from '../ui/ListToolbar';
import { EntityName } from '../ui/EntityName';
import { Inline } from '../ui/Page';
import { useEntityNames } from '../ui/entityNames';
import { useClientFilters } from '../../lib/useClientFilters';
import { useClientSort } from '../../lib/useClientSort';
import { nodeHref } from '../../lib/entityHref';
import { vlanFilters } from './tabFilters';
import { nodeTabFilterPrefix } from './tabs';
import {
  gatewayAddresses,
  interfacesSearch,
  isUndefinedVlan,
  PORT_GROUPS,
  vlanPortCounts,
  type PortGroup,
} from './vlanRows';

interface Props {
  rows: NodeVlan[];
  loaded: boolean;
  error: string | null;
  /** The node's interfaces, already loaded for the Interfaces tab — what the counts are taken from. */
  interfaces: InterfaceRow[];
}

export function VlansTab({ rows, loaded, error, interfaces }: Props) {
  const { t } = useTranslation('nodes');
  const [searchParams] = useSearchParams();
  const { nodeName } = useEntityNames();
  const counts = useMemo(() => vlanPortCounts(interfaces), [interfaces]);

  const countCell = (v: NodeVlan, group: PortGroup) => {
    const n = counts.get(v.vlan_id)?.[group] ?? 0;
    if (n === 0) return <span className="nd-muted">0</span>;
    return (
      <Link className="nd-nb-link" to={{ search: interfacesSearch(searchParams, v.vlan_id, group) }}>
        {n}
      </Link>
    );
  };

  const specs = vlanFilters(t);
  const columns: Column<NodeVlan>[] = [
    {
      key: 'vlan',
      header: t('vlans.colVlan'),
      width: '80px',
      align: 'right',
      sortValue: (v) => v.vlan_id,
      render: (v) => <span className="mono">{v.vlan_id}</span>,
    },
    {
      key: 'name',
      header: t('vlans.colName'),
      width: '1.4fr',
      sortValue: (v) => v.name ?? null,
      render: (v) => (
        <Inline as="span" gap={6}>
          {v.name ? (
            <span title={v.name}>{v.name}</span>
          ) : (
            !isUndefinedVlan(v) && <span className="nd-muted">—</span>
          )}
          {isUndefinedVlan(v) && (
            <span className="badge badge-warning">{t('vlans.notDefined')}</span>
          )}
          {v.state === 'suspended' && <span className="badge">{t('vlans.suspended')}</span>}
        </Inline>
      ),
    },
    ...PORT_GROUPS.map(
      (group): Column<NodeVlan> => ({
        key: group,
        header: t(`vlans.col.${group}`),
        width: '120px',
        align: 'right',
        sortValue: (v) => counts.get(v.vlan_id)?.[group] ?? 0,
        render: (v) => countCell(v, group),
      }),
    ),
    {
      key: 'gateway',
      header: t('vlans.colGateway'),
      width: '1.8fr',
      sortValue: (v) => v.gateway?.if_name ?? v.subnet ?? null,
      render: (v) => {
        const gw = v.gateway;
        const addrs = gatewayAddresses(v);
        if (!gw) return v.subnet ? <span className="mono nd-muted">{v.subnet}</span> : null;
        return (
          <Inline as="span" gap={8}>
            {gw.node_id ? (
              <Link className="nd-nb-link" to={nodeHref(gw.node_id)}>
                <EntityName name={nodeName(gw.node_id)} id={gw.node_id} />
              </Link>
            ) : (
              gw.if_name && <span className="mono">{gw.if_name}</span>
            )}
            {addrs && (
              <span className="mono nd-muted" title={addrs}>
                {addrs}
              </span>
            )}
          </Inline>
        );
      },
    },
  ];
  for (const c of columns) c.filter = specs[c.key];

  const filtering = useClientFilters(columns, rows, { prefix: nodeTabFilterPrefix('vlans') });
  const sorting = useClientSort(columns, filtering.shown, { prefix: nodeTabFilterPrefix('vlans') });

  return (
    <div className="nd-nb">
      {/* A failed re-read keeps the rows it had, so the error goes above them; with no rows it is
          the table's own empty line instead, never beside "no VLANs read yet". */}
      {error && rows.length > 0 && <p className="form-error nd-tabpad">{error}</p>}
      <ListToolbar
        list={filtering}
        labels={{
          vlan: t('vlans.colVlan'),
          name: t('vlans.colName'),
          gateway: t('vlans.colGateway'),
        }}
      />
      {/* The Neighbors tab's frame and link colour (`NeighborsTab.css`, loaded with the node detail):
          a flex column of a fixed height, so the table scrolls inside. */}
      <div className="nd-nb-table">
        <DataTable
          tableId="node.vlans"
          rows={sorting.rows}
          columns={sorting.columns}
          sort={sorting.sort}
          onSortChange={sorting.setSort}
          filters={filtering.filters}
          onFiltersChange={filtering.setFilters}
          filterCounts={filtering.counts}
          rowKey={(v) => String(v.vlan_id)}
          loading={!loaded}
          empty={error ?? (filtering.anyFiltered ? t('common:filter.noMatch') : t('vlans.empty'))}
        />
      </div>
    </div>
  );
}
