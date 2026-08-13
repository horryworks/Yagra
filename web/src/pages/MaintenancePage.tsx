// SPDX-License-Identifier: AGPL-3.0-only
// Maintenance windows (Alerts ▸ Maintenance windows). A window covers nodes by scope
// (node / profile / group, like thresholds) for a time range: covered nodes observe
// `maintenance` — no alerts fire and existing ones resolve — until the window ends.
// The engine refreshes its snapshot every ~30s, so boundaries take effect within that.
//
// Data-table standard v2: an action row (count + "+ Add window") over the shared `DataTable`, with
// the narrowing controls in the filter row under the header (ADR-053 Inc.5).
// Add and delete both go through modals; enable/disable is an immediate row action.

import { useCallback, useEffect, useMemo, useState } from 'react';
import { Trans, useTranslation } from 'react-i18next';
import { Link } from 'react-router-dom';
import { api, errMsg, ApiError } from '../services/api';
import { useAuthStore } from '../store';
import type { MaintenanceWindow, NodeGroup, ProfileSummary } from '../types/api';
import { PageHeader } from '../components/ui/PageHeader';
import { Card } from '../components/ui/Card';
import { Button } from '../components/ui/Button';
import { ConfirmDeleteModal } from '../components/ui/ConfirmDeleteModal';
import { Badge } from '../components/ui/Badge';
import { OverflowMenu } from '../components/ui/OverflowMenu';
import { TableToolbar, TableSpacer, ResultCount } from '../components/ui/TableToolbar';
import { DataTable, type Column } from '../components/ui/DataTable';
import { ClearFilters } from '../components/ui/ClearFilters';
import { MobileFilterButton, MobileFilterSheet } from '../components/ui/MobileFilterSheet';
import { useClientFilters } from '../lib/useClientFilters';
import { PowerIcon, TrashIcon } from '../components/ui/icons';
import { AddMaintenanceWindowModal } from '../components/suppression/AddMaintenanceWindowModal';
import { EntityName, useEntityNames } from '../components/ui/EntityName';
import { formatScheduleTime } from '../lib/format';
import { isEnded, windowStatus } from './maintenanceStatus';
import { windowFilters } from './suppressionFilters';
import './MaintenancePage.css';

/** Confirm + delete a maintenance window (destructive-consent modal). */
function DeleteWindowModal({
  win,
  onClose,
  onDone,
}: {
  win: MaintenanceWindow;
  onClose: () => void;
  onDone: () => void;
}) {
  const { t } = useTranslation('suppression');
  return (
    <ConfirmDeleteModal
      title={t('maintenance.delete.title')}
      onConfirm={() => api.deleteMaintenanceWindow(win.id)}
      errorFallback={t('maintenance.err.delete')}
      onClose={onClose}
      onDone={onDone}
    >
      <Trans
        t={t}
        i18nKey="maintenance.delete.confirm"
        values={{ name: win.name }}
        components={{ b: <strong /> }}
      />
    </ConfirmDeleteModal>
  );
}

/** Confirm + delete every ended window the account can see (destructive-consent modal).
 *
 *  One request, so there is no partial failure to represent: it commits or it rejects and the
 *  dialog stays open with the message. The server's count is not shown — the reloaded list is the
 *  answer, and it also reconciles a stale page or another operator having got there first. */
function ClearEndedModal({
  count,
  onClose,
  onDone,
}: {
  count: number;
  onClose: () => void;
  onDone: () => void;
}) {
  const { t } = useTranslation('suppression');
  return (
    <ConfirmDeleteModal
      title={t('maintenance.clearEnded.title')}
      confirmLabel={t('maintenance.clearEnded.confirmLabel')}
      onConfirm={() => api.clearEndedMaintenanceWindows()}
      errorFallback={t('maintenance.err.clearEnded')}
      onClose={onClose}
      onDone={onDone}
    >
      <Trans
        t={t}
        i18nKey="maintenance.clearEnded.confirm"
        count={count}
        values={{ count }}
        components={{ b: <strong /> }}
      />
    </ConfirmDeleteModal>
  );
}

