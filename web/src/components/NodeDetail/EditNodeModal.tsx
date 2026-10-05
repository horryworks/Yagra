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
import { isValidPoolName, poolPlaceholder } from '../../lib/pool';
import { isSnmpCredentialKind } from '../../lib/credentialKinds';
import type { CredentialSummary, NodeDetail, ProfileSummary } from '../../types/api';
import { Button } from '../ui/Button';
import { IconButton } from '../ui/IconButton';
import { Modal } from '../ui/Modal';
import { FieldError, Select, TextArea, TextInput } from '../ui/Field';
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
  isValidNodeName,
  isValidNotes,
  NODE_EDIT_FIELD_META,
  NODE_EDIT_KIND_SPEC,
  NOTES_MAX,
  PARTIAL_SAVE_KEY,
  withProfileChoice,
  type NodeEditDraft,
  type NodeEditField,
} from './nodeEditForm';
import { ChipInput } from '../ui/ChipInput';
import { LABELS_MAX, firstLabelProblem, labelsAreValid } from '../ui/labelRules';
import { Badge } from '../ui/Badge';
import { FormError, FormFooter } from '../ui/FormFooter';
import { done, WordedFailure } from '../../lib/submitState';
import { useSubmit } from '../../lib/useSubmit';

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
  const form = useSubmit({ errorFallback: t('err.saveNode'), onDone });
  const poolInvalid = !isValidPoolName(d.pool);
  const nameInvalid = !isValidNodeName(d.name);
  const notesTooLong = !isValidNotes(d.notes);
  const tagsInvalid = !labelsAreValid(d.tags);
  // What the folder chain still supplies after this draft's refusals — recomputed from the draft
  // rather than from `node.inherited_tags` alone, so ✕-ing a chip removes it from this list at
  // once instead of after a round trip.
  const inherited = (node.inherited_tags ?? []).filter((l) => !d.tagsExcluded.includes(l));

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
      form.refuse(t(`checkEdit.err.${built.error}`));
      return;
    }
    form.submit(() =>
      sendNodeEdit(node.id, built.req).then((out) => {
        if (out.ok) return done();
        const key = nodeEditErrorKey(built.req, out.stage);
        // The partial-save sentence keeps its own wording and takes the server's reason as a
        // clause — and part of the edit landed, so the dialog offers "Close". Every other failure
        // is wholly described by the server's message when there is one.
        if (key === PARTIAL_SAVE_KEY) {
          return {
            kind: 'keepOpen' as const,
            message: t(key, { reason: errMsg(out.error, t('err.saveNode')) }),
            refresh: false,
          };
        }
        throw new WordedFailure(errMsg(out.error, t(key)));
      }),
    );
  };

  // Keyed by field rather than chained on the kind, so a new field is one entry the compiler
  // demands instead of a branch that quietly falls through to the device form.
  const rows: Record<NodeEditField, ReactNode> = {
    name: (
      <Row label={t('field.name')} required>
        <TextInput value={d.name} onChange={(e) => set('name', e.target.value)} />
        {nameInvalid && <FieldError>{t('field.nameRequired')}</FieldError>}
      </Row>
    ),
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
        <Select
          value={d.profileId}
          onChange={(e) => {
            const next = e.target.value;
            setD((prev) => withProfileChoice(prev, next));
          }}
        >
          <option value="">{t('add.none')}</option>
          {profileOptions(node.kind, profiles, d.profileId).map((p) => (
            <option key={p.id} value={p.id}>
              {p.name}
            </option>
          ))}
        </Select>
      </Row>
    ),
    // Not a `Row`: the checkbox carries its own label, and that label says what the lock does
    // ("keep this profile when classification rules change") — which is also why Reclassify
    // leaves the node out, so that is not said again under it (ADR-200 Inc.24).
    profileLock: (
      <div className="modal-field nd-profile-lock">
        <label className="nd-profile-lock-check">
          <input
            type="checkbox"
            checked={d.profileLocked}
            onChange={(e) => set('profileLocked', e.target.checked)}
          />
          <span>{t('editNode.profileLock')}</span>
        </label>
      </div>
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
          placeholder={poolPlaceholder(inheritedPool, t)}
        />
        {poolInvalid && <FieldError>{t('field.poolInvalid')}</FieldError>}
      </Row>
    ),
    // ⚠️ Not wrapped in `Row`: that renders a `<label>`, and a label holding several inputs and a
    // button gives every one of them the same accessible name and sends a click on the label to
    // whichever came first. Each input carries its own `aria-label` instead.
    tags: (
      <div className="modal-field nd-tags">
        {/* The count beside the label is the cap the entry box closes at; "sent with alerts" is
            where a tag goes. Both used to be a 206-character hint under the field (ADR-200). */}
        <span className="modal-field-label">
          {t('field.tags')}
          <span className="nd-field-sub">{t('field.sentWithAlerts')}</span>
          <span className="nd-field-sub mono">
            {d.tags.length} / {LABELS_MAX}
          </span>
        </span>
        <ChipInput
          value={d.tags}
          onChange={(next) => set('tags', next)}
          placeholder={t('field.tagPlaceholder')}
          inputLabel={t('field.tags')}
        />
        {/* What the folder chain supplies, and the refusals over it. Hidden entirely when this
            node inherits nothing and refuses nothing — an empty section would be a claim nobody
            made. */}
        {(inherited.length > 0 || d.tagsExcluded.length > 0) && (
          <div className="nd-inherited-tags">
            {inherited.length > 0 && (
              <>
                <span className="modal-field-label">{t('field.tagsInherited')}</span>
                <ul className="nd-tag-badges">
                  {inherited.map((label) => (
                    <li key={label}>
                      <Badge tone="tag">{label}</Badge>
                      {/* ✕ here EXCLUDES rather than deletes: the label lives on the folder, and
                          this node cannot edit it — only refuse it. */}
                      <IconButton
                        title={t('field.tagExclude', { label })}
                        onClick={() => set('tagsExcluded', [...d.tagsExcluded, label])}
                      >
                        ✕
                      </IconButton>
                    </li>
                  ))}
                </ul>
              </>
            )}
            {d.tagsExcluded.length > 0 && (
              <>
                <span className="modal-field-label">{t('field.tagsExcluded')}</span>
                {/* Every refusal, including ones naming a label no folder currently supplies: an
                    exclusion that cannot be seen cannot be undone, and it stays meaningful because
                    an ancestor may re-add that label later. `lenient`, because a stored label may
                    predate the rules new ones follow. */}
                <ChipInput
                  value={d.tagsExcluded}
                  onChange={(next) => set('tagsExcluded', next)}
                  inputLabel={t('field.tagsExcluded')}
                  lenient
                />
              </>
            )}
          </div>
        )}
        {/* A stored tag the rules now refuse (migration 0109) is marked on its chip; this line
            says why Save is held, since a chip's reason is otherwise a hover. */}
        {tagsInvalid && (
          <FieldError>{t(`field.tagErr.${firstLabelProblem(d.tags)}`)}</FieldError>
        )}
      </div>
    ),
    notes: (
      <Row label={t('field.notes')} sub={t('field.sentWithAlerts')}>
        <TextArea
          rows={4}
          value={d.notes}
          onChange={(e) => set('notes', e.target.value)}
          placeholder={t('field.notesPlaceholder')}
        />
        {notesTooLong && (
          <FieldError>{t('field.notesTooLong', { max: NOTES_MAX })}</FieldError>
        )}
      </Row>
    ),
  };

  return (
    <Modal
      title={t(spec.titleKey)}
      onClose={onClose}
      footer={
        <FormFooter
          form={form}
          onClose={onClose}
          onSubmit={save}
          submitLabel={t('common:actions.save')}
          canSubmit={!(poolInvalid || nameInvalid || notesTooLong || tagsInvalid)}
        />
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
      <FormError form={form} />
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
        <FormError form={{ error }} />
      ) : (
        <p className="nd-muted">{t('common:loading')}</p>
      )}
    </Modal>
  );
}
