// SPDX-License-Identifier: AGPL-3.0-only
// Edit a node — the fields that kind actually has (ui-conventions §Modals case 3: focused editing).
//
// It replaces a dialog that showed every kind the same five device fields, so a URL monitor was
// offered an SNMP credential the poller can never read and a device profile picker listing switch
// profiles. What each kind gets, and the payload that must still carry the fields it hides, are in
// `nodeEditForm.ts` where a test can reach them; this file is layout plus the call.
//
// The node arrives as a prop rather than being re-fetched by id. The caller has it (and refetches
// it after a save), and the version that fetched its own copy swallowed the failure — a slow or
// failed load left five empty inputs whose Save wrote four NULLs over a working binding. A caller
// that holds only a list row gets `EditNodeModalById` at the bottom, which does the load *outside*
// the form: no fields exist until the node does.

import { Fragment, useEffect, useState, type ReactNode } from 'react';
import { useTranslation } from 'react-i18next';
import { api, errMsg } from '../../services/api';
import { isValidPoolName } from '../../lib/pool';
import { isSnmpCredentialKind } from '../../lib/credentialKinds';
import type { CredentialSummary, NodeDetail, ProfileSummary } from '../../types/api';
import { Button } from '../ui/Button';
import { Modal } from '../ui/Modal';
import { FieldHint, Select, TextInput } from '../ui/Field';
import { DnsCheckFields, Row, UrlCheckFields } from './CheckFields';
import {
  nodeEditDraftFrom,
  nodeEditErrorKey,
  nodeEditRequest,
  profileIsOffKind,
  profileOptions,
  sendNodeEdit,
  visibleNodeEditFields,
  visibleNodeEditSections,
  NODE_EDIT_FIELD_META,
  NODE_EDIT_KIND_SPEC,
  PARTIAL_SAVE_KEY,
  type NodeEditDraft,
  type NodeEditField,
} from './nodeEditForm';

