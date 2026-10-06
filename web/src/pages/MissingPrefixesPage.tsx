// SPDX-License-Identifier: AGPL-3.0-only
// Nodes ▸ Missing IP prefixes (ADR-170 Inc.2). For every site, the subnets its devices carry that
// none of its IP prefixes covers — the folder pane's "Subnets missing from the IP prefixes", for
// the whole fleet at once. Opens by site (ADR-170 decision 11); the subnet list is one switch away.
// One gap at a time can be marked as intentional, from the row (ADR-170 Inc.4) — the same button,
// in the same place, as Subnet overlaps. The judgement is in `missingPrefixes.ts`.

import { useMemo, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { Link } from 'react-router-dom';
import { api, errMsg } from '../services/api';
import { useCan, useScope } from '../store';
import type { PrefixGap, PrefixGapSitesView, SiteGapStatus, SitePrefixGaps } from '../types/api';
import { PageHeader } from '../components/ui/PageHeader';
import { Button } from '../components/ui/Button';
import { Tabs } from '../components/ui/Tabs';
import { Segmented } from '../components/ui/Segmented';
import { SearchField } from '../components/ui/SearchField';
import { ListToolbar } from '../components/ui/ListToolbar';
import { DataTable, type Column } from '../components/ui/DataTable';
import { LoadGate } from '../components/ui/LoadGate';
import { useEntityNames } from '../components/ui/entityNames';
import { useLoad } from '../lib/useLoad';
import { useClientFilters } from '../lib/useClientFilters';
import { columnLabels } from '../lib/listToolbar';
import { saveBlob } from '../lib/download';
import { nodeHref, nodesPageHref } from '../lib/entityHref';
import {
  gapReason,
  moreDevices,
  PREFIX_GAP_KINDS,
  type PrefixGapKind,
} from '../components/NodeDetail/prefixGaps';
import {
  allSubnetRows,
  GAP_STATUSES,
  gapStatus,
  gapStatusCounts,
  gapsCsv,
  kindCounts,
  openGaps,
  MISSING_PREFIX_VIEWS,
  SITE_GAP_STATUSES,
  siteKey,
  siteRowFilters,
  sitesOn,
  statusCounts,
  SUBNET_FILTER_PREFIX,
  matchingSubnetRows,
  subnetRowFilters,
  type GapStatus,
  type MissingPrefixView,
  type SiteRow,
  type SubnetRow,
} from './missingPrefixes';
import { AckGapModal } from './MissingPrefixModals';
import './MissingPrefixesPage.css';

/** The opened site's subnets: one template for the header and every row (table.css). The last
 *  track is the mark button, drawn only for someone who may press it. */
const GAP_COLS = '150px minmax(0, 1.2fr) minmax(0, 2fr) 72px minmax(0, 1fr)';
const GAP_COLS_ACT = `${GAP_COLS} 180px`;

/** What the view not on screen is given — one identity, so its memos do not re-run. */
const NO_SITES: SiteRow[] = [];
const NO_SUBNETS: SubnetRow[] = [];

export function MissingPrefixesPage() {
  const { t } = useTranslation('monitoring');
  const { nodeName } = useEntityNames();
  const [view, setView] = useState<MissingPrefixView>('site');
  const [tab, setTab] = useState<SiteGapStatus>('gaps');
  const [kind, setKind] = useState<PrefixGapKind | null>(null);
  const [q, setQ] = useState('');
  const [openKey, setOpenKey] = useState<string | null>(null);
  const [gapTab, setGapTab] = useState<GapStatus>('open');
  const [acking, setAcking] = useState<{ site: SitePrefixGaps; gap: PrefixGap } | null>(null);
  const [actionError, setActionError] = useState<string | null>(null);
  // A mark silences the gap for the whole site, so it is refused to a folder-scoped caller —
  // Subnet overlaps' rule, and the button is drawn for the same people on both screens.
  const mayConfig = useCan('manage_config');
  const scope = useScope();
  const canConfig = mayConfig && scope === 'All';

  const load = useLoad(() => api.getSitePrefixGaps(), [], {
    initial: null as PrefixGapSitesView | null,
  });
  const { data, loading, reload } = load;

  const filter = useMemo(() => ({ kind, q }), [kind, q]);
  // Only the view on screen builds and filters its rows; the subnet list is sorted once per
  // answer and filtered per keystroke (ADR-170 Inc.3). `gapRows` is still built in the site view
  // because Export CSV saves it from either view, and filtering it is linear.
  const siteRows = useMemo(
    () => (view === 'site' ? sitesOn(data, tab, filter, nodeName) : NO_SITES),
    [view, data, tab, filter, nodeName],
  );
  const allSubnets = useMemo(() => allSubnetRows(data), [data]);
  const gapRows = useMemo(
    () => matchingSubnetRows(allSubnets, filter, nodeName),
    [allSubnets, filter, nodeName],
  );
  const gapTabRows = useMemo(
    () => gapRows.filter((r) => gapStatus(r.gap) === gapTab),
    [gapRows, gapTab],
  );
  const gapTabs = useMemo(() => gapStatusCounts(allSubnets), [allSubnets]);
  const tiles = useMemo(() => kindCounts(openGaps(data)), [data]);
  const tabs = useMemo(() => statusCounts(data), [data]);

  const siteLabel = (s: SitePrefixGaps) => s.name ?? t('missingPrefixes.root');
  const reason = (g: PrefixGap) => {
    const r = gapReason(g);
    return t(`nodes:${r.key}`, r.values);
  };
  const reset = () => setOpenKey(null);

  const reopen = (s: SitePrefixGaps, g: PrefixGap) => {
    setActionError(null);
    api
      .unackPrefixGap(s.site_id ?? null, g.subnet)
      .then(() => reload())
      .catch((e: unknown) => setActionError(errMsg(e, t('missingPrefixes.reopenErr'))));
  };

  // The button sits inside a row that opens on click; pressing it must not open the row too.
  const markButton = (s: SitePrefixGaps, g: PrefixGap) =>
    g.intentional ? (
      <Button
        variant="outline"
        onClick={(e) => {
          e.stopPropagation();
          reopen(s, g);
        }}
      >
        {t('missingPrefixes.actions.reopen')}
      </Button>
    ) : (
      <Button
        variant="outline"
        onClick={(e) => {
          e.stopPropagation();
          setAcking({ site: s, gap: g });
        }}
      >
        {t('missingPrefixes.actions.intentional')}
      </Button>
    );

  const seenOn = (g: PrefixGap) => {
    const more = moreDevices(g);
    return (
      <span className="mp-seen">
        {g.seen_on.map((s) => (
          <span key={`${s.node_id}-${s.ifindex}-${s.ip}`}>
            <Link to={nodeHref(s.node_id)}>{nodeName(s.node_id)}</Link>
            <span className="mono">
              {s.if_name ? ` ${s.if_name}` : ''} {s.ip}
            </span>
          </span>
        ))}
        {more > 0 && <span className="muted">{t('missingPrefixes.moreDevices', { n: more })}</span>}
      </span>
    );
  };

  const links = (g: PrefixGap, s: SitePrefixGaps) => (
    <span className="mp-links">
      {s.site_id && (
        <Link to={nodesPageHref({ kind: 'group', id: s.site_id })}>
          {t('missingPrefixes.openFolder', { name: siteLabel(s) })}
        </Link>
      )}
      {g.kind === 'other_folder' && (
        <Link to="/nodes/subnet-overlaps">{t('missingPrefixes.seeOverlaps')}</Link>
      )}
    </span>
  );

  const noteCell = (g: PrefixGap) => {
    const note = g.intentional?.note || t('missingPrefixes.noNote');
    return (
      <span className={g.intentional?.note ? 'mp-note' : 'mp-note muted'} title={note}>
        {note}
      </span>
    );
  };

  const kindCell = (g: PrefixGap) => (
    <span className={`mp-kind mp-kind-${g.kind}`} title={reason(g)}>
      {reason(g)}
    </span>
  );

  const siteColumns = useMemo<Column<SiteRow>[]>(() => {
    const cols: Column<SiteRow>[] = [
      {
        key: 'site',
        header: t('missingPrefixes.cols.site'),
        width: '1.2fr',
        render: ({ site }) => (
          <span className="mp-site" title={[...site.path, siteLabel(site)].join(' / ')}>
            <span>
              <span className="mp-caret" aria-hidden="true">
                {openKey === siteKey(site) ? '▾' : '▸'}
              </span>{' '}
              {siteLabel(site)}
              {!site.is_site && site.site_id && (
                <span className="mp-tag">{t('missingPrefixes.notSite')}</span>
              )}
            </span>
            {site.path.length > 0 && <span className="mp-path">{site.path.join(' / ')}</span>}
          </span>
        ),
      },
      {
        key: 'gaps',
        header: t('missingPrefixes.cols.gaps'),
        width: '2fr',
        render: ({ site, gaps }) => {
          if (site.status === 'no_data') {
            return <span className="muted">{t('missingPrefixes.noData')}</span>;
          }
          if (site.status === 'clean') {
            return (
              <span className="mp-clean">
                {t('missingPrefixes.clean', { subnets: site.subnets_checked })}
              </span>
            );
          }
          const counts = kindCounts(gaps);
          return (
            <span className="mp-chips">
              {PREFIX_GAP_KINDS.filter((k) => counts[k] > 0).map((k) => (
                <span key={k} className={`mp-chip mp-kind-${k}`}>
                  {t(`nodes:prefixGaps.kind.${k}`)} {counts[k]}
                </span>
              ))}
            </span>
          );
        },
      },
      {
        key: 'read',
        header: t('missingPrefixes.cols.read'),
        width: '170px',
        render: ({ site }) =>
          t('missingPrefixes.readCell', {
            read: site.nodes_with_addresses,
            total: site.nodes_total,
          }),
      },
      {
        key: 'prefixes',
        header: t('missingPrefixes.cols.prefixes'),
        width: '110px',
        align: 'right',
        render: ({ site }) => site.prefixes,
      },
    ];
    const filters = siteRowFilters(t);
    for (const c of cols) c.filter = filters[c.key];
    return cols;
    // eslint-disable-next-line react-hooks/exhaustive-deps -- siteLabel reads only `t`
  }, [t, openKey]);

  const subnetColumns = useMemo<Column<SubnetRow>[]>(() => {
    const cols: Column<SubnetRow>[] = [
      {
        key: 'subnet',
        header: t('missingPrefixes.cols.subnet'),
        width: '190px',
        render: (r) => (
          <span className="mono" title={r.gap.subnet}>
            <span className="mp-caret" aria-hidden="true">
              {openKey === r.key ? '▾' : '▸'}
            </span>{' '}
            {r.gap.subnet}
          </span>
        ),
      },
      { key: 'reason', header: t('missingPrefixes.cols.reason'), width: '2fr', render: (r) => kindCell(r.gap) },
      {
        key: 'site',
        header: t('missingPrefixes.cols.site'),
        width: '1fr',
        render: (r) => <span title={[...r.site.path, siteLabel(r.site)].join(' / ')}>{siteLabel(r.site)}</span>,
      },
      {
        key: 'devices',
        header: t('missingPrefixes.cols.devices'),
        width: '90px',
        align: 'right',
        render: (r) => r.gap.node_count,
      },
    ];
    if (canConfig) {
      cols.push({
        key: 'action',
        header: t('missingPrefixes.cols.action'),
        width: '180px',
        align: 'right',
        render: (r) => markButton(r.site, r.gap),
      });
    }
    const filters = subnetRowFilters(t);
    for (const c of cols) c.filter = filters[c.key];
    return cols;
    // eslint-disable-next-line react-hooks/exhaustive-deps -- kindCell, siteLabel and markButton read only `t` and `reload`
  }, [t, openKey, canConfig]);
  const siteFiltering = useClientFilters(siteColumns, siteRows);
  const subnetFiltering = useClientFilters(subnetColumns, view === 'subnet' ? gapTabRows : NO_SUBNETS, {
    prefix: SUBNET_FILTER_PREFIX,
  });
  const anyFiltered = view === 'site' ? siteFiltering.anyFiltered : subnetFiltering.anyFiltered;

  const gapCols = canConfig ? GAP_COLS_ACT : GAP_COLS;
  const siteDetail = ({ site, gaps }: SiteRow) => (
    <div className="mp-detail">
      {site.nodes_truncated > 0 && (
        <p className="muted">{t('missingPrefixes.truncatedNodes', { n: site.nodes_truncated })}</p>
      )}
      {gaps.length === 0 ? (
        <p className="muted">{t('missingPrefixes.nothingHere')}</p>
      ) : (
        // `.ytable` rather than a `<table>`: it stacks into cards on a phone with no markup of
        // its own, where a five-column table scrolled sideways. Bounded by the server's cut
        // (`siteCut` below), so it needs no virtualization (ui-conventions.md, hand-rolled tables).
        <div className="ytable mp-gaps">
          <div className="ytable-head" style={{ gridTemplateColumns: gapCols }}>
            <div className="ytable-h">{t('missingPrefixes.cols.subnet')}</div>
            <div className="ytable-h">{t('missingPrefixes.cols.reason')}</div>
            <div className="ytable-h">
              {tab === 'intentional' ? t('missingPrefixes.cols.note') : t('missingPrefixes.cols.seenOn')}
            </div>
            <div className="ytable-h right">{t('missingPrefixes.cols.devices')}</div>
            <div className="ytable-h" />
            {canConfig && <div className="ytable-h right">{t('missingPrefixes.cols.action')}</div>}
          </div>
          {gaps.map((g) => (
            <div className="ytable-row" key={g.subnet} style={{ gridTemplateColumns: gapCols }}>
              <div className="ytable-cell mono">{g.subnet}</div>
              <div className="ytable-cell">{kindCell(g)}</div>
              <div className="ytable-cell">{g.intentional ? noteCell(g) : seenOn(g)}</div>
              <div className="ytable-cell right num">{g.node_count}</div>
              <div className="ytable-cell">{links(g, site)}</div>
              {canConfig && <div className="ytable-cell right">{markButton(site, g)}</div>}
            </div>
          ))}
        </div>
      )}
      {site.gap_count > site.gaps.length && (
        <p className="muted">
          {t('missingPrefixes.siteCut', { shown: site.gaps.length, total: site.gap_count })}
        </p>
      )}
    </div>
  );

  const subnetDetail = (r: SubnetRow) => (
    <div className="mp-detail">
      {r.gap.intentional && noteCell(r.gap)}
      {seenOn(r.gap)}
      {links(r.gap, r.site)}
    </div>
  );

  const exportCsv = () =>
    saveBlob(
      new Blob([gapsCsv(gapRows, nodeName)], { type: 'text/csv;charset=utf-8' }),
      'missing-ip-prefixes.csv',
    );

  const emptyText =
    q || kind || anyFiltered
      ? t('missingPrefixes.emptyFiltered')
      : view === 'site'
        ? t(`missingPrefixes.empty.${tab}`)
        : gapTab === 'intentional'
          ? t('missingPrefixes.empty.intentional')
          : t('missingPrefixes.empty.gaps');

  // One toolbar, two filter states: each layout keeps its own (the subnet one under a prefix).
  const toolbarLeading = (
    <>
      <Segmented
        options={MISSING_PREFIX_VIEWS.map((v) => ({ value: v, label: t(`missingPrefixes.views.${v}`) }))}
        value={view}
        onChange={(v) => {
          setView(v as MissingPrefixView);
          reset();
        }}
        ariaLabel={t('missingPrefixes.viewsLabel')}
      />
      {view === 'site' ? (
        <Tabs
          tabs={SITE_GAP_STATUSES.map((s) => ({
            key: s,
            label: t(`missingPrefixes.tabs.${s}`),
            count: data ? tabs[s] : undefined,
          }))}
          active={tab}
          onChange={(s) => {
            setTab(s);
            reset();
          }}
        />
      ) : (
        <Tabs
          tabs={GAP_STATUSES.map((s) => ({
            key: s,
            label: t(`missingPrefixes.gapTabs.${s}`),
            count: data ? gapTabs[s] : undefined,
          }))}
          active={gapTab}
          onChange={(s) => {
            setGapTab(s);
            reset();
          }}
        />
      )}
    </>
  );
  const toolbarTools = (
    <>
      <SearchField
        boxClassName="mp-search"
        value={q}
        onChange={(e) => {
          setQ(e.target.value);
          reset();
        }}
        onClear={() => setQ('')}
        placeholder={t('missingPrefixes.search')}
      />
      <Button variant="outline" onClick={exportCsv} disabled={gapRows.length === 0}>
        {t('missingPrefixes.csv')}
      </Button>
    </>
  );

  return (
    <div>
      <PageHeader
        title={t('nav:nodes.missingPrefixes')}
        trail={[{ label: t('nav:sections.nodes') }, { label: t('nav:nodes.missingPrefixes') }]}
      />

      <LoadGate load={load} permission="view">
        {data && (
          <p className="mp-coverage">
            {t('missingPrefixes.coverage', {
              sites: data.sites.length - tabs.no_data,
              read: data.nodes_with_addresses,
              total: data.nodes_total,
              subnets: data.subnets_checked,
            })}
            {tabs.no_data > 0 && ` ${t('missingPrefixes.notCompared', { n: tabs.no_data })}`}
            {data.nodes_truncated > 0 &&
              ` ${t('missingPrefixes.truncatedNodes', { n: data.nodes_truncated })}`}
          </p>
        )}
        {data && data.gaps_listed < data.gaps_total && (
          <p className="muted">
            {t('missingPrefixes.cut', { shown: data.gaps_listed, total: data.gaps_total })}
          </p>
        )}

        <div className="mp-tiles" role="group" aria-label={t('missingPrefixes.tilesLabel')}>
          {PREFIX_GAP_KINDS.map((k) => (
            <button
              key={k}
              type="button"
              className={`mp-tile mp-kind-${k}`}
              aria-pressed={kind === k}
              onClick={() => {
                setKind(kind === k ? null : k);
                setTab('gaps');
                setGapTab('open');
                reset();
              }}
            >
              <span className="mp-tile-n">{tiles[k]}</span>
              <span className="mp-tile-t">{t(`nodes:prefixGaps.kind.${k}`)}</span>
            </button>
          ))}
        </div>

        {view === 'site' ? (
          <ListToolbar list={siteFiltering} labels={columnLabels(siteColumns)} leading={toolbarLeading}>
            {toolbarTools}
          </ListToolbar>
        ) : (
          <ListToolbar list={subnetFiltering} labels={columnLabels(subnetColumns)} leading={toolbarLeading}>
            {toolbarTools}
          </ListToolbar>
        )}

        {actionError && (
          <p className="form-error" role="alert">
            {actionError}
          </p>
        )}

        {view === 'site' ? (
          <DataTable
            tableId="nodes.missingPrefixes"
            rows={siteFiltering.shown}
            columns={siteColumns}
            filters={siteFiltering.filters}
            onFiltersChange={siteFiltering.setFilters}
            filterCounts={siteFiltering.counts}
            rowKey={(r) => siteKey(r.site)}
            onRowClick={(r) => setOpenKey(openKey === siteKey(r.site) ? null : siteKey(r.site))}
            expanded={(r) => (siteKey(r.site) === openKey ? siteDetail(r) : null)}
            expandedKey={openKey}
            loading={loading}
            empty={emptyText}
          />
        ) : (
          <DataTable
            tableId="nodes.missingPrefixesSubnets"
            rows={subnetFiltering.shown}
            columns={subnetColumns}
            filters={subnetFiltering.filters}
            onFiltersChange={subnetFiltering.setFilters}
            filterCounts={subnetFiltering.counts}
            rowKey={(r) => r.key}
            onRowClick={(r) => setOpenKey(openKey === r.key ? null : r.key)}
            expanded={(r) => (r.key === openKey ? subnetDetail(r) : null)}
            expandedKey={openKey}
            loading={loading}
            empty={emptyText}
          />
        )}
      </LoadGate>

      {acking && (
        <AckGapModal
          site={acking.site}
          siteName={siteLabel(acking.site)}
          gap={acking.gap}
          onClose={() => setAcking(null)}
          onDone={() => {
            setAcking(null);
            reload();
          }}
        />
      )}
    </div>
  );
}
