// SPDX-License-Identifier: AGPL-3.0-only
// Add a monitor: a device (SNMP/ICMP), a URL/HTTP(S) endpoint, or a DNS name.
//
// It lived inside NodesPage, where its ~20 fields sat in the same scope as the inventory tree's
// state and a hand-written `resetAddForm` had to name every one of them to clear the dialog. Here
// the dialog *is* the component, so closing it unmounts the state — reset is structural, and there
// is no list to forget an entry in. (The old reset never cleared the failure message, so reopening
// the form showed the previous attempt's error above a blank form.)

import { useEffect, useMemo, useState, type ReactNode } from 'react';
import { Trans, useTranslation } from 'react-i18next';
import { api, errMsg } from '../../services/api';
import { isValidPoolName } from '../../lib/pool';
import {
  isAddableKind,
  monitorKind,
  MONITOR_KINDS,
  targetFilled,
  type AddableKind,
} from '../../pages/monitorKinds';
import type {
  CredentialSummary,
  DnsRecordType,
  NodeGroup,
  ProfileSummary,
} from '../../types/api';
import { Modal } from '../ui/Modal';
import { Button } from '../ui/Button';
import { TextInput, Select, RequiredMark } from '../ui/Field';
import { NodePicker } from '../NodePicker/NodePicker';
import {
  createRequest,
  sendCreate,
  EMPTY_ADD_NODE_FORM,
  type AddNodeForm,
} from './addNodeRequest';

