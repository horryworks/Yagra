// SPDX-License-Identifier: AGPL-3.0-only
// Create a mute — shared by the Mutes page ("+ Add mute") and the All Nodes right-click "Custom…"
// path. A node mute can target one metric; a folder-group mute silences every node under the group
// (recursive incl. subgroups, ADR-022) and has no metric. When `initialScope` is set the scope is
// fixed to that node/group; otherwise it's chosen here.

import { useState } from 'react';
import { useTranslation } from 'react-i18next';
import { api, ApiError } from '../../services/api';
import type { NodeGroup } from '../../types/api';
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

// Check-name presets: the liveness check plus the common polled metrics.
const CHECK_PRESETS = ['icmp_rtt_ms', 'icmp_loss_pct', 'snmp_sys_uptime_ticks'];

interface Props {
  groups: NodeGroup[];
  /** When set, the scope is fixed to this node/group (the All Nodes right-click "Custom…" path). */
  initialScope?: SuppressionTarget;
  onClose: () => void;
  onSaved: () => void;
}

export function AddMuteModal({ groups, initialScope, onClose, onSaved }: Props) {
  const { t } = useTranslation('suppression');
  const locked = !!initialScope;
  const [scopeKind, setScopeKind] = useState<'node' | 'group'>(initialScope?.kind ?? 'node');
  const [scopeId, setScopeId] = useState(initialScope?.id ?? '');
  // Resolved name for the node picker's trigger. NodePicker is a typeahead over the lazily-loaded
  // inventory, so it scales past the old flat <select> of the first 100 nodes (S12).
  const [nodeLabel, setNodeLabel] = useState(
    initialScope?.kind === 'node' ? (initialScope.name ?? '') : '',
  );
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
            {t('common:actions.cancel')}
          </Button>
          <Button variant="primary" onClick={submit} disabled={!ready || busy}>
            {t('muteForm.submit')}
          </Button>
        </>
      }
    >
      {locked ? (
        <div className="modal-field">
          <label className="modal-field-label">{t('muteForm.scope')}</label>
          <p className="modal-hint">
            {initialScope?.kind === 'group'
              ? t('muteForm.lockedGroup')
              : t('muteForm.lockedNode')}
            :{' '}
            <strong>{initialScope?.name}</strong>
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
              <Select value={scopeId} onChange={(e) => setScopeId(e.target.value)} autoFocus>
                <option value="">{t('muteForm.pickGroup')}</option>
                {groupItems.map((g) => (
                  <option key={g.id} value={g.id}>
                    {g.label}
                  </option>
                ))}
              </Select>
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
          <TextInput
            className="mono"
            placeholder={t('muteForm.metricPlaceholder')}
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
