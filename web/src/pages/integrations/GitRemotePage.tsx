// SPDX-License-Identifier: AGPL-3.0-only
// Settings ▸ Integrations ▸ Git. Where config backups are pushed (ADR-210 stage 4): one Git
// repository for the whole deployment, reached with an HTTPS token or with an SSH key Yagra made.
//
// Structured after `NetboxIntegrationPage`, for the reason it gives: one card, the form in a
// dialog. The judgement — what the last push means, what the form sends — is in `gitRemote.ts`.

import { useState } from 'react';
import { useTranslation } from 'react-i18next';
import { api, errMsg } from '../../services/api';
import { useCan } from '../../store';
import type { RemoteView } from '../../types/api';
import { PageHeader } from '../../components/ui/PageHeader';
import { Card } from '../../components/ui/Card';
import { Button } from '../../components/ui/Button';
import { Modal } from '../../components/ui/Modal';
import { TextInput, FieldError } from '../../components/ui/Field';
import { ConfirmDeleteModal } from '../../components/ui/ConfirmDeleteModal';
import { CopyCommand } from '../../components/ui/CopyCommand';
import { EmptyState } from '../../components/ui/EmptyState';
import { LoadBlockNotice } from '../../components/ui/LoadBlockNotice';
import { SecretInput } from '../../components/ui/SecretInput';
import { LoadGate } from '../../components/ui/LoadGate';
import { useLoad } from '../../lib/useLoad';
import { formatTimestamp } from '../../lib/format';
import { done } from '../../lib/submitState';
import { useSubmit } from '../../lib/useSubmit';
import { FormFooter } from '../../components/ui/FormFooter';
import {
  authForUrl,
  canSaveRemote,
  pushLine,
  remoteBody,
  storedTokenUsable,
  type GitRemoteForm,
} from './gitRemote';
import './GitRemotePage.css';

function RemoteModal({
  existing,
  onClose,
  onSaved,
}: {
  existing: RemoteView;
  onClose: () => void;
  onSaved: () => void;
}) {
  const { t } = useTranslation('system');
  const [form, setForm] = useState<GitRemoteForm>({
    url: existing.url ?? '',
    branch: existing.branch ?? '',
    username: existing.username ?? '',
    token: '',
  });
  const submit = useSubmit({ errorFallback: t('git.err.save'), onDone: onSaved });
  const auth = authForUrl(form.url);
  const tokenStored = existing.auth === 'https_token';
  // A sealed token goes only to the host it was given for; another host needs its own.
  const tokenUsable = storedTokenUsable(existing.url ?? null, existing.auth ?? null, form.url);
  const body = remoteBody(form);
  const save = () => {
    if (!body) return;
    submit.submit(() => api.putConfigBackupRemote(body).then(() => done()));
  };

  return (
    <Modal
      title={existing.configured ? t('git.form.editTitle') : t('git.form.addTitle')}
      onClose={onClose}
      footer={
        <FormFooter
          form={submit}
          onClose={onClose}
          onSubmit={save}
          submitLabel={t('common:actions.save')}
          canSubmit={canSaveRemote(form, tokenUsable)}
        />
      }
    >
      <label className="git-field">
        <span>{t('git.form.url')}</span>
        <TextInput
          value={form.url}
          placeholder="https://github.com/acme/configs.git"
          onChange={(e) => setForm({ ...form, url: e.target.value })}
        />
        {form.url.trim() !== '' && !auth && <FieldError>{t('git.form.urlShape')}</FieldError>}
      </label>
      <label className="git-field">
        <span>{t('git.form.branch')}</span>
        <TextInput
          value={form.branch}
          placeholder="main"
          onChange={(e) => setForm({ ...form, branch: e.target.value })}
        />
      </label>
      {auth === 'https_token' && (
        <>
          <label className="git-field">
            <span>{t('git.form.username')}</span>
            <TextInput
              value={form.username}
              placeholder="x-access-token"
              onChange={(e) => setForm({ ...form, username: e.target.value })}
            />
          </label>
          <div className="git-field">
            <label htmlFor="git-token">{t('git.form.token')}</label>
            <SecretInput
              id="git-token"
              stored={tokenStored}
              mustReplace={tokenStored && !tokenUsable}
              value={form.token}
              onChange={(token) => setForm({ ...form, token })}
            />
          </div>
        </>
      )}
      {auth === 'ssh_key' && existing.auth !== 'ssh_key' && (
        <p className="form-status">{t('git.form.keyMade')}</p>
      )}
      {/* The danger, said at the moment it is chosen (ADR-200): what leaves the box. */}
      <p className="form-warning">{t('git.form.warning')}</p>
    </Modal>
  );
}

