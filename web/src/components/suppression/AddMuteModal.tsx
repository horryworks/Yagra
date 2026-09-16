// SPDX-License-Identifier: AGPL-3.0-only
// Create a mute — shared by the Mutes page ("+ Add mute"), the All Nodes right-click "Custom…"
// path, and the per-alert Mute action on Active alerts. A node mute can target one metric; a
// folder-group mute silences every node under the group (recursive incl. subgroups, ADR-022) and
// has no metric. When `initialScope` is set the scope is fixed to that node/group; otherwise it's
// chosen here.

import { useState } from 'react';
import { useTranslation } from 'react-i18next';
import { api, errMsg } from '../../services/api';
import type { NodeGroup } from '../../types/api';
import { Modal } from '../ui/Modal';
import { Button } from '../ui/Button';
import { TextInput, Select } from '../ui/Field';
import { NodePicker } from '../NodePicker/NodePicker';
import { groupOptions } from '../../lib/nodeTree';
import { GroupPicker } from '../ui/GroupPicker';
import { localTimeZone, LIVENESS_METRIC } from '../../lib/format';
import { targetNodeNames, type ActionTarget } from '../../lib/actionTarget';
import { MetricPicker } from '../MetricPicker/MetricPicker';
import { toRfc3339 } from '../../lib/format';

const TZ = localTimeZone();
interface Props {
  /** Only consulted when the scope is chosen here — a locked scope names its own entity. */
  groups?: NodeGroup[];
  /** When set, the scope is fixed to this node, folder or working set (the All Nodes right-click
   *  "Custom…" path, and the selection bar's More… menu since ADR-124 増分 11). */
  initialScope?: ActionTarget;
  /** Metric to pre-fill for a node scope (the per-alert Mute action seeds the metric that fired). */
  initialMetric?: string;
  onClose: () => void;
  onSaved: () => void;
}

