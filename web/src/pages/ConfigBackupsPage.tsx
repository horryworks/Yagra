// SPDX-License-Identifier: AGPL-3.0-only
// Nodes ▸ Config backups (ADR-210 stage 3): every node whose configuration the bundled oxidized
// backs up, and how that is going — the login that works or is being tried, the last attempt, the
// last success, and when the lockout limits allow the next try. A node's own settings are its
// Config tab; this list is where to see at a glance which nodes are backed up and which need
// attention. The state word is `lib/configBackupMarks.ts`, the same one the tree's mark draws.
// Since ADR-224 it also lists the nodes switched off and those that could be, and switches them on
// or off one row at a time or many at once; the rules are `configBackupList.ts`.

import { useCallback, useMemo, useState } from 'react';
import { Link } from 'react-router-dom';
import { useTranslation } from 'react-i18next';
import { api, errMsg } from '../services/api';
import { useCan } from '../store';
import type { ConfigBackupRow } from '../types/api';
import { PageHeader } from '../components/ui/PageHeader';
import { DataTable, type Column } from '../components/ui/DataTable';
import { EmptyState } from '../components/ui/EmptyState';
import { EntityName } from '../components/ui/EntityName';
import { useEntityNames } from '../components/ui/entityNames';
import { TimeCell } from '../components/ui/tableCells';
import { useLoad } from '../lib/useLoad';
import { LoadGate } from '../components/ui/LoadGate';
import { nodeHref } from '../lib/entityHref';
import { isCliCredentialKind } from '../lib/credentialKinds';
import { markOf, stateKeyOf } from '../lib/configBackupMarks';
import { attemptLine, loginLabel } from '../components/NodeDetail/configBackupForm';
import { GIT_REMOTE_PATH, pushLine } from './integrations/gitRemote';
import { formatTimestamp } from '../lib/format';
import { Button } from '../components/ui/Button';
import { Segmented } from '../components/ui/Segmented';
import {
  LIST_FILTERS,
  canSwitchOn,
  countsOf,
  planSwitch,
  rowsIn,
  stateOf,
  type ListFilter,
} from './configBackupList';
import { ConfigBackupSwitchOnModal } from './ConfigBackupSwitchOnModal';
import { scheduleLine } from './configBackupSchedule';
import { ConfigBackupScheduleModal } from './ConfigBackupScheduleModal';
import { ConfigAutoSaveModal } from './ConfigAutoSaveModal';
import './ConfigBackupsPage.css';