export function EditNodeModal({
  node,
  onClose,
  onDone,
}: {
  node: NodeDetail;
  onClose: () => void;
  onDone: () => void;
}) {
  const { t } = useTranslation('nodes');
  const spec = NODE_EDIT_KIND_SPEC[node.kind];
  const fields = visibleNodeEditFields(node.kind);
  const headed = visibleNodeEditSections(node.kind).length > 1;

  const [d, setD] = useState<NodeEditDraft>(() => nodeEditDraftFrom(node));
  const [profiles, setProfiles] = useState<ProfileSummary[]>([]);
  const [credentials, setCredentials] = useState<CredentialSummary[]>([]);
  // What this node would inherit if its own pool were cleared — the field placeholder, so blanking
  // it is a visible choice rather than a mystery.
  const [inheritedPool, setInheritedPool] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const poolInvalid = !isValidPoolName(d.pool);

  const set = <K extends keyof NodeEditDraft>(k: K, v: NodeEditDraft[K]) =>
    setD((prev) => ({ ...prev, [k]: v }));

  const showsCredential = fields.includes('snmpCredential');

  useEffect(() => {
    api
      .listProfiles()
      .then(setProfiles)
      .catch(() => setProfiles([]));
    // Asking the server keeps the folder walk in one place (yagra-core's resolver) instead of
    // duplicating it here.
    api
      .getNodeAssignment(node.id)
      .then((a) => setInheritedPool(a.pool_source === 'node' ? null : a.pool))
      .catch(() => undefined);
  }, [node.id]);

  useEffect(() => {
    if (!showsCredential) return;
    api
      .listCredentials()
      .then((c) => setCredentials(c.filter((cr) => isSnmpCredentialKind(cr.kind))))
      .catch(() => setCredentials([]));
  }, [showsCredential]);

  const save = () => {
    const built = nodeEditRequest(node.kind, d);
    if ('error' in built) {
      setError(t(`checkEdit.err.${built.error}`));
      return;
    }
    setBusy(true);
    setError(null);
    void sendNodeEdit(node.id, built.req).then((out) => {
      // No `setBusy(false)` on success: it unmounts the dialog, and re-enabling the button first
      // would open a window where a second Save is live against an already-saved node.
      if (out.ok) return onDone();
      const key = nodeEditErrorKey(built.req, out.stage);
      // The partial-save sentence keeps its own wording and takes the server's reason as a clause;
      // every other failure is wholly described by the server's message when there is one.
      setError(
        key === PARTIAL_SAVE_KEY
          ? t(key, { reason: errMsg(out.error, t('err.saveNode')) })
          : errMsg(out.error, t(key)),
      );
      setBusy(false);
    });
  };

  // Keyed by field rather than chained on the kind, so a new field is one entry the compiler
  // demands instead of a branch that quietly falls through to the device form.
  const rows: Record<NodeEditField, ReactNode> = {
    urlCheck: d.url ? (
      <UrlCheckFields draft={d.url} onChange={(url) => set('url', url)} />
    ) : null,
    dnsCheck: d.dns ? (
      <DnsCheckFields draft={d.dns} onChange={(dns) => set('dns', dns)} />
    ) : null,
    profile: (
      <Row
        label={t(spec.profileLabelKey)}
        hint={
          profileIsOffKind(node.kind, profiles, d.profileId)
            ? t('editNode.profileOffKind')
            : undefined
        }
      >
        <Select value={d.profileId} onChange={(e) => set('profileId', e.target.value)}>
          <option value="">{t('add.none')}</option>
          {profileOptions(node.kind, profiles, d.profileId).map((p) => (
            <option key={p.id} value={p.id}>
              {p.name}
            </option>
          ))}
        </Select>
      </Row>
    ),
    snmpCredential: (
      <Row label={t('field.snmpCredential')}>
        <Select value={d.credentialId} onChange={(e) => set('credentialId', e.target.value)}>
          <option value="">{t('add.none')}</option>
          {credentials.map((c) => (
            <option key={c.id} value={c.id}>
              {c.name}
            </option>
          ))}
        </Select>
      </Row>
    ),
    identity: (
      <div className="modal-field-row">
        <Row label={t('field.maker')}>
          <TextInput
            value={d.vendor}
            onChange={(e) => set('vendor', e.target.value)}
            placeholder={t('field.makerPlaceholder')}
          />
        </Row>
        <Row label={t('field.model')}>
          <TextInput
            className="mono"
            value={d.model}
            onChange={(e) => set('model', e.target.value)}
            placeholder={t('field.modelPlaceholder')}
          />
        </Row>
      </div>
    ),
    pool: (
      <Row label={t('field.pool')}>
        <TextInput
          className="mono"
          value={d.pool}
          onChange={(e) => set('pool', e.target.value)}
          placeholder={
            inheritedPool ? t('field.poolInheritPlaceholder', { pool: inheritedPool }) : ''
          }
        />
        <FieldHint error={poolInvalid}>
          {poolInvalid ? t('field.poolInvalid') : t('field.poolHint')}
        </FieldHint>
      </Row>
    ),
  };

  return (
    <Modal
      title={t(spec.titleKey)}
      onClose={onClose}
      footer={
        <>
          <Button variant="outline" onClick={onClose} disabled={busy}>
            {t('common:actions.cancel')}
          </Button>
          <Button variant="primary" onClick={save} disabled={busy || poolInvalid}>
            {t('common:actions.save')}
          </Button>
        </>
      }
    >
      {fields.map((f, i) => {
        const section = NODE_EDIT_FIELD_META[f].section;
        // A heading where the section changes, and only when this kind has both halves: a device's
        // fields are all "the node", and heading a one-part form is noise.
        const opensSection = i === 0 || NODE_EDIT_FIELD_META[fields[i - 1]].section !== section;
        return (
          <Fragment key={f}>
            {headed && opensSection && (
              <p className="nd-edit-section">{t(`editNode.section.${section}`)}</p>
            )}
            {rows[f]}
          </Fragment>
        );
      })}
      {error && <p className="form-error">{error}</p>}
    </Modal>
  );
}

/** The same dialog for a caller that holds only a node's list row — the inventory tree's context
 *  menu, which never fetched the detail the pane has.
 *
 *  The load lives out here rather than inside `EditNodeModal` on purpose, and the header comment
 *  above says why: a dialog that fetches its own copy can paint the form before the answer arrives
 *  or after it failed, and that form's Save writes the blanks it is showing. So the fields do not
 *  exist until the node does — until then this is a shell with a Close and, on failure, the
 *  server's reason. */
export function EditNodeModalById({
  nodeId,
  name,
  onClose,
  onDone,
}: {
  nodeId: string;
  /** The row's name, so the shell has a title before the fetch answers. */
  name: string;
  onClose: () => void;
  onDone: () => void;
}) {
  const { t } = useTranslation('nodes');
  const [node, setNode] = useState<NodeDetail | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    api
      .getNode(nodeId)
      .then((n) => {
        if (!cancelled) setNode(n);
      })
      .catch((e: unknown) => {
        if (!cancelled) setError(errMsg(e, t('err.loadNode')));
      });
    return () => {
      cancelled = true;
    };
  }, [nodeId, t]);

  if (node) return <EditNodeModal node={node} onClose={onClose} onDone={onDone} />;
  return (
    <Modal
      title={name}
      onClose={onClose}
      footer={
        <Button variant="outline" onClick={onClose}>
          {t('common:actions.close')}
        </Button>
      }
    >
      {error ? (
        <p className="form-error">{error}</p>
      ) : (
        <p className="nd-muted">{t('common:loading')}</p>
      )}
    </Modal>
  );
}