export function AddMuteModal({
  groups = [],
  initialScope,
  initialMetric,
  onClose,
  onSaved,
}: Props) {
  const { t } = useTranslation('suppression');
  const locked = !!initialScope;
  // A set of nodes goes through the bulk endpoint, which takes ids rather than one scope.
  const batch = initialScope?.kind === 'nodes' ? initialScope : null;
  const names = initialScope ? targetNodeNames(initialScope) : [];
  const [scopeKind, setScopeKind] = useState<'node' | 'group'>(
    initialScope && initialScope.kind !== 'nodes' ? initialScope.kind : 'node',
  );
  const [scopeId, setScopeId] = useState(
    initialScope && initialScope.kind !== 'nodes' ? initialScope.id : '',
  );
  // Resolved name for the node picker's trigger. NodePicker is a typeahead over the lazily-loaded
  // inventory, so it scales past the old flat <select> of the first 100 nodes (S12).
  const [nodeLabel, setNodeLabel] = useState(
    initialScope?.kind === 'node' ? (initialScope.name ?? '') : '',
  );
  const [check, setCheck] = useState(initialMetric ?? '');
  // The liveness sentinel is an internal token, never shown to an operator (`AlertWhatText` renders
  // it as "Reachability"). It still has to reach the API verbatim — a mute's identity is
  // check_id(node, name) — so the value is kept and the *field* becomes a read-only line instead.
  const livenessCheck = check === LIVENESS_METRIC;
  const [until, setUntil] = useState('');
  const [reason, setReason] = useState('');
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [partial, setPartial] = useState(false);

  const groupItems = groupOptions(groups);
  // A batch supplies its targets as ids, so there is no single `scopeId` to require.
  const ready = (!!batch || !!scopeId) && !!until;

  const submit = () => {
    if (!ready) return;
    setBusy(true);
    setError(null);
    if (batch) {
      // 🚨 A shortfall keeps the dialog open: closing would read as "all of them are quiet".
      api
        .createMutes({
          node_ids: batch.nodes.map((n) => n.id),
          until: toRfc3339(until),
          metric_name: check.trim() || undefined,
          reason: reason.trim() || undefined,
        })
        .then((r) => {
          if (r.created < r.requested) {
            setPartial(true);
            setError(t('muteForm.partial', { created: r.created, requested: r.requested }));
            setBusy(false);
            return;
          }
          onSaved();
          onClose();
        })
        .catch((e: unknown) => {
          setError(errMsg(e, t('muteForm.err.add')));
          setBusy(false);
        });
      return;
    }
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
        setError(errMsg(e, t('muteForm.err.add')));
        setBusy(false);
      });
  };

  return (
    <Modal
      title={t('muteForm.title')}
      onClose={onClose}
      footer={
        <>
          <Button variant="outline" onClick={onClose} disabled={busy}>
            {partial ? t('common:actions.close') : t('common:actions.cancel')}
          </Button>
          <Button variant="primary" onClick={submit} disabled={!ready || busy}>
            {t('muteForm.submit')}
          </Button>
        </>
      }
    >
      {batch ? (
        <div className="modal-field">
          <label className="modal-field-label">{t('muteForm.scope')}</label>
          <p className="modal-hint">{t('muteForm.lockedNodes', { count: names.length })}</p>
          <ul className="form-targets scroll-y">
            {names.map((n, i) => (
              <li key={`${n}-${i}`}>{n}</li>
            ))}
          </ul>
        </div>
      ) : locked ? (
        <div className="modal-field">
          <label className="modal-field-label">{t('muteForm.scope')}</label>
          <p className="modal-hint">
            {initialScope?.kind === 'group'
              ? t('muteForm.lockedGroup')
              : t('muteForm.lockedNode')}
            :{' '}
            <strong>{initialScope && initialScope.kind !== 'nodes' ? initialScope.name : ''}</strong>
            {initialScope?.kind === 'group' && t('muteForm.inclSubgroups')}
          </p>
        </div>
      ) : (
        <>
          <div className="modal-field">
            <label className="modal-field-label">{t('muteForm.scope')}</label>
            <Select
              value={scopeKind}
              onChange={(e) => {
                setScopeKind(e.target.value as 'node' | 'group');
                setScopeId('');
                setNodeLabel('');
              }}
            >
              <option value="node">{t('muteForm.kind.node')}</option>
              <option value="group">{t('muteForm.kind.group')}</option>
            </Select>
          </div>
          <div className="modal-field">
            <label className="modal-field-label">
              {scopeKind === 'group' ? t('muteForm.entityGroup') : t('muteForm.entityNode')}
            </label>
            {scopeKind === 'node' ? (
              <NodePicker
                id="mute-node"
                value={scopeId || null}
                valueLabel={nodeLabel || undefined}
                onChange={(n) => {
                  setScopeId(n?.id ?? '');
                  setNodeLabel(n?.name ?? '');
                }}
                placeholder={t('muteForm.pickNode')}
              />
            ) : (
              <GroupPicker
                options={groupItems}
                value={scopeId}
                onChange={setScopeId}
                emptyOption={t('muteForm.pickGroup')}
                autoFocus
              />
            )}
            {scopeKind === 'group' && (
              <span className="modal-hint">{t('muteForm.groupHint')}</span>
            )}
          </div>
        </>
      )}

      {/* Per-metric mute only applies to a single node — a group mute silences everything. */}
      {scopeKind === 'node' && (
        <div className="modal-field">
          <label className="modal-field-label">{t('muteForm.metric')}</label>
          {livenessCheck ? (
            <p className="modal-hint">
              <strong>{t('format:liveness')}</strong>
            </p>
          ) : (
            <MetricPicker value={check} onChange={setCheck} />
          )}
        </div>
      )}
      <div className="modal-field">
        <label className="modal-field-label">{t('muteForm.until')}</label>
        <TextInput type="datetime-local" value={until} onChange={(e) => setUntil(e.target.value)} />
        <span className="modal-hint">{t('muteForm.tzHint', { tz: TZ })}</span>
      </div>
      <div className="modal-field">
        <label className="modal-field-label">{t('muteForm.reason')}</label>
        <TextInput
          placeholder={t('muteForm.reasonPlaceholder')}
          value={reason}
          onChange={(e) => setReason(e.target.value)}
        />
      </div>
      {error && <p className="form-error">{error}</p>}
    </Modal>
  );
}