export function MaintenancePage() {
  const { t } = useTranslation('suppression');
  const authed = useAuthStore((s) => s.authed);
  const [rows, setRows] = useState<MaintenanceWindow[]>([]);
  const [groups, setGroups] = useState<NodeGroup[]>([]);
  const [profiles, setProfiles] = useState<ProfileSummary[]>([]);
  const [unavailable, setUnavailable] = useState(false);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [adding, setAdding] = useState(false);
  const [deleting, setDeleting] = useState<MaintenanceWindow | null>(null);
  const [clearing, setClearing] = useState(false);
  const [sheet, setSheet] = useState(false);

  const load = useCallback(() => {
    api
      .listMaintenanceWindows()
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
    api.listProfiles().then(setProfiles).catch(() => undefined);
  }, [load]);

  const setEnabled = (id: string, enabled: boolean) =>
    api
      .setMaintenanceWindowEnabled(id, enabled)
      .then(load)
      .catch((e: unknown) => setError(errMsg(e, t('maintenance.err.update'))));

  // Short badge for the scope level: `group_id` is a folder group, plain `group` the legacy tag.
  const scopeBadge = (w: MaintenanceWindow): string => {
    if (w.scope_level === 'group_id') return t('maintenance.scope.group');
    if (w.scope_level === 'group') return t('maintenance.scope.groupTag');
    if (w.scope_level === 'node') return t('maintenance.scope.node');
    if (w.scope_level === 'profile') return t('maintenance.scope.profile');
    // Opened by the upgrade path, never by hand — it is not offered in the add dialog (ADR-050).
    if (w.scope_level === 'system') return t('maintenance.scope.system');
    return w.scope_level;
  };

  // One clock reading for the whole render, so the badges and the clear button's count cannot be
  // computed a millisecond apart and disagree about a window that ends mid-render.
  const now = Date.now();
  const endedCount = rows.filter((w) => isEnded(w, now)).length;

  // Resolve a node scope by name across the whole fleet (not just the first list page — the old
  // nodes.find() capped at 100 and showed a raw UUID for the 101st+ node, S12).
  const { nodeName } = useEntityNames();

  // Human label for a scope id (node/profile/folder-group names resolved when known).
  const scopeLabel = (w: MaintenanceWindow): string => {
    if (w.scope_level === 'node') return nodeName(w.scope_id);
    if (w.scope_level === 'profile')
      return profiles.find((p) => p.id === w.scope_id)?.name ?? w.scope_id;
    if (w.scope_level === 'group_id')
      return groups.find((g) => g.id === w.scope_id)?.name ?? w.scope_id;
    return w.scope_id;
  };

  // Client-side because the list is bounded by what an operator typed in, not by fleet size
  // (ui-conventions). The judgement is in `suppressionFilters.ts`; this is only the wiring.
  const columns = useMemo<Column<MaintenanceWindow>[]>(() => {
    const specs = windowFilters(t, scopeLabel, now);
    const cols: Column<MaintenanceWindow>[] = [
      {
        key: 'status',
        header: t('maintenance.cols.status'),
        width: '120px',
        render: (w) => {
          const status = windowStatus(w, now);
          return <Badge tone={status.tone}>{t(`maintenance.status.${status.labelKey}`)}</Badge>;
        },
      },
      {
        key: 'name',
        header: t('maintenance.cols.name'),
        width: '1.4fr',
        render: (w) => <span className="yt-name-txt">{w.name}</span>,
      },
      {
        key: 'scope',
        header: t('maintenance.cols.scope'),
        width: '1fr',
        render: (w) => (
          <span className="maint-scope">
            <Badge>{scopeBadge(w)}</Badge>
            <EntityName name={scopeLabel(w)} id={w.scope_id} />
          </span>
        ),
      },
      {
        key: 'range',
        header: t('maintenance.cols.range'),
        width: '230px',
        render: (w) => (
          <span className="mono">
            {formatScheduleTime(w.starts_at)} → {formatScheduleTime(w.ends_at)}
          </span>
        ),
      },
      {
        key: 'actions',
        header: t('maintenance.cols.actions'),
        width: '120px',
        align: 'right',
        render: (w) =>
          authed ? (
            <span className="ytable-actions">
              <OverflowMenu
                actions={[
                  {
                    label: w.enabled
                      ? t('maintenance.actions.disable')
                      : t('maintenance.actions.enable'),
                    icon: <PowerIcon />,
                    onClick: () => setEnabled(w.id, !w.enabled),
                  },
                  {
                    label: t('common:actions.delete'),
                    icon: <TrashIcon />,
                    danger: true,
                    onClick: () => setDeleting(w),
                  },
                ]}
              />
            </span>
          ) : null,
      },
    ];
    for (const c of cols) c.filter = specs[c.key];
    return cols;
    // `scopeLabel`/`scopeBadge`/`setEnabled` close over state and are rebuilt every render; the
    // values they actually read are listed instead, so the columns (and therefore the predicate)
    // are not rebuilt on every keystroke elsewhere on the page.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [t, now, authed, groups, profiles, nodeName]);

  // URL-backed: one table on this route, so a narrowed view is linkable.
  const { filterCols, filters, setFilters, clear, shown, counts, anyFiltered } = useClientFilters(
    columns,
    rows,
    { url: true },
  );

  return (
    <div>
      <PageHeader
        title={t('nav:alerts.maintenance')}
        trail={[{ label: t('nav:sections.alerts') }, { label: t('nav:alerts.maintenance') }]}
        note={
          <Trans
            t={t}
            i18nKey="maintenance.note"
            components={{ mutesLink: <Link to="/alerts/mutes" /> }}
          />
        }
      />

      {unavailable ? (
        <Card>
          <p className="muted">{t('maintenance.unavailable')}</p>
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
              noun={t('common:noun.window', { count: shown.length })}
            />
            {authed && (
              <>
                {/* Kept mounted and disabled at zero rather than appearing and disappearing: at
                    zero it still tells the operator the capability exists. */}
                <Button
                  variant="danger"
                  onClick={() => setClearing(true)}
                  disabled={endedCount === 0}
                >
                  {t('maintenance.clearEnded.action', { count: endedCount })}
                </Button>
                <Button variant="primary" onClick={() => setAdding(true)}>
                  {t('maintenance.add')}
                </Button>
              </>
            )}
          </TableToolbar>

          {error && <p className="form-error">{error}</p>}

          <DataTable
            rows={shown}
            columns={columns}
            rowKey={(w) => w.id}
            filters={filters}
            onFiltersChange={setFilters}
            filterCounts={counts}
            loading={loading}
            empty={anyFiltered ? t('maintenance.empty.filtered') : t('maintenance.empty.title')}
          />
          {sheet && (
            <MobileFilterSheet
              columns={filterCols}
              filters={filters}
              onChange={setFilters}
              counts={counts}
              labels={Object.fromEntries(columns.map((c) => [c.key, t(`maintenance.cols.${c.key}`)]))}
              onClose={() => setSheet(false)}
            />
          )}
        </>
      )}

      {adding && (
        <AddMaintenanceWindowModal
          groups={groups}
          profiles={profiles}
          onClose={() => setAdding(false)}
          onSaved={load}
        />
      )}
      {deleting && (
        <DeleteWindowModal
          win={deleting}
          onClose={() => setDeleting(null)}
          onDone={() => {
            setDeleting(null);
            load();
          }}
        />
      )}
      {clearing && (
        <ClearEndedModal
          count={endedCount}
          onClose={() => setClearing(false)}
          onDone={() => {
            setClearing(false);
            load();
          }}
        />
      )}
    </div>
  );
}
