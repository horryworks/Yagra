// SPDX-License-Identifier: AGPL-3.0-only
// Mutes (Alerts ▸ Mutes). A mute silences notifications for one node — optionally one
// check (metric name) — until a given time. The alert still fires and shows in the UI and
// history; only the page is suppressed. Expired mutes drop off automatically.
//
// Data-table standard v2: an action row (count + "+ Add mute") over the shared `DataTable`, with
// the narrowing controls in the filter row under the header (ADR-053 Inc.5). Adding a mute and
// lifting one both go through modals — the add form picks a node + optional check and an expiry;
// lifting is a destructive-consent confirm.
//
// The migration off hand-rolled `ytable-head` markup also buys virtualization, which this screen
// did not have and the stated tens-of-thousands-of-rows requirement asks for. A mute list is not
// fleet-scaled, so that is insurance rather than a fix — the reason to do it here is that the
// filter row and the hand-rolled grid could not coexist (three grids share one template).

import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { Trans, useTranslation } from 'react-i18next';
import { Link } from 'react-router-dom';
import { api, ApiError } from '../services/api';
import { useAuthStore } from '../store';
import type { Mute, NodeGroup } from '../types/api';
import { PageHeader } from '../components/ui/PageHeader';
import { Card } from '../components/ui/Card';
import { Button } from '../components/ui/Button';
import { ConfirmDeleteModal } from '../components/ui/ConfirmDeleteModal';
import { Badge } from '../components/ui/Badge';
import { IconButton } from '../components/ui/IconButton';
import { TableToolbar, TableSpacer, ResultCount } from '../components/ui/TableToolbar';
import { DataTable, type Column } from '../components/ui/DataTable';
import { ClearFilters } from '../components/ui/ClearFilters';
import { MobileFilterButton, MobileFilterSheet } from '../components/ui/MobileFilterSheet';
import { useClientFilters } from '../lib/useClientFilters';
import { TrashIcon } from '../components/ui/icons';
import { AddMuteModal } from '../components/suppression/AddMuteModal';
import { EntityName, useEntityNames } from '../components/ui/EntityName';
import { formatScheduleTime } from '../lib/format';
import { muteFilters } from './suppressionFilters';
import './MutesPage.css';

/** Confirm + lift a mute. Destructive-consent chrome comes from the shared modal; only the
 *  sentence and the confirm label are this dialog's own (the action is "lift", not "delete"). */
function LiftMuteModal({
  mute,
  targetName,
  onClose,
  onDone,
}: {
  mute: Mute;
  targetName: string;
  onClose: () => void;
  onDone: () => void;
}) {
  const { t } = useTranslation('suppression');
  return (
    <ConfirmDeleteModal
      title={t('mutes.lift.title')}
      confirmLabel={t('mutes.lift.title')}
      onConfirm={() => api.deleteMute(mute.id)}
      errorFallback={t('mutes.err.lift')}
      onClose={onClose}
      onDone={onDone}
    >
      {mute.metric_name ? (
        <Trans
          t={t}
          i18nKey="mutes.lift.confirmMetric"
          values={{ target: targetName, metric: mute.metric_name }}
          components={{ b: <strong />, m: <span className="mono" /> }}
        />
      ) : (
        <Trans
          t={t}
          i18nKey="mutes.lift.confirmAll"
          values={{ target: targetName }}
          components={{ b: <strong /> }}
        />
      )}
    </ConfirmDeleteModal>
  );
}

