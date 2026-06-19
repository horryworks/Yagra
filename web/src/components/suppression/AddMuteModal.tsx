// Create a mute — shared by the Mutes page ("+ Add mute") and the All Nodes right-click "Custom…"
// path. A node mute can target one metric; a folder-group mute silences every node under the group
// (recursive incl. subgroups, ADR-022) and has no metric. When `initialScope` is set the scope is
// fixed to that node/group; otherwise it's chosen here.

import { useState } from 'react';
import { api, ApiError } from '../../services/api';
import type { NodeGroup, NodeSummary } from '../../types/api';
import { Modal } from '../ui/Modal';
import { Button } from '../ui/Button';
import { TextInput, Select } from '../ui/Field';
import { groupOptions } from '../../lib/nodeTree';
import { localTimeZone } from '../../lib/format';
import type { SuppressionTarget } from '../../lib/suppression';

const TZ = localTimeZone();
const toRfc3339 = (local: string) => new Date(local).toISOString();
const errMsg = (e: unknown, fallback: string) =>
  e instanceof ApiError ? e.message : fallback;

// Check-name presets: the liveness check plus the common polled metrics.
const CHECK_PRESETS = ['icmp_rtt_ms', 'icmp_loss_pct', 'snmp_sys_uptime_ticks'];

interface Props {
  nodes: NodeSummary[];
  groups: NodeGroup[];
  /** When set, the scope is fixed to this node/group (the All Nodes right-click "Custom…" path). */
  initialScope?: SuppressionTarget;
  onClose: () => void;
  onSaved: () => void;
}

export function AddMuteModal({ nodes, groups, initialScope, onClose, onSaved }: Props) {
  const locked = !!initialScope;
  const [scopeKind, setScopeKind] = useState<'node' | 'group'>(initialScope?.kind ?? 'node');
  const [scopeId, setScopeId] = useState(initialScope?.id ?? '');
  const [check, setCheck] = useState('');
  const [until, setUntil] = useState('');
  const [reason, setReason] = useState('');
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const groupItems = groupOptions(groups);
  const ready = !!scopeId && !!until;

  const submit = () => {
    if (!ready) return;
    setBusy(true);
    setError(null);
    api
      .createMute({
        scope_kind: scopeKind,
        scope_id: scopeId,
        metric_name: scopeKind === 'node' ? check.trim() || undefined : undefined,
        until: toRfc3339(until),
        reason: reason.trim() || undefined,
      })
      .then(() => {
        onSaved();
        onClose();
      })
      .catch((e: unknown) => {
        setError(errMsg(e, 'failed to add mute'));
        setBusy(false);
      });
  };

  return (
    <Modal
      title="Add mute"
      onClose={onClose}
      footer={
        <>
          <Button variant="outline" onClick={onClose} disabled={busy}>
            Cancel
          </Button>
          <Button variant="primary" onClick={submit} disabled={!ready || busy}>
            Add mute
          </Button>
        </>
      }
    >
      {locked ? (
        <div className="modal-field">
          <label className="modal-field-label">Scope</label>
          <p className="modal-hint">
            {initialScope?.kind === 'group' ? 'Folder group' : 'Node'}:{' '}
            <strong>{initialScope?.name}</strong>
            {initialScope?.kind === 'group' && ' (incl. subgroups)'}
          </p>
        </div>
      ) : (
        <>
          <div className="modal-field">
            <label className="modal-field-label">Scope</label>
            <Select
              value={scopeKind}
              onChange={(e) => {
                setScopeKind(e.target.value as 'node' | 'group');
                setScopeId('');
              }}
            >
              <option value="node">node</option>
              <option value="group">folder group</option>
            </Select>
          </div>
          <div className="modal-field">
            <label className="modal-field-label">{scopeKind === 'group' ? 'Group' : 'Node'}</label>
            {scopeKind === 'node' ? (
              <Select value={scopeId} onChange={(e) => setScopeId(e.target.value)} autoFocus>
                <option value="">(pick a node)</option>
                {nodes.map((n) => (
                  <option key={n.id} value={n.id}>
                    {n.name}
                  </option>
                ))}
              </Select>
            ) : (
              <Select value={scopeId} onChange={(e) => setScopeId(e.target.value)} autoFocus>
                <option value="">(pick a group)</option>
                {groupItems.map((g) => (
                  <option key={g.id} value={g.id}>
                    {g.label}
                  </option>
                ))}
              </Select>
            )}
            {scopeKind === 'group' && (
              <span className="modal-hint">Silences every node under the group and its subgroups.</span>
            )}
          </div>
        </>
      )}

      {/* Per-metric mute only applies to a single node — a group mute silences everything. */}
      {scopeKind === 'node' && (
        <div className="modal-field">
          <label className="modal-field-label">Metric</label>
          <TextInput
            className="mono"
            placeholder="metric (empty = whole node)"
            list="mute-check-presets"
            value={check}
            onChange={(e) => setCheck(e.target.value)}
          />
          <datalist id="mute-check-presets">
            {CHECK_PRESETS.map((c) => (
              <option key={c} value={c} />
            ))}
          </datalist>
        </div>
      )}
      <div className="modal-field">
        <label className="modal-field-label">Until</label>
        <TextInput type="datetime-local" value={until} onChange={(e) => setUntil(e.target.value)} />
        <span className="modal-hint">In {TZ} (your browser’s local time) and stored as UTC.</span>
      </div>
      <div className="modal-field">
        <label className="modal-field-label">Reason</label>
        <TextInput
          placeholder="reason (optional)"
          value={reason}
          onChange={(e) => setReason(e.target.value)}
        />
      </div>
      {error && <p className="form-error">{error}</p>}
    </Modal>
  );
}