export function AddNodeModal({
  groups,
  groupId,
  onClose,
  onCreated,
}: {
  groups: NodeGroup[];
  /** Folder the new node lands in (from the tree's right-click); `null` = top level. */
  groupId: string | null;
  onClose: () => void;
  onCreated: () => void;
}) {
  const { t } = useTranslation('nodes');
  const [kind, setKind] = useState<AddableKind>('device');
  const [form, setForm] = useState<AddNodeForm>(EMPTY_ADD_NODE_FORM);
  /** The parent picker's display label — UI only, so it is not part of the request payload. */
  const [parentName, setParentName] = useState('');
  const [profiles, setProfiles] = useState<ProfileSummary[]>([]);
  const [credentials, setCredentials] = useState<CredentialSummary[]>([]);
  const [error, setError] = useState<string | null>(null);
  /** In flight. Creation is not idempotent, so an unguarded second click makes a second node —
   *  and the operator only finds out when a duplicate appears in the tree. */
  const [busy, setBusy] = useState(false);

  const set = <K extends keyof AddNodeForm>(key: K, value: AddNodeForm[K]) =>
    setForm((f) => ({ ...f, [key]: value }));

  // Binding options for the device form. Fetched on open (the dialog mounts when it opens).
  useEffect(() => {
    api.listProfiles().then(setProfiles).catch(() => setProfiles([]));
    api
      .listCredentials()
      .then((c) => setCredentials(c.filter((cr) => cr.kind === 'snmp_v2c')))
      .catch(() => setCredentials([]));
  }, []);

  // Per-kind strings and the required-target rule come from the MONITOR_KINDS registry.
  const spec = monitorKind(kind);
  const title = t(spec.titleKey);
  const canSubmit =
    form.name.trim() !== '' &&
    targetFilled(kind, { address: form.address, url: form.url, dnsName: form.dnsName });

  const submit = () => {
    setBusy(true);
    setError(null);
    sendCreate(createRequest(kind, form))
      // The create endpoints take no group_id, so a node lands Ungrouped; when the add was launched
      // from a folder's right-click, place it there with the canonical op (same as drag-drop). A
      // placement failure is soft — the node still exists, just at top level.
      .then(({ id }) => (groupId ? api.setNodeGroup(id, groupId) : undefined))
      // No `setBusy(false)` here: success unmounts the dialog, and re-enabling it first would open
      // a window where the button is live again with the node already created.
      .then(onCreated)
      .catch((e: unknown) => {
        setError(errMsg(e, t(spec.errorKey)));
        setBusy(false);
      });
  };

  // Keyed by kind rather than chained on it, so the compiler demands an entry for a fourth kind
  // instead of quietly falling through to the device branch of a ternary.
  const targetFields: Record<AddableKind, ReactNode> = {
    url: (
      <>
        <label className="form-label">
          <span>
            {t('field.url')} <RequiredMark />
          </span>
          <TextInput
            className="mono"
            value={form.url}
            onChange={(e) => set('url', e.target.value)}
            placeholder="https://api.example.com/health"
          />
        </label>
        <div className="form-row">
          <label className="form-label">
            {t('add.method')}
            <Select
              value={form.urlMethod}
              onChange={(e) => set('urlMethod', e.target.value as AddNodeForm['urlMethod'])}
            >
              <option value="GET">GET</option>
              <option value="HEAD">HEAD</option>
              <option value="POST">POST</option>
            </Select>
          </label>
          <label className="form-label form-check">
            <input
              type="checkbox"
              checked={form.verifyTls}
              onChange={(e) => set('verifyTls', e.target.checked)}
            />
            <span>{t('add.verifyTls')}</span>
          </label>
        </div>
      </>
    ),
    dns: (
      <>
        <label className="form-label">
          <span>
            {t('field.dnsName')} <RequiredMark />
          </span>
          <TextInput
            className="mono"
            value={form.dnsName}
            onChange={(e) => set('dnsName', e.target.value)}
            placeholder={t('add.dnsNamePlaceholder')}
          />
        </label>
        <div className="form-row">
          <label className="form-label">
            {t('field.recordType')}
            <Select
              value={form.dnsRecordType}
              onChange={(e) => set('dnsRecordType', e.target.value as DnsRecordType)}
            >
              <option value="A">A</option>
              <option value="AAAA">AAAA</option>
              <option value="CNAME">CNAME</option>
            </Select>
          </label>
          <label className="form-label">
            {t('add.resolverOptional')}
            <TextInput
              className="mono"
              value={form.dnsResolver}
              onChange={(e) => set('dnsResolver', e.target.value)}
              placeholder={t('add.resolverPlaceholder')}
            />
          </label>
        </div>
      </>
    ),
    device: (
      <>
        <label className="form-label">
          <span>
            {t('field.ipAddress')} <RequiredMark />
          </span>
          <TextInput
            className="mono"
            value={form.address}
            onChange={(e) => set('address', e.target.value)}
            placeholder={t('add.addressPlaceholder')}
          />
        </label>
        <label className="form-label">
          {t('add.deviceProfileOptional')}
          <Select value={form.profileId} onChange={(e) => set('profileId', e.target.value)}>
            <option value="">{t('add.none')}</option>
            {profiles.map((p) => (
              <option key={p.id} value={p.id}>
                {p.name}
              </option>
            ))}
          </Select>
        </label>
        <label className="form-label">
          {t('add.snmpCredentialOptional')}
          <Select value={form.credentialId} onChange={(e) => set('credentialId', e.target.value)}>
            <option value="">{t('add.none')}</option>
            {credentials.map((c) => (
              <option key={c.id} value={c.id}>
                {c.name}
              </option>
            ))}
          </Select>
        </label>
      </>
    ),
  };

  /** Kind-specific fields that belong *below* the parent picker. Only the device form has any. */
  const trailingFields: Record<AddableKind, ReactNode> = {
    url: null,
    dns: null,
    device: (
      <div className="form-row">
        <label className="form-label">
          {t('add.makerOptional')}
          <TextInput
            value={form.vendor}
            onChange={(e) => set('vendor', e.target.value)}
            placeholder={t('field.makerPlaceholder')}
          />
        </label>
        <label className="form-label">
          {t('add.modelOptional')}
          <TextInput
            className="mono"
            value={form.model}
            onChange={(e) => set('model', e.target.value)}
            placeholder={t('field.modelPlaceholder')}
          />
        </label>
      </div>
    ),
  };

  const poolValid = isValidPoolName(form.pool);
  const target = useMemo(
    () => groups.find((g) => g.id === groupId)?.name ?? t('add.topLevel'),
    [groups, groupId, t],
  );

  return (
    <Modal
      title={title}
      onClose={onClose}
      footer={
        <>
          <Button onClick={onClose} disabled={busy}>
            {t('common:actions.cancel')}
          </Button>
          <Button variant="primary" onClick={submit} disabled={!canSubmit || busy}>
            {title}
          </Button>
        </>
      }
    >
      <div className="form-stack">
        <p className="form-note">
          <Trans t={t} i18nKey="add.addingTo" values={{ target }} components={{ b: <strong /> }} />
        </p>
        <label className="form-label">
          {t('add.monitoringType')}
          <Select
            value={kind}
            onChange={(e) => {
              // Narrowed, not cast: the value arrives as a string, and casting it would admit a
              // kind with no create endpoint into the form state.
              if (isAddableKind(e.target.value)) setKind(e.target.value);
            }}
          >
            {MONITOR_KINDS.map((k) => (
              <option key={k.kind} value={k.kind}>
                {t(k.optionKey)}
              </option>
            ))}
          </Select>
        </label>
        <label className="form-label">
          <span>
            {t('field.name')} <RequiredMark />
          </span>
          <TextInput value={form.name} onChange={(e) => set('name', e.target.value)} autoFocus />
        </label>
        {targetFields[kind]}
        <label className="form-label">
          {t('add.parentNodeOptional')}
          {/* Server-side typeahead (A-2) — the parent can be any node in the fleet, not just the
              lazily-loaded ones, so this never needs the whole inventory in the browser. */}
          <NodePicker
            value={form.parentId || null}
            valueLabel={parentName || undefined}
            onChange={(n) => {
              set('parentId', n?.id ?? '');
              setParentName(n?.name ?? '');
            }}
            placeholder={t('add.none')}
          />
        </label>
        {trailingFields[kind]}
        <label className="form-label">
          {t('add.poolOptional')}
          <TextInput
            className="mono"
            value={form.pool}
            onChange={(e) => set('pool', e.target.value)}
            placeholder={t('add.poolPlaceholder')}
          />
          <span className={`form-hint${poolValid ? '' : ' form-hint-error'}`}>
            {poolValid ? t('add.poolHint') : t('field.poolInvalid')}
          </span>
        </label>
        {error && <p className="form-error">{error}</p>}
      </div>
    </Modal>
  );
}
