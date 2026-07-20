// SPDX-License-Identifier: AGPL-3.0-only
// Create a maintenance window — shared by the Maintenance page ("+ Add window") and the All Nodes
// right-click "Custom…" path. When `initialScope` is set the scope is fixed to that node/folder
// group (the operator already picked the target); otherwise the scope is chosen here (node /
// folder group / device profile). The legacy tag-based "group" scope is no longer offered for
// creation — a folder group (`group_id`, recursive incl. subgroups, ADR-022) supersedes it; old
// tag-group windows still list and resolve.

import { useState } from 'react';
import { useTranslation } from 'react-i18next';
import { api, ApiError } from '../../services/api';
import type { MaintenanceScopeLevel, NodeGroup, ProfileSummary } from '../../types/api';
import { Modal } from '../ui/Modal';
import { Button } from '../ui/Button';
import { TextInput, Select } from '../ui/Field';
import { NodePicker } from '../NodePicker/NodePicker';
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
  groups: NodeGroup[];
  /** Offered as the "profile" scope when present; omit (or empty) to hide that choice. */
  profiles?: ProfileSummary[];
  /** When set, the scope is fixed to this node/group (the All Nodes right-click "Custom…" path). */
  initialScope?: SuppressionTarget;
  onClose: () => void;
  onSaved: () => void;
}

export function AddMaintenanceWindowModal({
  groups,
  profiles = [],
  initialScope,
  onClose,
  onSaved,
}: Props) {
  const { t } = useTranslation('suppression');
  const locked = !!initialScope;
  const [name, setName] = useState(
    initialScope ? t('maintenanceForm.defaultName', { name: initialScope.name }) : '',
  );
  const [scope, setScope] = useState<CreateScope>(
    initialScope ? (initialScope.kind === 'group' ? 'group_id' : 'node') : 'node',
  );
  const [scopeId, setScopeId] = useState(initialScope?.id ?? '');
  // Resolved name for the node picker's trigger (typeahead over the lazily-loaded inventory, so it
  // scales past the old flat <select> of the first 100 nodes — S12).
  const [nodeLabel, setNodeLabel] = useState(
    initialScope?.kind === 'node' ? (initialScope.name ?? '') : '',
  );
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
        setError(errMsg(e, t('maintenanceForm.err.add')));
        setBusy(false);
      });
  };

  return (
    <Modal
      title={t('maintenanceForm.title')}
      onClose={onClose}
      footer={
        <>
          <Button variant="outline" onClick={onClose} disabled={busy}>
            {t('common:actions.cancel')}
          </Button>
          <Button variant="primary" onClick={submit} disabled={!ready || busy}>
            {t('maintenanceForm.submit')}
          </Button>
        </>
      }
    >
      <div className="modal-field">
        <label className="modal-field-label">{t('maintenanceForm.name')}</label>
        <TextInput
          placeholder={t('maintenanceForm.namePlaceholder')}
          value={name}
          onChange={(e) => setName(e.target.value)}
          autoFocus
        />
      </div>

      {locked ? (
        <div className="modal-field">
          <label className="modal-field-label">{t('maintenanceForm.scope')}</label>
          <p className="modal-hint">
            {initialScope?.kind === 'group'
              ? t('maintenanceForm.lockedGroup')
              : t('maintenanceForm.lockedNode')}
            :{' '}
            <strong>{initialScope?.name}</strong>
            {initialScope?.kind === 'group' && t('maintenanceForm.inclSubgroups')}
          </p>
        </div>
      ) : (
        <>
          <div className="modal-field">
            <label className="modal-field-label">{t('maintenanceForm.scopeLevel')}</label>
            <Select
              value={scope}
              onChange={(e) => {
                setScope(e.target.value as CreateScope);
                setScopeId('');
                setNodeLabel('');
              }}
            >
              <option value="node">{t('maintenanceForm.level.node')}</option>
              <option value="group_id">{t('maintenanceForm.level.groupId')}</option>
              {profiles.length > 0 && (
                <option value="profile">{t('maintenanceForm.level.profile')}</option>
              )}
            </Select>
          </div>
          <div className="modal-field">
            <label className="modal-field-label">{t('maintenanceForm.scope')}</label>
            {scope === 'node' ? (
              <NodePicker
                id="maint-node"
                value={scopeId || null}
                valueLabel={nodeLabel || undefined}
                onChange={(n) => {
                  setScopeId(n?.id ?? '');
                  setNodeLabel(n?.name ?? '');
                }}
                placeholder={t('maintenanceForm.pickNode')}
              />
            ) : scope === 'profile' ? (
              <Select value={scopeId} onChange={(e) => setScopeId(e.target.value)}>
                <option value="">{t('maintenanceForm.pickProfile')}</option>
                {profiles.map((p) => (
                  <option key={p.id} value={p.id}>
                    {p.name}
                  </option>
                ))}
              </Select>
            ) : (
              <Select value={scopeId} onChange={(e) => setScopeId(e.target.value)}>
                <option value="">{t('maintenanceForm.pickGroup')}</option>
                {groupItems.map((g) => (
                  <option key={g.id} value={g.id}>
                    {g.label}
                  </option>
                ))}
              </Select>
            )}
            {scope === 'group_id' && (
              <span className="modal-hint">{t('maintenanceForm.groupHint')}</span>
            )}
          </div>
        </>
      )}

      <div className="modal-field">
        <label className="modal-field-label">{t('common:range.from')}</label>
        <TextInput
          type="datetime-local"
          value={startsAt}
          onChange={(e) => setStartsAt(e.target.value)}
        />
      </div>
      <div className="modal-field">
        <label className="modal-field-label">{t('common:range.to')}</label>
        <TextInput type="datetime-local" value={endsAt} onChange={(e) => setEndsAt(e.target.value)} />
        <span className="modal-hint">{t('maintenanceForm.tzHint', { tz: TZ })}</span>
      </div>
      {error && <p className="form-error">{error}</p>}
    </Modal>
  );
}
