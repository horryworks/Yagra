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
import { GroupPicker } from '../ui/GroupPicker';
import { inheritedGroupPool, isValidPoolName } from '../../lib/pool';
import { geoBodyFrom, geoChanged, geoDraftFrom, inheritedPin } from './geoFields';
import {
  prefixBodyFrom,
  prefixDraftFrom,
  prefixesChanged,
  syncOwnedRows,
} from './prefixFields';
import { Modal } from '../ui/Modal';
import { Button } from '../ui/Button';
import { TextInput, Select, RequiredMark } from '../ui/Field';
import './GroupModal.css';

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
  /** The folder's hand-made IP ranges (ADR-131). Same shape as the pin: a sub-resource with its
   *  own endpoint, saved after the group body, and skipped entirely when untouched. */
  const [prefixRows, setPrefixRows] = useState(() => prefixDraftFrom(state.group));
  /** Ranges a NetBox sync maintains — shown, never editable. */
  const syncRows = syncOwnedRows(state.group);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const poolInvalid = !isValidPoolName(pool);
  // What this folder would inherit if its own pool is cleared. Preview only — the authority on what
  // actually polls a node is the server (`getNodeAssignment`), never this walk.
  const inherited = inheritedGroupPool(groups, parent || null);
  // Whether this folder is already on the map through an ancestor. Unlike the pool preview above
  // there is no client-side walk here: the server resolved it, and a second answer is exactly what
  // would let the dialog and the map disagree.
  const pinnedAtId = inheritedPin(state.group, geo, parent || null);
  const pinnedAt = pinnedAtId ? groups.find((g) => g.id === pinnedAtId) : undefined;

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
    // Same rule for the ranges: validated before any request, so a bad row costs no round trip and
    // cannot leave the folder saved with its ranges rejected. ⚠️ This checks emptiness, duplicates
    // and the caps only — the CIDR itself is the server's to judge (see `prefixFields.ts`).
    const ranges = prefixBodyFrom(prefixRows, state.group);
    if ('error' in ranges) {
      setError(t(`err.${ranges.error}`, { prefix: ranges.prefix ?? '' }));
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
      .then(async (id) => {
        if (geoChanged(geo, state.group)) await api.setNodeGroupGeo(id, pin.body);
        // Skipped when untouched, for the reason the pin is: on a create with no ranges typed
        // that is the common case, and an unchanged dialog should issue nothing.
        if (prefixesChanged(prefixRows, state.group)) {
          await api.setNodeGroupPrefixes(id, ranges.body);
        }
      })
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
          <GroupPicker
            options={parentChoices}
            value={parent}
            onChange={setParent}
            emptyOption={t('group.topLevelOption')}
          />
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
        {/* The folder's IP ranges (ADR-131). A sub-resource with its own endpoint, saved after the
            group body exactly as the pin is. Ranges a sync maintains are listed above the editable
            ones and are disabled rather than hidden: the value is worth reading, and only the
            action goes (`ui-conventions.md`). */}
        <div className="form-label gm-prefixes">
          <span>{t('group.prefixes')}</span>
          {syncRows.length > 0 && (
            <>
              {syncRows.map((r) => (
                <div className="gm-prefix-row" key={`sync-${r.prefix}`}>
                  <TextInput className="mono" value={r.prefix} disabled />
                  <TextInput value={r.description} disabled />
                  <span className="gm-prefix-badge">{t('group.prefixSource.sync')}</span>
                </div>
              ))}
              <span className="form-hint">{t('group.prefixSyncHint')}</span>
            </>
          )}
          {prefixRows.map((r, i) => (
            // Index keys: the rows are an ordered editable list with no stable id of their own,
            // and a range is not one until the server has canonicalised it.
            <div className="gm-prefix-row" key={`manual-${i}`}>
              <TextInput
                className="mono"
                value={r.prefix}
                onChange={(e) =>
                  setPrefixRows(
                    prefixRows.map((x, j) => (j === i ? { ...x, prefix: e.target.value } : x)),
                  )
                }
                placeholder={t('group.prefixPlaceholder')}
              />
              <TextInput
                value={r.description}
                onChange={(e) =>
                  setPrefixRows(
                    prefixRows.map((x, j) =>
                      j === i ? { ...x, description: e.target.value } : x,
                    ),
                  )
                }
                placeholder={t('group.prefixDescPlaceholder')}
              />
              <Button
                variant="outline"
                onClick={() => setPrefixRows(prefixRows.filter((_, j) => j !== i))}
                aria-label={t('group.prefixRemove')}
              >
                ✕
              </Button>
            </div>
          ))}
          <div>
            <Button
              variant="outline"
              onClick={() => setPrefixRows([...prefixRows, { prefix: '', description: '' }])}
            >
              {t('group.prefixAdd')}
            </Button>
          </div>
          <span className="form-hint">{t('group.prefixesHint')}</span>
        </div>
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
        <span className="form-hint">
          {/* Falls back to the plain hint when the supplying ancestor is outside the caller's
              scope — naming a folder they cannot see would be worse than saying nothing. */}
          {pinnedAt ? t('group.geoInherited', { name: pinnedAt.name }) : t('group.geoHint')}
        </span>
        {error && <p className="form-error">{error}</p>}
      </div>
    </Modal>
  );
}