export function MutesPage() {
  const { t } = useTranslation('suppression');
  const authed = useAuthStore((s) => s.authed);
  const [rows, setRows] = useState<Mute[]>([]);
  const [groups, setGroups] = useState<NodeGroup[]>([]);
  const [unavailable, setUnavailable] = useState(false);
  const [loading, setLoading] = useState(true);
  const [adding, setAdding] = useState(false);
  const [lifting, setLifting] = useState<Mute | null>(null);
  const [sheet, setSheet] = useState(false);

  const load = useCallback(() => {
    api
      .listMutes()
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
    load();
    api.listNodeGroups().then(setGroups).catch(() => undefined);
  }, [load]);

  // Resolve a mute's node target by name across the whole fleet (not just the first list page —
  // the old nodes.find() capped at 100 and showed a raw UUID for the 101st+ node, S12).
  const { nodeName } = useEntityNames();

  // The mute's target: a node name, or a folder-group name (recursive). Falls back to the raw id.
  const targetName = (m: Mute): string =>
    m.scope_kind === 'group'
      ? groups.find((g) => g.id === m.group_id)?.name ?? m.group_id ?? '—'
      : m.node_id
        ? nodeName(m.node_id)
        : '—';

  // Client-side because the list is bounded by what an operator typed in, not by fleet size
  // (ui-conventions). The judgement is in `suppressionFilters.ts`; this is only the wiring.
  //
  // One clock reading, pinned to the row list rather than re-read each render, so a mute cannot
  // expire *while the operator reads the screen* and move between the two facet counts. A ref
  // rather than a memo: a memo is a cache the runtime may drop, not a guarantee.
  const clock = useRef<{ rows: unknown; at: number } | null>(null);
  if (!clock.current || clock.current.rows !== rows) clock.current = { rows, at: Date.now() };
  const now = clock.current.at;

  const columns = useMemo<Column<Mute>[]>(() => {
    const specs = muteFilters(t, targetName, now);
    const cols: Column<Mute>[] = [
      {
        key: 'target',
        header: t('mutes.cols.target'),
        width: '1.4fr',
        render: (m) => (
          <span className="mute-target">
            {m.scope_kind === 'group' && <Badge>{t('mutes.badge.group')}</Badge>}
            <EntityName
              name={targetName(m)}
              id={(m.scope_kind === 'group' ? m.group_id : m.node_id) ?? undefined}
            />
          </span>
        ),
      },
      {
        key: 'metric',
        header: t('mutes.cols.metric'),
        width: '170px',
        render: (m) =>
          m.scope_kind === 'group' ? (
            <Badge tone="info">{t('mutes.badge.subgroups')}</Badge>
          ) : m.metric_name ? (
            <Badge tone="neutral">
              <span className="mono">{m.metric_name}</span>
            </Badge>
          ) : (
            <Badge tone="info">{t('mutes.badge.allMetrics')}</Badge>
          ),
      },
      {
        key: 'until',
        header: t('mutes.cols.until'),
        width: '180px',
        render: (m) => <span className="mono">{formatScheduleTime(m.until_at)}</span>,
      },
      {
        key: 'reason',
        header: t('mutes.cols.reason'),
        width: '1fr',
        render: (m) => <span className="ellipsis muted">{m.reason}</span>,
      },
      {
        key: 'actions',
        header: t('mutes.cols.actions'),
        width: '92px',
        align: 'right',
        render: (m) =>
          authed ? (
            <span className="ytable-actions">
              <IconButton title={t('mutes.lift.title')} danger onClick={() => setLifting(m)}>
                <TrashIcon />
              </IconButton>
            </span>
          ) : null,
      },
    ];
    for (const c of cols) c.filter = specs[c.key];
    return cols;
    // `targetName` closes over `groups` and the name resolver, so it is not stable across renders;
    // depending on it would rebuild the columns every render and re-run the predicate. The two
    // things it actually reads are in the list instead.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [t, now, authed, groups, nodeName]);

  // URL-backed: one table on this route, so the column keys are free and a narrowed view can be
  // sent to someone.
  const { filterCols, filters, setFilters, clear, shown, counts, anyFiltered } = useClientFilters(
    columns,
    rows,
    { url: true },
  );

  return (
    <div>
      <PageHeader
        title={t('nav:alerts.mutes')}
        trail={[{ label: t('nav:sections.alerts') }, { label: t('nav:alerts.mutes') }]}
        note={
          <Trans
            t={t}
            i18nKey="mutes.note"
            components={{ maintenanceLink: <Link to="/alerts/maintenance" /> }}
          />
        }
      />

      {unavailable ? (
        <Card>
          <p className="muted">{t('mutes.unavailable')}</p>
        </Card>
      ) : (
        <>
          <TableToolbar>
            <MobileFilterButton
              columns={filterCols}
              filters={filters}
              onOpen={() => setSheet(true)}
            />
            <ClearFilters columns={filterCols} filters={filters} onClear={clear} />
            <TableSpacer />
            <ResultCount
              shown={shown.length}
              total={anyFiltered ? rows.length : undefined}
              noun={t('mutes.resultNoun')}
            />
            {authed && (
              <Button variant="primary" onClick={() => setAdding(true)}>
                {t('mutes.add')}
              </Button>
            )}
          </TableToolbar>

          <DataTable
            rows={shown}
            columns={columns}
            rowKey={(m) => m.id}
            filters={filters}
            onFiltersChange={setFilters}
            filterCounts={counts}
            loading={loading}
            empty={anyFiltered ? t('mutes.empty.filtered') : t('mutes.empty.title')}
          />
          {sheet && (
            <MobileFilterSheet
              columns={filterCols}
              filters={filters}
              onChange={setFilters}
              counts={counts}
              labels={Object.fromEntries(columns.map((c) => [c.key, t(`mutes.cols.${c.key}`)]))}
              onClose={() => setSheet(false)}
            />
          )}
        </>
      )}

      {adding && (
        <AddMuteModal
          groups={groups}
          onClose={() => setAdding(false)}
          onSaved={load}
        />
      )}
      {lifting && (
        <LiftMuteModal
          mute={lifting}
          targetName={targetName(lifting)}
          onClose={() => setLifting(null)}
          onDone={() => {
            setLifting(null);
            load();
          }}
        />
      )}
    </div>
  );
}
