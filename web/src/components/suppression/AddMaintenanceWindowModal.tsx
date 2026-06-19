// Create a maintenance window — shared by the Maintenance page ("+ Add window") and the All Nodes
// right-click "Custom…" path. When `initialScope` is set the scope is fixed to that node/folder
// group (the operator already picked the target); otherwise the scope is chosen here (node /
// folder group / device profile). The legacy tag-based "group" scope is no longer offered for
// creation — a folder group (`group_id`, recursive incl. subgroups, ADR-022) supersedes it; old
// tag-group windows still list and resolve.

import { useState } from 'react';
import { api, ApiError } from '../../services/api';
import type {
  MaintenanceScopeLevel,
  NodeGroup,
  NodeSummary,
  ProfileSummary,
} from '../../types/api';
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

/** Scope choices when not locked to a right-click target. */
type CreateScope = 'node' | 'group_id' | 'profile';

interface Props {
  nodes: NodeSummary[];
  groups: NodeGroup[];
  /** Offered as the "profile" scope when present; omit (or empty) to hide that choice. */
  profiles?: ProfileSummary[];
  /** When set, the scope is fixed to this node/group (the All Nodes right-click "Custom…" path). */
  initialScope?: SuppressionTarget;
  onClose: () => void;
  onSaved: () => void;
}

export function AddMaintenanceWindowModal({
  nodes,
  groups,
  profiles = [],
  initialScope,
  onClose,
  onSaved,
}: Props) {
  const locked = !!initialScope;
  const [name, setName] = useState(initialScope ? `Maintenance — ${initialScope.name}` : '');
  const [scope, setScope] = useState<CreateScope>(
    initialScope ? (initialScope.kind === 'group' ? 'group_id' : 'node') : 'node',
  );
  const [scopeId, setScopeId] = useState(initialScope?.id ?? '');
  const [startsAt, setStartsAt] = useState('');
  const [endsAt, setEndsAt] = useState('');
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const groupItems = groupOptions(groups);
  const ready = !!name.trim() && !!scopeId.trim() && !!startsAt && !!endsAt;

  const submit = () => {
    if (!ready) return;
    setBusy(true);
    setError(null);
    api
      .createMaintenanceWindow({
        name: name.trim(),
        scope_level: scope as MaintenanceScopeLevel,
        scope_id: scopeId.trim(),
        starts_at: toRfc3339(startsAt),
        ends_at: toRfc3339(endsAt),
      })
      .then(() => {
        onSaved();
        onClose();
      })
      .catch((e: unknown) => {
        setError(errMsg(e, 'failed to add window'));
        setBusy(false);
      });
  };

  return (
    <Modal
      title="Add maintenance window"
      onClose={onClose}
      footer={
        <>
          <Button variant="outline" onClick={onClose} disabled={busy}>
            Cancel
          </Button>
          <Button variant="primary" onClick={submit} disabled={!ready || busy}>
            Add window
          </Button>
        </>
      }
    >
      <div className="modal-field">
        <label className="modal-field-label">Name</label>
        <TextInput
          placeholder="e.g. core switch firmware"
          value={name}
          onChange={(e) => setName(e.target.value)}
          autoFocus
        />
      </div>

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
            <label className="modal-field-label">Scope level</label>
            <Select
              value={scope}
              onChange={(e) => {
                setScope(e.target.value as CreateScope);
                setScopeId('');
              }}
            >
              <option value="node">node</option>
              <option value="group_id">folder group</option>
              {profiles.length > 0 && <option value="profile">device profile</option>}
            </Select>
          </div>
          <div className="modal-field">
            <label className="modal-field-label">Scope</label>
            {scope === 'node' ? (
              <Select value={scopeId} onChange={(e) => setScopeId(e.target.value)}>
                <option value="">(pick a node)</option>
                {nodes.map((n) => (
                  <option key={n.id} value={n.id}>
                    {n.name}
                  </option>
                ))}
              </Select>
            ) : scope === 'profile' ? (
              <Select value={scopeId} onChange={(e) => setScopeId(e.target.value)}>
                <option value="">(pick a profile)</option>
                {profiles.map((p) => (
                  <option key={p.id} value={p.id}>
                    {p.name}
                  </option>
                ))}
              </Select>
            ) : (
              <Select value={scopeId} onChange={(e) => setScopeId(e.target.value)}>
                <option value="">(pick a group)</option>
                {groupItems.map((g) => (
                  <option key={g.id} value={g.id}>
                    {g.label}
                  </option>
                ))}
              </Select>
            )}
            {scope === 'group_id' && (
              <span className="modal-hint">Covers the group and all its subgroups.</span>
            )}
          </div>
        </>
      )}

      <div className="modal-field">
        <label className="modal-field-label">From</label>
        <TextInput
          type="datetime-local"
          value={startsAt}
          onChange={(e) => setStartsAt(e.target.value)}
        />
      </div>
      <div className="modal-field">
        <label className="modal-field-label">To</label>
        <TextInput type="datetime-local" value={endsAt} onChange={(e) => setEndsAt(e.target.value)} />
        <span className="modal-hint">
          “From”/“To” are in {TZ} (your browser’s local time) and are stored as UTC.
        </span>
      </div>
      {error && <p className="form-error">{error}</p>}
    </Modal>
  );
}
