// SPDX-License-Identifier: AGPL-3.0-only
// Add or edit a node group (folder): name, type, parent, and poll-pool.
//
// A group's parent doubles as its 'move', so this one dialog covers create, rename, retype, move
// and re-pool. Lived at the bottom of NodesPage as a second component in the page file.

import { useState } from 'react';
import { useTranslation } from 'react-i18next';
import { api, errMsg } from '../../services/api';
import { GROUP_TYPES } from '../../types/api';
import type { GroupType, NodeGroup } from '../../types/api';
import { asGroupType, groupOptions, isSelfOrDescendant } from '../../lib/nodeTree';
import { inheritedGroupPool, isValidPoolName } from '../../lib/pool';
import { geoBodyFrom, geoChanged, geoDraftFrom } from './geoFields';
import { Modal } from '../ui/Modal';
import { Button } from '../ui/Button';
import { TextInput, Select, RequiredMark } from '../ui/Field';

/** Add/edit a group: name, type, and parent (parent doubles as 'move'). */
export interface GroupModalState {
  mode: 'add' | 'edit';
  group?: NodeGroup;
  parentId: string | null;
}

/** Add or edit a group (name + type + parent). Editing the parent moves the group; self and
 *  descendants are excluded from the parent options so a move can't create a cycle. */
export function GroupModal({
  state,
  groups,
  onClose,
  onSaved,
}: {
  state: GroupModalState;
  groups: NodeGroup[];
  onClose: () => void;
  onSaved: () => void;
}) {
  const { t } = useTranslation('nodes');
  const editing = state.mode === 'edit';
  const [name, setName] = useState(state.group?.name ?? '');
  const [type, setType] = useState<GroupType>(asGroupType(state.group?.group_type));
  const [parent, setParent] = useState<string>(
    (editing ? state.group?.parent_id : state.parentId) ?? '',
  );
  /** The folder's own pool ('' ⇒ inherit from an ancestor, else the default pool). */
  const [pool, setPool] = useState(state.group?.pool ?? '');
  /** The folder's map pin. Saved by its own endpoint after the group body — see `save`. */
  const [geo, setGeo] = useState(() => geoDraftFrom(state.group));
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const poolInvalid = !isValidPoolName(pool);
  // What this folder would inherit if its own pool is cleared. Preview only — the authority on what
  // actually polls a node is the server (`getNodeAssignment`), never this walk.
  const inherited = inheritedGroupPool(groups, parent || null);

  // For an edit, a group cannot be parented under itself or any of its descendants.
  const parentChoices = groupOptions(groups).filter(
    (o) => !(editing && state.group && isSelfOrDescendant(groups, state.group.id, o.id)),
  );

  const save = () => {
    // Validate the pin before issuing anything, so a bad coordinate costs no round trip and cannot
    // leave the group saved with the pin rejected.
    const pin = geoBodyFrom(geo);
    if ('error' in pin) {
      setError(t(`err.${pin.error}`));
      return;
    }
    setBusy(true);
    setError(null);
    // `pool` is always sent: '' clears it back to inherited (a JSON-absent field would mean
    // "unchanged" server-side and silently drop the edit).
    const body = {
      name: name.trim(),
      group_type: type,
      parent_id: parent || null,
      pool: pool.trim(),
    };
    const saved: Promise<string> = editing
      ? api.updateNodeGroup(state.group!.id, body).then(() => state.group!.id)
      : api.createNodeGroup(body).then((r) => r.id);
    saved
      // The pin is a sub-resource with its own endpoint (like placement and pool), so saving one
      // is a second request. Skip it when the pin is untouched — on a create with no coordinates
      // entered that is the common case.
      .then((id) => (geoChanged(geo, state.group) ? api.setNodeGroupGeo(id, pin.body) : undefined))
      .then(onSaved)
      .catch((e: unknown) => {
        setError(errMsg(e, t('err.saveGroup')));
        setBusy(false);
      });
  };

  return (
    <Modal
      title={editing ? t('group.edit') : t('group.add')}
      onClose={onClose}
      footer={
        <>
          <Button variant="outline" onClick={onClose} disabled={busy}>
            {t('common:actions.cancel')}
          </Button>
          <Button variant="primary" onClick={save} disabled={!name.trim() || busy || poolInvalid}>
            {t('common:actions.save')}
          </Button>
        </>
      }
    >
      <div className="form-stack">
        <label className="form-label">
          <span>
            {t('field.name')} <RequiredMark />
          </span>
          <TextInput value={name} onChange={(e) => setName(e.target.value)} autoFocus />
        </label>
        <label className="form-label">
          {t('group.type')}
          <Select value={type} onChange={(e) => setType(e.target.value as GroupType)}>
            {GROUP_TYPES.map((gt) => (
              <option key={gt} value={gt}>
                {t(`groupType.${gt}`)}
              </option>
            ))}
          </Select>
        </label>
        <label className="form-label">
          {t('group.parentGroup')}
          <Select value={parent} onChange={(e) => setParent(e.target.value)}>
            <option value="">{t('group.topLevelOption')}</option>
            {parentChoices.map((o) => (
              <option key={o.id} value={o.id}>
                {o.label}
              </option>
            ))}
          </Select>
        </label>
        <label className="form-label">
          {t('group.pool')}
          <TextInput
            className="mono"
            value={pool}
            onChange={(e) => setPool(e.target.value)}
            placeholder={inherited ? t('field.poolInheritPlaceholder', { pool: inherited }) : ''}
          />
          <span className={`form-hint${poolInvalid ? ' form-hint-error' : ''}`}>
            {poolInvalid ? t('field.poolInvalid') : t('group.poolHint')}
          </span>
        </label>
        <div className="form-row">
          <label className="form-label">
            {t('group.latitude')}
            <TextInput
              className="mono"
              inputMode="decimal"
              value={geo.latitude}
              onChange={(e) => setGeo({ ...geo, latitude: e.target.value })}
              placeholder="35.681"
            />
          </label>
          <label className="form-label">
            {t('group.longitude')}
            <TextInput
              className="mono"
              inputMode="decimal"
              value={geo.longitude}
              onChange={(e) => setGeo({ ...geo, longitude: e.target.value })}
              placeholder="139.767"
            />
          </label>
        </div>
        <span className="form-hint">{t('group.geoHint')}</span>
        {error && <p className="form-error">{error}</p>}
      </div>
    </Modal>
  );
}
