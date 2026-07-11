// Maintenance windows (Alerts ▸ Maintenance windows). A window covers nodes by scope
// (node / profile / group, like thresholds) for a time range: covered nodes observe
// `maintenance` — no alerts fire and existing ones resolve — until the window ends.
// The engine refreshes its snapshot every ~30s, so boundaries take effect within that.
//
// Data-table standard v2: a toolbar (count + "+ Add window") over the shared `.ytable`.
// Add and delete both go through modals; enable/disable is an immediate row action.

import { useCallback, useEffect, useState } from 'react';
import { Trans, useTranslation } from 'react-i18next';
import { Link } from 'react-router-dom';
import { api, ApiError } from '../services/api';
import { useAuthStore } from '../store';
import type { MaintenanceWindow, NodeGroup, ProfileSummary } from '../types/api';
import { PageHeader } from '../components/ui/PageHeader';
import { Card } from '../components/ui/Card';
import { Button } from '../components/ui/Button';
import { Modal } from '../components/ui/Modal';
import { Badge } from '../components/ui/Badge';
import { IconButton } from '../components/ui/IconButton';
import { TableToolbar, TableSpacer, ResultCount } from '../components/ui/TableToolbar';
import { PowerIcon, TrashIcon } from '../components/ui/icons';
import { AddMaintenanceWindowModal } from '../components/suppression/AddMaintenanceWindowModal';
import { useEntityNames } from '../components/ui/EntityName';
import './MaintenancePage.css';

const COLS = '120px 1.4fr 1fr 230px 120px';

const errMsg = (e: unknown, fallback: string) =>
  e instanceof ApiError ? e.message : fallback;

/** RFC 3339 → compact local display. */
const fmtTime = (iso: string) =>
  new Date(iso).toLocaleString(undefined, {
    year: 'numeric',
    month: '2-digit',
    day: '2-digit',
    hour: '2-digit',
    minute: '2-digit',
  });

/** Language-agnostic status key + badge tone; the label is resolved at the call site. */
function windowStatus(w: MaintenanceWindow): { labelKey: string; tone: 'info' | 'neutral' } {
  if (!w.enabled) return { labelKey: 'disabled', tone: 'neutral' };
  if (w.active) return { labelKey: 'active', tone: 'info' };
  if (new Date(w.ends_at).getTime() < Date.now()) return { labelKey: 'ended', tone: 'neutral' };
  return { labelKey: 'scheduled', tone: 'neutral' };
}

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
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const submit = () => {
    setBusy(true);
    setError(null);
    api
      .deleteMaintenanceWindow(win.id)
      .then(onDone)
      .catch((e: unknown) => {
        setError(errMsg(e, t('maintenance.err.delete')));
        setBusy(false);
      });
  };

  return (
    <Modal
      title={t('maintenance.delete.title')}
      onClose={onClose}
      footer={
        <>
          <Button variant="outline" onClick={onClose} disabled={busy}>
            {t('common:actions.cancel')}
          </Button>
          <Button variant="danger" onClick={submit} disabled={busy}>
            {t('common:actions.delete')}
          </Button>
        </>
      }
    >
      <p className="modal-confirm-text">
        <Trans
          t={t}
          i18nKey="maintenance.delete.confirm"
          values={{ name: win.name }}
          components={{ b: <strong /> }}
        />
      </p>
      {error && <p className="form-error">{error}</p>}
    </Modal>
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
    return w.scope_level;
  };

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
            <TableSpacer />
            <ResultCount shown={rows.length} noun={t('common:noun.window', { count: rows.length })} />
            {authed && (
              <Button variant="primary" onClick={() => setAdding(true)}>
                {t('maintenance.add')}
              </Button>
            )}
          </TableToolbar>

          {error && <p className="form-error">{error}</p>}

          <div className="ytable">
            <div className="ytable-head" style={{ gridTemplateColumns: COLS }}>
              <div className="ytable-h">{t('maintenance.cols.status')}</div>
              <div className="ytable-h">{t('maintenance.cols.name')}</div>
              <div className="ytable-h">{t('maintenance.cols.scope')}</div>
              <div className="ytable-h">{t('maintenance.cols.range')}</div>
              <div className="ytable-h right">{t('maintenance.cols.actions')}</div>
            </div>

            {rows.length === 0 ? (
              <div className="yt-empty">
                <p className="yt-empty-title">
                  {loading ? t('common:loading') : t('maintenance.empty.title')}
                </p>
                {!loading && (
                  <p className="yt-empty-sub">{t('maintenance.empty.sub')}</p>
                )}
              </div>
            ) : (
              rows.map((w) => {
                const status = windowStatus(w);
                return (
                  <div className="ytable-row" style={{ gridTemplateColumns: COLS }} key={w.id}>
                    <div className="ytable-cell">
                      <Badge tone={status.tone}>{t(`maintenance.status.${status.labelKey}`)}</Badge>
                    </div>
                    <div className="ytable-cell">
                      <span className="yt-name-txt">{w.name}</span>
                    </div>
                    <div className="ytable-cell">
                      <span className="maint-scope">
                        <Badge>{scopeBadge(w)}</Badge>
                        <span>{scopeLabel(w)}</span>
                      </span>
                    </div>
                    <div className="ytable-cell mono">
                      {fmtTime(w.starts_at)} → {fmtTime(w.ends_at)}
                    </div>
                    <div className="ytable-cell right">
                      {authed && (
                        <span className="ytable-actions">
                          <IconButton
                            title={w.enabled ? t('maintenance.actions.disable') : t('maintenance.actions.enable')}
                            onClick={() => setEnabled(w.id, !w.enabled)}
                          >
                            <PowerIcon />
                          </IconButton>
                          <IconButton title={t('common:actions.delete')} danger onClick={() => setDeleting(w)}>
                            <TrashIcon />
                          </IconButton>
                        </span>
                      )}
                    </div>
                  </div>
                );
              })
            )}
          </div>
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
    </div>
  );
}
