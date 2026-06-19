// Mutes (Alerts ▸ Mutes). A mute silences notifications for one node — optionally one
// check (metric name) — until a given time. The alert still fires and shows in the UI and
// history; only the page is suppressed. Expired mutes drop off automatically.
//
// Data-table standard v2: a toolbar (count + "+ Add mute") over the shared `.ytable`. Adding a
// mute and lifting one both go through modals — the add form picks a node + optional check and an
// expiry; lifting is a destructive-consent confirm.

import { useCallback, useEffect, useState } from 'react';
import { api, ApiError } from '../services/api';
import { useAuthStore } from '../store';
import type { Mute, NodeGroup, NodeSummary } from '../types/api';
import { PageHeader } from '../components/ui/PageHeader';
import { Card } from '../components/ui/Card';
import { Button } from '../components/ui/Button';
import { Modal } from '../components/ui/Modal';
import { Badge } from '../components/ui/Badge';
import { IconButton } from '../components/ui/IconButton';
import { TableToolbar, TableSpacer, ResultCount } from '../components/ui/TableToolbar';
import { TrashIcon } from '../components/ui/icons';
import { AddMuteModal } from '../components/suppression/AddMuteModal';
import './MutesPage.css';

const COLS = '1.4fr 170px 180px 1fr 92px';

const errMsg = (e: unknown, fallback: string) =>
  e instanceof ApiError ? e.message : fallback;

const fmtTime = (iso: string) =>
  new Date(iso).toLocaleString(undefined, {
    year: 'numeric',
    month: '2-digit',
    day: '2-digit',
    hour: '2-digit',
    minute: '2-digit',
  });

/** Confirm + lift a mute (destructive-consent modal). */
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
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const submit = () => {
    setBusy(true);
    setError(null);
    api
      .deleteMute(mute.id)
      .then(onDone)
      .catch((e: unknown) => {
        setError(errMsg(e, 'failed to lift mute'));
        setBusy(false);
      });
  };

  return (
    <Modal
      title="Lift mute"
      onClose={onClose}
      footer={
        <>
          <Button variant="outline" onClick={onClose} disabled={busy}>
            Cancel
          </Button>
          <Button variant="danger" onClick={submit} disabled={busy}>
            Lift mute
          </Button>
        </>
      }
    >
      <p className="modal-confirm-text">
        Lift the mute on <strong>{targetName}</strong>
        {mute.metric_name ? (
          <>
            {' '}
            (<span className="mono">{mute.metric_name}</span>)
          </>
        ) : (
          ' (all metrics)'
        )}
        ? Notifications resume immediately.
      </p>
      {error && <p className="form-error">{error}</p>}
    </Modal>
  );
}

export function MutesPage() {
  const authed = useAuthStore((s) => s.authed);
  const [rows, setRows] = useState<Mute[]>([]);
  const [nodes, setNodes] = useState<NodeSummary[]>([]);
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
    api.listNodes().then(setNodes).catch(() => undefined);
    api.listNodeGroups().then(setGroups).catch(() => undefined);
  }, [load]);

  // The mute's target: a node name, or a folder-group name (recursive). Falls back to the raw id.
  const targetName = (m: Mute): string =>
    m.scope_kind === 'group'
      ? groups.find((g) => g.id === m.group_id)?.name ?? m.group_id ?? '—'
      : nodes.find((n) => n.id === m.node_id)?.name ?? m.node_id ?? '—';

  return (
    <div>
      <PageHeader
        title="Mutes"
        trail={[{ label: 'Alerts' }, { label: 'Mutes' }]}
        note="Silence notifications for a node (or one check) until a time — alerts stay visible."
      />

      {unavailable ? (
        <Card>
          <p className="muted">Mute management is unavailable in skeleton mode.</p>
        </Card>
      ) : (
        <>
          <TableToolbar>
            <TableSpacer />
            <ResultCount shown={rows.length} noun="active mutes" />
            {authed && (
              <Button variant="primary" onClick={() => setAdding(true)}>
                + Add mute
              </Button>
            )}
          </TableToolbar>

          <div className="ytable">
            <div className="ytable-scroll">
              <div className="ytable-head" style={{ gridTemplateColumns: COLS }}>
                <div className="ytable-h">Target</div>
                <div className="ytable-h">Metric</div>
                <div className="ytable-h">Until</div>
                <div className="ytable-h">Reason</div>
                <div className="ytable-h right">Actions</div>
              </div>

              {rows.length === 0 ? (
                <div className="yt-empty">
                  <p className="yt-empty-title">{loading ? 'Loading…' : 'No active mutes'}</p>
                  {!loading && (
                    <p className="yt-empty-sub">
                      Silence a node (or one check) until a time to suppress its notifications.
                    </p>
                  )}
                </div>
              ) : (
                rows.map((m) => (
                  <div className="ytable-row" style={{ gridTemplateColumns: COLS }} key={m.id}>
                    <div className="ytable-cell">
                      <span className="mute-target">
                        {m.scope_kind === 'group' && <Badge>group</Badge>}
                        <span className="yt-name-txt">{targetName(m)}</span>
                      </span>
                    </div>
                    <div className="ytable-cell">
                      {m.scope_kind === 'group' ? (
                        <Badge tone="info">incl. subgroups</Badge>
                      ) : m.metric_name ? (
                        <Badge tone="neutral">
                          <span className="mono">{m.metric_name}</span>
                        </Badge>
                      ) : (
                        <Badge tone="info">all metrics</Badge>
                      )}
                    </div>
                    <div className="ytable-cell mono">{fmtTime(m.until_at)}</div>
                    <div className="ytable-cell ellipsis muted">{m.reason}</div>
                    <div className="ytable-cell right">
                      {authed && (
                        <span className="ytable-actions">
                          <IconButton title="Lift mute" danger onClick={() => setLifting(m)}>
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
          nodes={nodes}
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