export function GitRemotePage() {
  const { t } = useTranslation('system');
  // The permission every handler here checks (`RequireManageSystem`, ADR-056).
  const canManage = useCan('manage_system');
  const [editing, setEditing] = useState(false);
  const [confirm, setConfirm] = useState<'delete' | 'key' | null>(null);
  const [forgetError, setForgetError] = useState<string | null>(null);
  // Not confirmed: the next push learns the keys again, so nothing is lost.
  const forget = () => {
    setForgetError(null);
    api
      .forgetConfigBackupRemoteHostKeys()
      .then(() => load.reload())
      .catch((e: unknown) => setForgetError(errMsg(e, t('git.err.save'))));
  };
  const load = useLoad(() => api.getConfigBackupRemote(), [], { initial: null as RemoteView | null });
  const remote = load.data;

  const header = (
    <PageHeader
      title={t('git.name')}
      trail={[
        { label: t('nav:sections.settings') },
        { label: t('nav:settings.integrations'), to: '/settings/integrations' },
        { label: t('git.name') },
      ]}
      note={t('git.note')}
    />
  );
  if (!canManage) {
    return (
      <div>
        {header}
        <LoadBlockNotice block="forbidden" permission="manage_system" />
      </div>
    );
  }

  const line = remote ? pushLine(remote.status) : null;
  return (
    <div>
      {header}
      <LoadGate load={load}>
        {remote && !remote.configured && (
          <Card title={t('git.card.title')}>
            <EmptyState
              text={t('git.card.empty')}
              action={
                <Button variant="primary" onClick={() => setEditing(true)}>
                  {t('git.card.set')}
                </Button>
              }
            />
          </Card>
        )}
        {remote?.configured && (
          <Card
            title={t('git.card.title')}
            actions={
              <>
                <Button onClick={() => setEditing(true)}>{t('common:actions.edit')}</Button>
                <Button onClick={() => setConfirm('delete')}>{t('common:actions.delete')}</Button>
              </>
            }
          >
            {line && (
              <p className={`git-line ${line.key === 'git.push.failed' ? 'bad' : ''}`}>
                {line.key === 'git.push.ok' &&
                  t(line.key, { at: formatTimestamp(Date.parse(line.at)), head: line.head })}
                {line.key === 'git.push.failed' &&
                  t(line.key, {
                    at: formatTimestamp(Date.parse(line.at)),
                    why: t(`git.failure.${line.failure}`),
                  })}
                {(line.key === 'git.push.waiting' || line.key === 'git.push.none') && t(line.key)}
              </p>
            )}
            <dl className="git-facts">
              <dt>{t('git.form.url')}</dt>
              <dd className="mono">{remote.url}</dd>
              <dt>{t('git.form.branch')}</dt>
              <dd className="mono">{remote.branch}</dd>
              {remote.auth === 'https_token' && (
                <>
                  <dt>{t('git.form.username')}</dt>
                  <dd className="mono">{remote.username}</dd>
                </>
              )}
              {remote.auth === 'ssh_key' && remote.ssh_public_key && (
                <>
                  <dt>{t('git.card.deployKey')}</dt>
                  <dd>
                    <CopyCommand command={remote.ssh_public_key} />
                    <Button onClick={() => setConfirm('key')}>{t('git.card.newKey')}</Button>
                  </dd>
                </>
              )}
              {remote.host_keys.length > 0 && (
                <>
                  <dt>{t('git.card.hostKeys')}</dt>
                  <dd>
                    {remote.host_keys.map((k) => (
                      <div key={k} className="mono">
                        {k}
                      </div>
                    ))}
                    <Button onClick={forget}>{t('git.card.forget')}</Button>
                    {forgetError && <FieldError>{forgetError}</FieldError>}
                  </dd>
                </>
              )}
            </dl>
          </Card>
        )}
      </LoadGate>

      {editing && remote && (
        <RemoteModal
          existing={remote}
          onClose={() => setEditing(false)}
          onSaved={() => {
            setEditing(false);
            load.reload();
          }}
        />
      )}
      {confirm && (
        <ConfirmDeleteModal
          title={t(`git.confirm.${confirm}.title`)}
          confirmLabel={t(`git.confirm.${confirm}.action`)}
          onConfirm={() =>
            confirm === 'delete'
              ? api.deleteConfigBackupRemote()
              : api.newConfigBackupRemoteKey().then(() => undefined)
          }
          errorFallback={t('git.err.save')}
          onClose={() => setConfirm(null)}
          onDone={() => {
            setConfirm(null);
            load.reload();
          }}
        >
          {t(`git.confirm.${confirm}.body`)}
        </ConfirmDeleteModal>
      )}
    </div>
  );
}
