// SPDX-License-Identifier: AGPL-3.0-only
// Mutes (Alerts ▸ Mutes). A mute silences notifications for one node — optionally one
// check (metric name) — until a given time. The alert still fires and shows in the UI and
// history; only the page is suppressed. Expired mutes drop off automatically.
//
// Data-table standard v2: a toolbar (count + "+ Add mute") over the shared `.ytable`. Adding a
// mute and lifting one both go through modals — the add form picks a node + optional check and an
// expiry; lifting is a destructive-consent confirm.

import { useCallback, useEffect, useState } from 'react';
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
import {
  TableToolbar,
  TableSpacer,
  ResultCount,
  SearchInput,
  FilterSelect,
} from '../components/ui/TableToolbar';
import { TrashIcon } from '../components/ui/icons';
import { AddMuteModal } from '../components/suppression/AddMuteModal';
import { EntityName, useEntityNames } from '../components/ui/EntityName';
import { formatScheduleTime } from '../lib/format';
import {
  DEFAULT_MUTE_FILTERS,
  isMuteFiltered,
  matchesMute,
  MUTE_STATES,
  type MuteFilters,
} from './suppressionFilters';
import './MutesPage.css';

const COLS = '1.4fr 170px 180px 1fr 92px';

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
  // (ui-conventions). The judgement is in `suppressionFilters.ts`; this is only the wiring. One
  // clock reading per render, so two rows cannot disagree about a mute expiring between them.
  const now = Date.now();
  const [filters, setFilters] = useState<MuteFilters>(DEFAULT_MUTE_FILTERS);
  const set = <K extends keyof MuteFilters>(key: K, value: MuteFilters[K]) =>
    setFilters((f) => ({ ...f, [key]: value }));
  const shown = rows.filter((m) => matchesMute(m, filters, targetName, now));

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
            <SearchInput
              value={filters.q}
              onChange={(v) => set('q', v)}
              placeholder={t('mutes.filter.searchPlaceholder')}
              ariaLabel={t('mutes.filter.searchAria')}
            />
            <FilterSelect
              value={filters.state}
              onChange={(v) => set('state', v)}
              options={MUTE_STATES.map((s) => ({ value: s, label: t(`mutes.state.${s}`) }))}
              allLabel={t('mutes.filter.allStates')}
              ariaLabel={t('mutes.filter.stateAria')}
            />
            <TableSpacer />
            <ResultCount
              shown={shown.length}
              total={isMuteFiltered(filters) ? rows.length : undefined}
              noun={t('mutes.resultNoun')}
            />
            {authed && (
              <Button variant="primary" onClick={() => setAdding(true)}>
                {t('mutes.add')}
              </Button>
            )}
          </TableToolbar>

          <div className="ytable">
            <div className="ytable-scroll">
              <div className="ytable-head" style={{ gridTemplateColumns: COLS }}>
                <div className="ytable-h">{t('mutes.cols.target')}</div>
                <div className="ytable-h">{t('mutes.cols.metric')}</div>
                <div className="ytable-h">{t('mutes.cols.until')}</div>
                <div className="ytable-h">{t('mutes.cols.reason')}</div>
                <div className="ytable-h right">{t('mutes.cols.actions')}</div>
              </div>

              {shown.length === 0 ? (
                <div className="yt-empty">
                  <p className="yt-empty-title">
                    {loading
                      ? t('common:loading')
                      : isMuteFiltered(filters)
                        ? t('mutes.empty.filtered')
                        : t('mutes.empty.title')}
                  </p>
                  {!loading && !isMuteFiltered(filters) && (
                    <p className="yt-empty-sub">{t('mutes.empty.sub')}</p>
                  )}
                </div>
              ) : (
                shown.map((m) => (
                  <div className="ytable-row" style={{ gridTemplateColumns: COLS }} key={m.id}>
                    <div className="ytable-cell">
                      <span className="mute-target">
                        {m.scope_kind === 'group' && <Badge>{t('mutes.badge.group')}</Badge>}
                        <EntityName
                          name={targetName(m)}
                          id={(m.scope_kind === 'group' ? m.group_id : m.node_id) ?? undefined}
                        />
                      </span>
                    </div>
                    <div className="ytable-cell">
                      {m.scope_kind === 'group' ? (
                        <Badge tone="info">{t('mutes.badge.subgroups')}</Badge>
                      ) : m.metric_name ? (
                        <Badge tone="neutral">
                          <span className="mono">{m.metric_name}</span>
                        </Badge>
                      ) : (
                        <Badge tone="info">{t('mutes.badge.allMetrics')}</Badge>
                      )}
                    </div>
                    <div className="ytable-cell mono">{formatScheduleTime(m.until_at)}</div>
                    <div className="ytable-cell ellipsis muted">{m.reason}</div>
                    <div className="ytable-cell right">
                      {authed && (
                        <span className="ytable-actions">
                          <IconButton title={t('mutes.lift.title')} danger onClick={() => setLifting(m)}>
                            <TrashIcon />
                          </IconButton>
                        </span>
                      )}
                    </div>
                  </div>
                ))
              )}
            </div>
          </div>
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
