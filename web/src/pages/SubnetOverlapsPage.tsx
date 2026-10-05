// SPDX-License-Identifier: AGPL-3.0-only
// Nodes ▸ Subnet overlaps (ADR-187). Address ranges that devices at more than one site carry —
// the same range at two sites, or one site's range inside another's — read from the interface
// addresses the devices already report. Nothing is polled for this screen.
//
// What repeats on purpose is taken out two ways, and both are the operator's call: a rule (a WAN
// port, a range) moves every overlap it explains to Excluded, and "Mark as intentional" moves one
// overlap until another site joins it. A hint ("looks like WAN") is only ever a suggestion — it
// pre-fills a rule, it never applies one. The judgement is in `subnetOverlaps.ts`.

import { useMemo, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { Link } from 'react-router-dom';
import { api, errMsg } from '../services/api';
import { useCan, useScope } from '../store';
import {
  OVERLAP_KINDS,
  OVERLAP_STATUSES,
  type OverlapKind,
  type OverlapRuleBody,
  type OverlapStatus,
  type SubnetOverlap,
  type SubnetOverlapsView,
} from '../types/api';
import { PageHeader } from '../components/ui/PageHeader';
import { Button } from '../components/ui/Button';
import { Tabs } from '../components/ui/Tabs';
import { ListToolbar } from '../components/ui/ListToolbar';
import { DataTable, type Column } from '../components/ui/DataTable';
import { useLoad } from '../lib/useLoad';
import { useClientFilters } from '../lib/useClientFilters';
import { columnLabels } from '../lib/listToolbar';
import { LoadGate } from '../components/ui/LoadGate';
import { nodeHref } from '../lib/entityHref';
import {
  emptyKey,
  openKindCounts,
  overlapFilters,
  overlapsOn,
  rangeBars,
  suggestedRule,
  visibleSites,
} from './subnetOverlaps';
import { AckOverlapModal, OverlapRuleModal, OverlapRulesModal } from './SubnetOverlapModals';
import './SubnetOverlapsPage.css';

const SITES_SHOWN = 3;

export function SubnetOverlapsPage() {
  const { t } = useTranslation('monitoring');
  // Every write here is refused to a folder-scoped caller (an overlap spans sites it cannot see),
  // so the permission alone does not make a control usable.
  const mayConfig = useCan('manage_config');
  const scope = useScope();
  const canConfig = mayConfig && scope === 'All';
  const scopedOut = mayConfig && scope !== null && scope !== 'All';
  const [tab, setTab] = useState<OverlapStatus>('open');
  const [kind, setKind] = useState<OverlapKind | null>(null);
  const [openKey, setOpenKey] = useState<string | null>(null);
  const [rulesOpen, setRulesOpen] = useState(false);
  const [newRule, setNewRule] = useState<OverlapRuleBody | null | undefined>(undefined);
  const [acking, setAcking] = useState<SubnetOverlap | null>(null);
  const [actionError, setActionError] = useState<string | null>(null);

  const load = useLoad(() => api.getSubnetOverlaps(), [], {
    initial: null as SubnetOverlapsView | null,
  });
  const { data: view, loading, reload } = load;

  const rows = useMemo(() => overlapsOn(view, tab, kind), [view, tab, kind]);
  const kindCounts = openKindCounts(view);
  const empty = emptyKey(view, tab);

  const reopen = (o: SubnetOverlap) => {
    setActionError(null);
    api
      .unackOverlap(o.key)
      .then(() => reload())
      .catch((e: unknown) => setActionError(errMsg(e, t('subnetOverlaps.reopenErr'))));
  };

  const ruleName = (id: string) => {
    const r = view?.rules.find((x) => x.id === id);
    if (!r) return id;
    return ruleText(r.range ?? null, r.port_text ?? null);
  };
  const ruleText = (range: string | null, text: string | null) =>
    range && text
      ? t('subnetOverlaps.rules.both', { range, text })
      : range
        ? t('subnetOverlaps.rules.range', { range })
        : t('subnetOverlaps.rules.port', { text: text ?? '' });

  const hintText = (o: SubnetOverlap) => {
    const h = o.hint;
    if (!h) return null;
    switch (h.kind) {
      case 'wan':
      case 'redundancy':
        return t(`subnetOverlaps.hint.${h.kind}`, { word: h.word });
      case 'shared_line':
      case 'template':
        return t(`subnetOverlaps.hint.${h.kind}`, { count: o.site_count });
      default: {
        const never: never = h;
        return never;
      }
    }
  };

  const whyCell = (o: SubnetOverlap) => {
    if (o.status === 'excluded') {
      const text = o.excluded_by
        .map((x) =>
          x.kind === 'link'
            ? t('subnetOverlaps.exclusion.link')
            : t('subnetOverlaps.exclusion.rule', { rule: ruleName(x.rule_id) }),
        )
        .join(' · ');
      return <span title={text}>{text}</span>;
    }
    if (o.status === 'intentional') {
      const note = o.note || t('subnetOverlaps.noNote');
      return <span title={note}>{note}</span>;
    }
    const hint = hintText(o);
    return hint ? (
      <span className="so-hint">
        <span className="so-hint-tag">{t(`subnetOverlaps.hintTag.${o.hint?.kind ?? 'wan'}`)}</span>
        <span className="so-hint-text" title={hint}>
          {hint}
        </span>
      </span>
    ) : (
      <span className="muted">{t('subnetOverlaps.noHint')}</span>
    );
  };

  const columns = useMemo<Column<SubnetOverlap>[]>(() => {
    const cols: Column<SubnetOverlap>[] = [
      {
        key: 'subnet',
        header: t('subnetOverlaps.cols.subnet'),
        width: '200px',
        render: (o) => (
          <span className="mono" title={o.subnet}>
            <span className="so-caret" aria-hidden="true">
              {openKey === o.key ? '▾' : '▸'}
            </span>{' '}
            {o.subnet}
          </span>
        ),
      },
      {
        key: 'kind',
        header: t('subnetOverlaps.cols.kind'),
        width: '210px',
        render: (o) => (
          <span className={`so-kind so-kind-${o.kind}`}>{t(`subnetOverlaps.kind.${o.kind}`)}</span>
        ),
      },
      {
        key: 'sites',
        header: t('subnetOverlaps.cols.sites'),
        width: '1.4fr',
        render: (o) => {
          const sites = visibleSites(o);
          const names = sites.map((s) => s.name ?? t('subnetOverlaps.root'));
          const more = o.site_count - o.hidden_sites - Math.min(SITES_SHOWN, sites.length);
          const hidden =
            o.hidden_sites > 0 ? t('subnetOverlaps.hiddenSites', { count: o.hidden_sites }) : null;
          const all = [...names, ...(hidden ? [hidden] : [])].join(', ');
          return (
            <span className="so-sites" title={all}>
              {names.slice(0, SITES_SHOWN).map((n, i) => (
                <span className="so-chip" key={`${n}-${i}`}>
                  {n}
                </span>
              ))}
              {more > 0 && <span className="so-chip so-chip-more">{t('subnetOverlaps.sitesMore', { count: more })}</span>}
              {hidden && <span className="so-chip so-chip-more">{hidden}</span>}
            </span>
          );
        },
      },
      {
        key: 'devices',
        header: t('subnetOverlaps.cols.devices'),
        width: '90px',
        align: 'right',
        render: (o) => o.node_count,
      },
      {
        key: 'why',
        header: t(`subnetOverlaps.whyHeader.${tab}`),
        width: '2fr',
        render: whyCell,
      },
    ];
    const filters = overlapFilters(t);
    for (const c of cols) c.filter = filters[c.key];
    return cols;
    // eslint-disable-next-line react-hooks/exhaustive-deps -- whyCell reads `view` through ruleName
  }, [t, openKey, tab, view]);
  const filtering = useClientFilters(columns, rows);
  const { filters, setFilters, shown, counts, anyFiltered } = filtering;

  const detail = (o: SubnetOverlap) => {
    const bars = rangeBars(o);
    const suggestion = suggestedRule(o);
    return (
      <div className={`so-detail so-detail-${o.kind}`}>
        <p className="so-why">
          {hintText(o) ?? t(`subnetOverlaps.kindHelp.${o.kind}`)}
        </p>
        {o.outer_withheld && <p className="muted">{t('subnetOverlaps.detail.outerWithheld')}</p>}
        {o.shared_addresses.length > 0 && (
          <p className="so-line">
            {t('subnetOverlaps.detail.shared')}{' '}
            <span className="mono so-hit">{o.shared_addresses.join(', ')}</span>
          </p>
        )}
        {bars.length > 0 && (
          <div className="so-bars" aria-label={t('subnetOverlaps.detail.inner')}>
            {bars.map((b) => (
              <div className="so-bar" key={b.subnet}>
                <span className="mono so-bar-label">{b.subnet}</span>
                <span className="so-lane">
                  <span
                    className={b.outer ? 'so-seg so-seg-outer' : 'so-seg'}
                    style={{ left: `${b.leftPct}%`, width: `${b.widthPct}%` }}
                  />
                </span>
              </div>
            ))}
            {o.inner_count > o.inner.length && (
              <p className="muted">
                {t('subnetOverlaps.detail.innerMore', { shown: o.inner.length, total: o.inner_count })}
              </p>
            )}
          </div>
        )}
        <div className="so-places-wrap">
          <table className="so-places">
            <thead>
              <tr>
                <th>{t('subnetOverlaps.detail.site')}</th>
                <th>{t('subnetOverlaps.detail.node')}</th>
                <th>{t('subnetOverlaps.detail.port')}</th>
                <th>{t('subnetOverlaps.detail.alias')}</th>
                <th>{t('subnetOverlaps.detail.address')}</th>
              </tr>
            </thead>
            <tbody>
              {o.places.map((p) => {
                const ip = p.address.split('/')[0];
                return (
                  <tr key={`${p.node_id}-${p.ifindex}-${p.address}`}>
                    <td>{p.site_name ?? t('subnetOverlaps.root')}</td>
                    <td>
                      <Link to={nodeHref(p.node_id)}>{p.node_name ?? p.node_id}</Link>
                    </td>
                    <td className="mono">{p.if_name ?? `#${p.ifindex}`}</td>
                    <td>{p.if_alias || '—'}</td>
                    <td className={o.shared_addresses.includes(ip) ? 'mono so-hit' : 'mono'}>
                      {p.address}
                    </td>
                  </tr>
                );
              })}
            </tbody>
          </table>
        </div>
        {o.place_count > o.places.length && (
          <p className="muted">
            {t('subnetOverlaps.detail.placesMore', { shown: o.places.length, total: o.place_count })}
          </p>
        )}
        {canConfig && o.status === 'open' && (
          <div className="so-actions">
            <Button variant="outline" onClick={() => setAcking(o)}>
              {t('subnetOverlaps.actions.intentional')}
            </Button>
            {suggestion && (
              <Button variant="outline" onClick={() => setNewRule(suggestion)}>
                {t('subnetOverlaps.actions.suggest', { word: suggestion.port_text })}
              </Button>
            )}
          </div>
        )}
        {canConfig && o.status === 'intentional' && (
          <div className="so-actions">
            <Button variant="outline" onClick={() => reopen(o)}>
              {t('subnetOverlaps.actions.reopen')}
            </Button>
          </div>
        )}
      </div>
    );
  };

  return (
    <div>
      <PageHeader
        title={t('nav:nodes.subnetOverlaps')}
        trail={[{ label: t('nav:sections.nodes') }, { label: t('nav:nodes.subnetOverlaps') }]}
      />

      <LoadGate load={load} permission="view">
        {view && (
          <p className="so-coverage">
            {t('subnetOverlaps.coverage', {
              withAddresses: view.nodes_with_addresses,
              total: view.nodes_total,
              subnets: view.subnets_checked,
            })}
            {view.nodes_truncated > 0 &&
              ` ${t('subnetOverlaps.coverageTruncated', { count: view.nodes_truncated })}`}
          </p>
        )}
        {scopedOut && <p className="muted">{t('subnetOverlaps.scopedReadOnly')}</p>}

        <div className="so-tiles" role="group" aria-label={t('subnetOverlaps.tilesLabel')}>
          {OVERLAP_KINDS.map((k) => (
            <button
              key={k}
              type="button"
              className={`so-tile so-tile-${k}`}
              aria-pressed={kind === k}
              onClick={() => {
                setKind(kind === k ? null : k);
                setTab('open');
                setOpenKey(null);
              }}
            >
              <span className="so-tile-n">{kindCounts[k]}</span>
              <span className="so-tile-t">{t(`subnetOverlaps.kind.${k}`)}</span>
            </button>
          ))}
        </div>

        <ListToolbar
          list={filtering}
          labels={columnLabels(columns)}
          leading={
            <Tabs
              tabs={OVERLAP_STATUSES.map((s) => ({
                key: s,
                label: t(`subnetOverlaps.tabs.${s}`),
                count: view ? view.counts[s] : undefined,
              }))}
              active={tab}
              onChange={(s) => {
                setTab(s);
                setOpenKey(null);
              }}
            />
          }
        >
          <Button variant="outline" onClick={() => setRulesOpen(true)}>
            {t('subnetOverlaps.rulesButton', { count: view?.rules.filter((r) => r.enabled).length ?? 0 })}
          </Button>
        </ListToolbar>

        {actionError && (
          <p className="form-error" role="alert">
            {actionError}
          </p>
        )}
        {view && view.overlaps.length < view.counts.open + view.counts.intentional + view.counts.excluded && (
          <p className="muted">
            {t('subnetOverlaps.truncated', {
              shown: view.overlaps.length,
              total: view.counts.open + view.counts.intentional + view.counts.excluded,
            })}
          </p>
        )}

        <DataTable
          tableId="nodes.subnetOverlaps"
          rows={shown}
          columns={columns}
          filters={filters}
          onFiltersChange={setFilters}
          filterCounts={counts}
          rowKey={(o) => o.key}
          onRowClick={(o) => setOpenKey(openKey === o.key ? null : o.key)}
          expanded={(o) => (o.key === openKey ? detail(o) : null)}
          expandedKey={openKey}
          loading={loading}
          empty={
            anyFiltered
              ? t('common:filter.noMatch')
              : t(`subnetOverlaps.${empty.key}`, { missing: empty.missing })
          }
        />
      </LoadGate>

      {rulesOpen && view && (
        <OverlapRulesModal
          rules={view.rules}
          canConfig={canConfig}
          describe={(r) => ruleText(r.range ?? null, r.port_text ?? null)}
          onAdd={() => setNewRule(null)}
          onChanged={reload}
          onClose={() => setRulesOpen(false)}
        />
      )}
      {newRule !== undefined && (
        <OverlapRuleModal
          initial={newRule}
          places={view?.overlaps.flatMap((o) => o.places) ?? []}
          onClose={() => setNewRule(undefined)}
          onDone={() => {
            setNewRule(undefined);
            setOpenKey(null);
            reload();
          }}
        />
      )}
      {acking && (
        <AckOverlapModal
          overlap={acking}
          onClose={() => setAcking(null)}
          onDone={() => {
            setAcking(null);
            setOpenKey(null);
            reload();
          }}
        />
      )}
    </div>
  );
}