export function ConfigBackupsPage() {
  const { t } = useTranslation('nodes');
  const canCreds = useCan('manage_credentials');
  const { nodeName } = useEntityNames();
  const list = useLoad(() => api.listConfigBackups('all'), [], {
    initial: [] as ConfigBackupRow[],
    intervalMs: 30_000,
    errorFallback: t('config.err.list'),
  });
  const creds = useLoad(() => api.listCredentials(), [], { initial: [], enabled: canCreds });
  // Where the backups go (ADR-210 stage 4). Said only once a destination is set: with none, the
  // backups simply stay on the box, which is not a fault.
  const push = useLoad(() => api.getConfigBackupRemoteStatus(), [], {
    initial: null,
    intervalMs: 60_000,
  });
  const line = push.data?.configured ? pushLine(push.data) : null;
  // When the backups are fetched (ADR-210 stage 6): the daily start and how far a run has got.
  const canConfig = useCan('manage_config');
  const canSave = useCan('save_device_config');
  const [autoSaveOpen, setAutoSaveOpen] = useState(false);
  const [editing, setEditing] = useState(false);
  const schedule = useLoad(() => api.getConfigBackupSchedule(), [], {
    initial: null,
    intervalMs: 30_000,
  });
  const sched = schedule.data ? scheduleLine(schedule.data) : null;
  const names = useMemo(
    () =>
      new Map(creds.data.filter((c) => isCliCredentialKind(c.kind)).map((c) => [c.id, c.name])),
    [creds.data],
  );

  // Switching (ADR-224): off takes ManageConfig, on also RevealCredentials, as the Config tab.
  const canReveal = useCan('reveal_credentials');
  const canOff = canConfig;
  const canOn = canConfig && canReveal;
  // `null` until a person picks one: then the list opens on what is backed up, or on everything
  // while nothing is yet — an empty first view would hide the nodes there are to switch on.
  const [picking, setFilter] = useState<ListFilter | null>(null);
  const [selected, setSelected] = useState<ReadonlySet<string>>(new Set());
  const [switching, setSwitching] = useState(false);
  const [switchError, setSwitchError] = useState<string | null>(null);
  const [asking, setAsking] = useState<{ ids: string[]; skipped: string[] } | null>(null);
  const counts = useMemo(() => countsOf(list.data), [list.data]);
  const filter: ListFilter = picking ?? (counts.on > 0 || counts.all === 0 ? 'on' : 'all');
  const shown = useMemo(() => rowsIn(list.data, filter), [list.data, filter]);
  // Only rows still on screen count as picked: a filter change or a reload drops the rest.
  const picked = useMemo(() => shown.filter((r) => selected.has(r.node_id)), [shown, selected]);

  const pickFilter = (next: ListFilter) => {
    setFilter(next);
    setSelected(new Set());
  };
  const reload = list.reload;
  /** Switch `rows`: off at once; on at once when every one has settings, else ask first how the
   *  never-configured ones are reached. */
  const onSwitch = useCallback(
    (rows: readonly ConfigBackupRow[], enabled: boolean) => {
      const plan = planSwitch(rows, enabled);
      if (plan.ids.length === 0) return;
      if (enabled && plan.needsInput) {
        setAsking({ ids: plan.ids, skipped: plan.skipped.map((r) => nodeName(r.node_id)) });
        return;
      }
      setSwitching(true);
      setSwitchError(null);
      api
        .switchConfigBackups({ node_ids: plan.ids, enabled })
        .then(() => setSelected(new Set()))
        .catch((e: unknown) => setSwitchError(errMsg(e, t('config.err.save'))))
        .finally(() => {
          setSwitching(false);
          reload();
        });
    },
    [nodeName, reload, t],
  );

  const columns = useMemo<Column<ConfigBackupRow>[]>(() => {
    const login = (id: string | null | undefined) => {
      const l = loginLabel(id, names, []);
      return l.kind === 'name' ? l.name : '—';
    };
    const toggle = (id: string) =>
      setSelected((prev) => {
        const next = new Set(prev);
        if (!next.delete(id)) next.add(id);
        return next;
      });
    const allPicked = shown.length > 0 && shown.every((r) => selected.has(r.node_id));
    return [
      ...(canOff
        ? [
            {
              key: 'pick',
              header: (
                <input
                  type="checkbox"
                  checked={allPicked}
                  aria-label={t('config.list.pickAll')}
                  onChange={() =>
                    setSelected(allPicked ? new Set() : new Set(shown.map((r) => r.node_id)))
                  }
                />
              ),
              width: '36px',
              render: (r: ConfigBackupRow) => (
                <input
                  type="checkbox"
                  checked={selected.has(r.node_id)}
                  aria-label={t('config.list.pick', { name: nodeName(r.node_id) })}
                  onChange={() => toggle(r.node_id)}
                />
              ),
            },
          ]
        : []),
      {
        key: 'node',
        header: t('config.list.cols.node'),
        width: '1fr',
        render: (r) => (
          <EntityName name={nodeName(r.node_id)} id={r.node_id} to={`${nodeHref(r.node_id)}?tab=config`} />
        ),
      },
      {
        key: 'backup',
        header: t('config.switch.label'),
        width: '96px',
        render: (r) => {
          // A switch is drawn only where this caller can move it (ADR-056).
          const movable = r.enabled ? canOff : canOn && canSwitchOn(r);
          if (!movable) return t(r.enabled ? 'config.switch.on' : 'config.switch.off');
          return (
            <label className="cfgbl-sw">
              <input
                type="checkbox"
                role="switch"
                checked={r.enabled}
                disabled={switching}
                aria-label={t('config.list.switchOne', { name: nodeName(r.node_id) })}
                onChange={() => onSwitch([r], !r.enabled)}
              />
              <span className="cfgbl-sw-track" aria-hidden="true" />
            </label>
          );
        },
      },
      {
        key: 'state',
        header: t('config.list.cols.state'),
        width: '150px',
        render: (r) => {
          const state = stateOf(r);
          if (state !== 'on') {
            if (r.model == null) {
              return <Link to={`${nodeHref(r.node_id)}?tab=config`}>{t('config.list.pickModel')}</Link>;
            }
            return <span className="cfgbl-mark cfgbl-mark-busy">{t(`config.list.state.${state}`)}</span>;
          }
          const mark = markOf(r);
          return <span className={`cfgbl-mark cfgbl-mark-${mark}`}>{t(stateKeyOf(r))}</span>;
        },
      },
      {
        key: 'last',
        header: t('config.list.cols.last'),
        width: '1.4fr',
        render: (r) => {
          if (!r.last) return '—';
          const line = attemptLine(r.last);
          return t(line.key, { login: login(line.login) });
        },
      },
      ...(canCreds
        ? [
            {
              key: 'login',
              header: t('config.list.cols.login'),
              width: '160px',
              render: (r: ConfigBackupRow) => login(r.credential_id),
            },
          ]
        : []),
      {
        key: 'success',
        header: t('config.list.cols.success'),
        width: '200px',
        render: (r) => (r.last_success_at ? <TimeCell iso={r.last_success_at} /> : '—'),
      },
      {
        key: 'next',
        header: t('config.list.cols.next'),
        width: '200px',
        render: (r) => (r.try_after ? <TimeCell iso={r.try_after} /> : '—'),
      },
    ];
  }, [t, nodeName, names, canCreds, canOff, canOn, shown, selected, switching, onSwitch]);

  const onPlan = planSwitch(picked, true);
  const offPlan = planSwitch(picked, false);

  return (
    <div>
      <PageHeader
        title={t('nav:nodes.configBackups')}
        trail={[{ label: t('nav:sections.nodes') }, { label: t('nav:nodes.configBackups') }]}
      />
      {sched && schedule.data && (
        <p className="cfgbl-sched">
          <span>{t('config.schedule.rule', { time: sched.time, zone: sched.zone, count: sched.count })}</span>
          <span>
            {sched.state.key === 'config.schedule.next' &&
              t(sched.state.key, { at: formatTimestamp(Date.parse(sched.state.at)) })}
            {sched.state.key === 'config.schedule.running' &&
              t(sched.state.key, { done: sched.state.done, total: sched.state.total })}
            {sched.state.key === 'config.schedule.runningBare' && t(sched.state.key)}
          </span>
          {canConfig && (
            <Button onClick={() => setEditing(true)} aria-label={t('config.schedule.title')}>
              {t('config.schedule.change')}
            </Button>
          )}
        </p>
      )}
      {schedule.data && (
        <p className="cfgbl-sched">
          <span>
            {schedule.data.auto_save.enabled
              ? t('config.autoSave.lineOn')
              : t('config.autoSave.lineOff')}
          </span>
          {canSave && (
            <Button onClick={() => setAutoSaveOpen(true)} aria-label={t('config.autoSave.title')}>
              {t('config.autoSave.change')}
            </Button>
          )}
        </p>
      )}
      {autoSaveOpen && schedule.data && (
        <ConfigAutoSaveModal
          schedule={schedule.data}
          onClose={() => setAutoSaveOpen(false)}
          onSaved={() => {
            setAutoSaveOpen(false);
            schedule.reload();
          }}
        />
      )}
      {editing && schedule.data && (
        <ConfigBackupScheduleModal
          schedule={schedule.data}
          onClose={() => setEditing(false)}
          onSaved={() => {
            setEditing(false);
            schedule.reload();
          }}
        />
      )}
      {line && (
        <p className={`cfgbl-push ${line.key === 'git.push.failed' ? 'bad' : ''}`}>
          <Link to={GIT_REMOTE_PATH}>{t('system:git.name')}</Link>
          {': '}
          {line.key === 'git.push.ok' &&
            t(`system:${line.key}`, { at: formatTimestamp(Date.parse(line.at)), head: line.head })}
          {line.key === 'git.push.failed' &&
            t(`system:${line.key}`, {
              at: formatTimestamp(Date.parse(line.at)),
              why: t(`system:git.failure.${line.failure}`),
            })}
          {(line.key === 'git.push.waiting' || line.key === 'git.push.none') &&
            t(`system:${line.key}`)}
        </p>
      )}
      <div className="cfgbl-tools">
        <Segmented
          ariaLabel={t('config.list.filter')}
          options={LIST_FILTERS.map((f) => ({
            value: f,
            label: f === 'all' ? t('config.list.state.all') : `${t(`config.list.state.${f}`)} ${counts[f]}`,
          }))}
          value={filter}
          onChange={(next) => pickFilter(next as ListFilter)}
        />
        {picked.length > 0 && (
          <div className="cfgbl-bulk">
            <span>{t('config.list.picked', { count: picked.length })}</span>
            {canOn && onPlan.ids.length > 0 && (
              <Button disabled={switching} onClick={() => onSwitch(picked, true)}>
                {t('config.list.switchOnN', { count: onPlan.ids.length })}
              </Button>
            )}
            {offPlan.ids.length > 0 && (
              <Button disabled={switching} onClick={() => onSwitch(picked, false)}>
                {t('config.list.switchOffN', { count: offPlan.ids.length })}
              </Button>
            )}
            <Button onClick={() => setSelected(new Set())}>{t('config.list.unpick')}</Button>
          </div>
        )}
      </div>
      {switchError && <p className="form-error">{switchError}</p>}
      {asking && (
        <ConfigBackupSwitchOnModal
          ids={asking.ids}
          skipped={asking.skipped}
          onClose={() => setAsking(null)}
          onSaved={() => {
            setAsking(null);
            setSelected(new Set());
            list.reload();
          }}
        />
      )}
      <LoadGate load={list}>
        <DataTable
          tableId="nodes.configBackups"
          rows={shown}
          columns={columns}
          rowKey={(r) => r.node_id}
          loading={list.loading}
          empty={list.error ?? <EmptyState text={t(`config.list.emptyOf.${filter}`)} />}
        />
      </LoadGate>
    </div>
  );
}
