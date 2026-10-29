// SPDX-License-Identifier: AGPL-3.0-only
// Credentials & secrets (Settings ▸ Credentials & secrets). Stores monitoring secrets
// (SNMP communities, v3 creds, API tokens) — the crown jewels. The list NEVER includes secret
// values (the API returns metadata only); the secret is write-only here and sent over the
// encrypted-at-rest create endpoint. ManageCredentials-gated.
//
// Data-table standard v2: a toolbar (search + type filter + count + "+ Add credential") over the
// shared `.ytable`. Add/edit/delete all go through modals — the type select drives the add form
// (snmp_v3 reveals the USM sub-form). snmp_v3 secrets are structured (USM): the form collects
// user / level / auth / privacy fields and serializes them into the JSON document the backend
// validates and seals. Edit: name is always editable; the secret is never returned, so it's left
// intact unless the operator opts to replace it (then kind + secret are re-entered and re-sealed).

import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { Trans, useTranslation } from 'react-i18next';
import { api, errMsg } from '../services/api';
import { useCopy } from '../lib/useCopy';
import { rowActionsWidth } from '../lib/rowActions';
import { moveWithinGroup } from '../lib/reorder';
import { useCan } from '../store';
import type { CredentialSummary, RevealedCredential } from '../types/api';
import { PageHeader } from '../components/ui/PageHeader';
import { Button } from '../components/ui/Button';
import { ConfirmDeleteModal } from '../components/ui/ConfirmDeleteModal';
import { Modal } from '../components/ui/Modal';
import { TextInput, Select, FieldError } from '../components/ui/Field';
import { SecretInput } from '../components/ui/SecretInput';
import { ScreenLink } from '../components/ui/ScreenLink';
import { OverflowMenu } from '../components/ui/OverflowMenu';
import { ListToolbar } from '../components/ui/ListToolbar';
import { columnLabels } from '../lib/listToolbar';
import { DataTable, type Column } from '../components/ui/DataTable';
import { useClientFilters } from '../lib/useClientFilters';
import { sortRows } from '../lib/tableSort';
import { useSortParams } from '../lib/useSortParams';
import { SealedSecret, CopyableId } from '../components/ui/tableCells';
import { ClippedText } from '../components/ui/ClippedText';
import { HashIcon, ShieldIcon, KeyIcon, EditIcon, TrashIcon, LockIcon } from '../components/ui/icons';
import { Tabs } from '../components/ui/Tabs';
import { EmptyState } from '../components/ui/EmptyState';
import { useEnumParam } from '../lib/useEnumParam';
import type { ComponentType } from 'react';
import {
  buildV3Secret,
  emptyV3,
  v3Ready,
  V3_AUTH_PROTOCOLS,
  V3_LEVELS,
  V3_PRIV_PROTOCOLS,
  type V3State,
} from './v3Credential';
import {
  buildHttpAuthSecret,
  emptyHttpAuth,
  headerNameRefused,
  httpAuthReady,
  HTTP_AUTH_SCHEMES,
  type HttpAuthScheme,
  type HttpAuthState,
} from './httpAuthCredential';
import {
  buildCliLoginSecret,
  cliLoginReady,
  emptyCliLogin,
  type CliLoginState,
} from './cliLoginCredential';
import {
  canReorderCredentials,
  credentialFilters,
  credentialSortIn,
  credentialSortValues,
  CREDENTIAL_SORT_KEYS,
  DEFAULT_CREDENTIAL_SORT,
} from './credentialList';
import {
  CREDENTIAL_GROUPS,
  CREDENTIAL_KINDS,
  creatableKindsIn,
  credentialGroupOf,
  integrationSecret,
  isRevealableCredentialKind,
  REVEAL_AUTO_CLOSE_MS,
  secretReplacementFor,
  shownRevealFields,
  type CredentialGroup,
  type CredentialKind,
} from '../lib/credentialKinds';
import './CredentialsPage.css';
import { useLoad } from '../lib/useLoad';
import { LoadGate } from '../components/ui/LoadGate';
import {
  CREDENTIAL_ADD_TITLE_KEYS,
  CREDENTIAL_GROUP_LABEL_KEYS,
  CREDENTIAL_TABLE_PREFIX,
  credentialGroupCounts,
  integrationUsageLabel,
  isHeldByIntegration,
  kindLabel,
  totalUsage,
  usageLabel,
  visibleCredentialGroups,
} from './credentialList';
import { done } from '../lib/submitState';
import { useSubmit } from '../lib/useSubmit';
import { FormError, FormFooter } from '../components/ui/FormFooter';

/** The creatable kinds and their type come from `lib/credentialKinds.ts`, which is also what the
 *  node-binding, URL-monitor and discovery pickers filter on — one list, so a kind added here
 *  cannot be one those pickers refuse to offer. */
const KINDS = CREDENTIAL_KINDS;
type Kind = CredentialKind;

// Kind → { i18n label key, icon }. The label is resolved with `t` at the call site (never at
// module load) so it follows the active language.
/** The icon per credential kind. ⚠️ The **label** half lives in `credentialList.ts` — icons are
 *  components, and that file is loaded by a test in a node environment. `kindIcons` in this file
 *  and `CREDENTIAL_KIND_LABEL_KEYS` there are keyed by the same strings, and
 *  `credentialList.test.ts` pins that they agree. */
const KIND_ICONS: Record<string, ComponentType> = {
  snmp_v2c: HashIcon,
  snmp_v3: ShieldIcon,
  http_auth: KeyIcon,
  api_token: KeyIcon,
  // Meraki keys are created via Settings ▸ Integrations; shown here read-only.
  meraki_api: KeyIcon,
  // Likewise NetBox tokens (ADR-100) — created on the integration page, never here.
  netbox_token: KeyIcon,
  // The config-backup Git destination's token or key (ADR-210 stage 4) - set on its integration
  // page, never here.
  git_remote: KeyIcon,
  // A device's command-line login for config backup (ADR-210 stage 2).
  cli_login: LockIcon,
};

/** The CLI login sub-form (ADR-210 stage 2): the login, and the privileged-mode password when the
 *  account does not land privileged. */
function CliLoginFields({
  value,
  onChange,
}: {
  value: CliLoginState;
  onChange: (next: CliLoginState) => void;
}) {
  const { t } = useTranslation('access');
  return (
    <>
      <div className="modal-field">
        <label className="modal-field-label" htmlFor="cred-cli-user">
          {t('cred.cli.username')}
        </label>
        <TextInput
          id="cred-cli-user"
          autoComplete="off"
          placeholder={t('cred.cli.none')}
          value={value.username}
          onChange={(e) => onChange({ ...value, username: e.target.value })}
        />
      </div>
      <div className="modal-field">
        <label className="modal-field-label" htmlFor="cred-cli-password">
          {t('cred.cli.password')}
        </label>
        <SecretInput
          id="cred-cli-password"
          stored={false}
          value={value.password}
          onChange={(password) => onChange({ ...value, password })}
        />
      </div>
      <div className="modal-field">
        <label className="modal-field-label" htmlFor="cred-cli-enable">
          {t('cred.cli.enable')}
        </label>
        <SecretInput
          id="cred-cli-enable"
          stored={false}
          placeholder={t('cred.cli.none')}
          value={value.enable}
          onChange={(enable) => onChange({ ...value, enable })}
        />
      </div>
    </>
  );
}



/** The HTTP-auth sub-form. Only the selected scheme's fields render, but every field stays in
 *  state, so switching scheme and back does not discard what was typed. */
function HttpAuthFields({
  value,
  onChange,
}: {
  value: HttpAuthState;
  onChange: (v: HttpAuthState) => void;
}) {
  const { t } = useTranslation('access');
  const set = (patch: Partial<HttpAuthState>) => onChange({ ...value, ...patch });
  return (
    <>
      <div className="modal-field">
        <label className="modal-field-label">{t('cred.http.scheme')}</label>
        <Select
          value={value.scheme}
          onChange={(e) => set({ scheme: e.target.value as HttpAuthScheme })}
        >
          {HTTP_AUTH_SCHEMES.map((s) => (
            <option key={s} value={s}>
              {t(`cred.http.schemeName.${s}`)}
            </option>
          ))}
        </Select>
      </div>
      {value.scheme === 'basic' && (
        <>
          <div className="modal-field">
            <label className="modal-field-label">{t('cred.http.username')}</label>
            <TextInput value={value.username} onChange={(e) => set({ username: e.target.value })} />
          </div>
          <div className="modal-field">
            <label className="modal-field-label">{t('cred.http.password')}</label>
            <TextInput
              className="mono"
              type="password"
              value={value.password}
              onChange={(e) => set({ password: e.target.value })}
              autoComplete="new-password"
            />
          </div>
        </>
      )}
      {value.scheme === 'bearer' && (
        <div className="modal-field">
          <label className="modal-field-label">{t('cred.http.token')}</label>
          <TextInput
            className="mono"
            type="password"
            value={value.token}
            onChange={(e) => set({ token: e.target.value })}
            autoComplete="new-password"
          />
        </div>
      )}
      {value.scheme === 'header' && (
        <>
          <div className="modal-field">
            <label className="modal-field-label">{t('cred.http.headerName')}</label>
            <TextInput
              className="mono"
              placeholder="X-API-Key"
              value={value.headerName}
              onChange={(e) => set({ headerName: e.target.value })}
            />
            {headerNameRefused(value.headerName) && (
              <FieldError>{t('cred.http.headerNameRefused')}</FieldError>
            )}
          </div>
          <div className="modal-field">
            <label className="modal-field-label">{t('cred.http.headerValue')}</label>
            <TextInput
              className="mono"
              type="password"
              value={value.headerValue}
              onChange={(e) => set({ headerValue: e.target.value })}
              autoComplete="new-password"
            />
          </div>
        </>
      )}
    </>
  );
}

/** The SNMPv3 (USM) sub-form. Controlled — the same fields back the add and edit modals. */
function V3Fields({ value, onChange }: { value: V3State; onChange: (v: V3State) => void }) {
  const { t } = useTranslation('access');
  const needsAuth = value.level !== 'noauth';
  const needsPriv = value.level === 'authpriv';
  const set = (patch: Partial<V3State>) => onChange({ ...value, ...patch });
  return (
    <>
      <div className="modal-field">
        <label className="modal-field-label">{t('cred.v3.usmUser')}</label>
        <TextInput
          className="mono"
          placeholder={t('cred.v3.usmUserPlaceholder')}
          value={value.user}
          onChange={(e) => set({ user: e.target.value })}
        />
      </div>
      <div className="modal-field">
        <label className="modal-field-label">{t('cred.v3.securityLevel')}</label>
        <Select
          value={value.level}
          onChange={(e) => set({ level: e.target.value as (typeof V3_LEVELS)[number] })}
        >
          {V3_LEVELS.map((l) => (
            <option key={l} value={l}>
              {l}
            </option>
          ))}
        </Select>
      </div>
      {needsAuth && (
        <div className="cred-v3-pair">
          <div className="modal-field">
            <label className="modal-field-label">{t('cred.v3.authProtocol')}</label>
            <Select value={value.authProto} onChange={(e) => set({ authProto: e.target.value })}>
              {V3_AUTH_PROTOCOLS.map((p) => (
                <option key={p} value={p}>
                  {p}
                </option>
              ))}
            </Select>
          </div>
          <div className="modal-field">
            <label className="modal-field-label">{t('cred.v3.authPassphrase')}</label>
            <TextInput
              className="mono"
              type="password"
              value={value.authKey}
              onChange={(e) => set({ authKey: e.target.value })}
              autoComplete="new-password"
            />
          </div>
        </div>
      )}
      {needsPriv && (
        <div className="cred-v3-pair">
          <div className="modal-field">
            <label className="modal-field-label">{t('cred.v3.privacyProtocol')}</label>
            <Select value={value.privProto} onChange={(e) => set({ privProto: e.target.value })}>
              {V3_PRIV_PROTOCOLS.map((p) => (
                <option key={p} value={p}>
                  {p}
                </option>
              ))}
            </Select>
          </div>
          <div className="modal-field">
            <label className="modal-field-label">{t('cred.v3.privacyPassphrase')}</label>
            <TextInput
              className="mono"
              type="password"
              value={value.privKey}
              onChange={(e) => set({ privKey: e.target.value })}
              autoComplete="new-password"
            />
          </div>
        </div>
      )}
    </>
  );
}

/** Create a credential (type-driven focused-editing modal). `kinds` is what the tab it was opened
 *  from may create (ADR-205 Inc.1), and the first of them is selected; `title` names that group
 *  ("Add SNMP credential"). */
function AddCredentialModal({
  kinds,
  title,
  onClose,
  onSaved,
}: {
  kinds: readonly Kind[];
  title: string;
  onClose: () => void;
  onSaved: () => void;
}) {
  const { t } = useTranslation('access');
  const [name, setName] = useState('');
  const [kind, setKind] = useState<Kind>(kinds[0] ?? 'snmp_v2c');
  const [secret, setSecret] = useState('');
  const [v3, setV3] = useState<V3State>(emptyV3);
  const [httpAuth, setHttpAuth] = useState<HttpAuthState>(emptyHttpAuth);
  const [cli, setCli] = useState<CliLoginState>(emptyCliLogin);
  const form = useSubmit({
    errorFallback: t('cred.err.add'),
    onDone: () => {
      onSaved();
      onClose();
    },
  });

  const isV3 = kind === 'snmp_v3';
  const isHttpAuth = kind === 'http_auth';
  const isCli = kind === 'cli_login';
  const ready =
    name.trim() !== '' &&
    (isV3
      ? v3Ready(v3)
      : isHttpAuth
        ? httpAuthReady(httpAuth)
        : isCli
          ? cliLoginReady(cli)
          : secret !== '');

  const submit = () => {
    if (!ready) return;
    form.submit(() =>
      api
        .createCredential({
          name: name.trim(),
          kind,
          secret: isV3
            ? buildV3Secret(v3)
            : isHttpAuth
              ? buildHttpAuthSecret(httpAuth)
              : isCli
                ? buildCliLoginSecret(cli)
                : secret,
        })
        .then(() => done()),
    );
  };

  return (
    <Modal
      title={title}
      onClose={onClose}
      footer={
        <FormFooter
          form={form}
          onClose={onClose}
          onSubmit={submit}
          submitLabel={title}
          canSubmit={ready}
        />
      }
    >
      <div className="modal-field">
        <label className="modal-field-label">{t('cred.field.name')}</label>
        <TextInput
          placeholder={t('cred.add.namePlaceholder')}
          value={name}
          onChange={(e) => setName(e.target.value)}
          autoFocus
        />
      </div>
      <div className="modal-field">
        <label className="modal-field-label">{t('cred.field.type')}</label>
        <Select value={kind} onChange={(e) => setKind(e.target.value as Kind)}>
          {kinds.map((k) => (
            <option key={k} value={k}>
              {kindLabel(k, t)}
            </option>
          ))}
        </Select>
      </div>
      {isV3 ? (
        <V3Fields value={v3} onChange={setV3} />
      ) : isHttpAuth ? (
        <HttpAuthFields value={httpAuth} onChange={setHttpAuth} />
      ) : isCli ? (
        <CliLoginFields value={cli} onChange={setCli} />
      ) : (
        <div className="modal-field">
          <label className="modal-field-label" htmlFor="cred-add-secret">
            {t('cred.field.secret')}
          </label>
          <SecretInput
            id="cred-add-secret"
            stored={false}
            placeholder={
              kind === 'api_token'
                ? t('cred.add.secretPlaceholder.apiToken')
                : t('cred.add.secretPlaceholder.community')
            }
            value={secret}
            onChange={setSecret}
          />
        </div>
      )}
      <FormError form={form} />
    </Modal>
  );
}

/** Edit a credential: rename always; optionally replace the (write-only) secret. */
function EditCredentialModal({
  cred,
  onClose,
  onSaved,
}: {
  cred: CredentialSummary;
  onClose: () => void;
  onSaved: () => void;
}) {
  const { t } = useTranslation('access');
  // Whether the kind may change, is fixed, or the secret cannot be replaced from this build at all
  // — decided in `credentialKinds.ts`, where a test reaches it. An integration's key keeps its kind.
  const replacement = secretReplacementFor(cred.kind);
  const [name, setName] = useState(cred.name);
  const [replace, setReplace] = useState(false);
  const [kind, setKind] = useState<Kind>(
    replacement.mode === 'choose' ? replacement.initial : 'snmp_v2c',
  );
  const [secret, setSecret] = useState('');
  const [v3, setV3] = useState<V3State>(emptyV3);
  const [cli, setCli] = useState<CliLoginState>(emptyCliLogin);
  const form = useSubmit({
    errorFallback: t('cred.err.update'),
    onDone: () => {
      onSaved();
      onClose();
    },
  });

  const fixedKind = replacement.mode === 'fixed' ? replacement.kind : null;
  const isV3 = fixedKind === null && kind === 'snmp_v3';
  const isCli = fixedKind === null && kind === 'cli_login';
  const typed = fixedKind ? secret.trim() : secret;
  const secretReady =
    !replace || (isV3 ? v3Ready(v3) : isCli ? cliLoginReady(cli) : typed !== '');
  const ready = name.trim() !== '' && secretReady;

  const save = () => {
    const body = !replace
      ? { name: name.trim() }
      : fixedKind
        ? { name: name.trim(), kind: fixedKind, secret: integrationSecret(fixedKind, secret) }
        : {
            name: name.trim(),
            kind,
            secret: isV3 ? buildV3Secret(v3) : isCli ? buildCliLoginSecret(cli) : secret,
          };
    form.submit(() =>
      api
        .updateCredential(cred.id, body)
        .then(() => done()),
    );
  };

  return (
    <Modal
      title={t('cred.edit.title')}
      onClose={onClose}
      footer={
        <FormFooter
          form={form}
          onClose={onClose}
          onSubmit={save}
          submitLabel={t('common:actions.save')}
          canSubmit={ready}
        />
      }
    >
      <div className="modal-field">
        <label className="modal-field-label">{t('cred.field.name')}</label>
        <TextInput value={name} onChange={(e) => setName(e.target.value)} autoFocus />
      </div>
      {/* A kind this build does not know gets no replacement at all: it cannot know the shape the
          secret is sealed in, and a control that can only do harm is not drawn. */}
      {/* The stored secret is a masked mark with Replace (`SecretInput`); Replace opens the type
          and the new value, and "Keep stored" closes them again without sending a secret. */}
      {replacement.mode !== 'rename_only' && !replace && (
        <div className="modal-field">
          <label className="modal-field-label">{t('cred.field.secret')}</label>
          <SecretInput
            stored
            value=""
            onChange={setSecret}
            replacing={false}
            onReplacingChange={setReplace}
          />
        </div>
      )}
      {replace && (
        <>
          <div className="modal-field">
            <label className="modal-field-label">{t('cred.field.type')}</label>
            {fixedKind ? (
              // An integration's key keeps its kind: the select never held it, so it showed
              // another one over it and saved that. `.modal-field-value` is what a field holds
              // where a control cannot go — it keeps the row the height every other field is.
              <span className="modal-field-value">{kindLabel(fixedKind, t)}</span>
            ) : (
              <Select value={kind} onChange={(e) => setKind(e.target.value as Kind)}>
                {KINDS.map((k) => (
                  <option key={k} value={k}>
                    {kindLabel(k, t)}
                  </option>
                ))}
              </Select>
            )}
          </div>
          {isV3 || isCli ? (
            <>
              {isV3 ? (
                <V3Fields value={v3} onChange={setV3} />
              ) : (
                <CliLoginFields value={cli} onChange={setCli} />
              )}
              <div className="cred-keep">
                <Button type="button" onClick={() => setReplace(false)}>
                  {t('common:secret.keep')}
                </Button>
              </div>
            </>
          ) : (
            <div className="modal-field">
              <label className="modal-field-label" htmlFor="cred-new-secret">
                {t('cred.edit.newSecret')}
              </label>
              <SecretInput
                id="cred-new-secret"
                stored
                value={secret}
                onChange={setSecret}
                replacing
                onReplacingChange={setReplace}
              />
            </div>
          )}
        </>
      )}
      <FormError form={form} />
    </Modal>
  );
}

/** A device credential's secret, read back for a person signing in to the device (ADR-210).
 *
 *  Asked for when the dialog opens — the server writes an audit row for every ask — and held only
 *  in this component, so closing the dialog drops it. It closes by itself after
 *  `REVEAL_AUTO_CLOSE_MS`, so a secret is not left on an unattended screen. */
function RevealCredentialModal({ cred, onClose }: { cred: CredentialSummary; onClose: () => void }) {
  const { t } = useTranslation('access');
  const { copied, copy } = useCopy();
  const [revealed, setRevealed] = useState<RevealedCredential | null>(null);
  const [error, setError] = useState<string | null>(null);
  useEffect(() => {
    let live = true;
    api
      .revealCredential(cred.id)
      .then((r) => live && setRevealed(r))
      .catch((e: unknown) => live && setError(errMsg(e, t('cred.err.reveal'))));
    const timer = setTimeout(onClose, REVEAL_AUTO_CLOSE_MS);
    return () => {
      live = false;
      clearTimeout(timer);
    };
  }, [cred.id, onClose, t]);

  return (
    <Modal
      title={t('cred.reveal.title', { name: cred.name })}
      onClose={onClose}
      footer={
        <Button variant="primary" onClick={onClose}>
          {t('common:actions.close')}
        </Button>
      }
    >
      {error && (
        <p className="form-error" role="alert">
          {error}
        </p>
      )}
      {!revealed && !error && <p className="form-status">{t('common:loading')}</p>}
      {shownRevealFields(revealed?.fields ?? []).map((f) => (
        <div className="modal-field" key={f.field}>
          <label className="modal-field-label">{t(`cred.reveal.field.${f.field}`)}</label>
          <div className="cred-reveal-row">
            <code className="cred-reveal-value mono">{f.value}</code>
            <Button variant="outline" onClick={() => copy(f.value, f.field)}>
              {copied === f.field ? t('common:copy.copied') : t('cred.reveal.copy')}
            </Button>
          </div>
        </div>
      ))}
    </Modal>
  );
}

/** Confirm + delete a credential (destructive-consent modal) — or, for one an integration still
 *  uses, say that it cannot be deleted and where it has to be released first.
 *
 *  The two kinds of "in use" end differently. Nodes lose their binding when the credential goes,
 *  so that delete is offered with a warning. A Meraki organization or a NetBox server cannot lose
 *  theirs (`409 credential_in_use`), and a Delete button that can only be refused is a control the
 *  operator cannot use (ADR-056) — so that dialog has no Delete, only the reason. */
function DeleteCredentialModal({
  cred,
  onClose,
  onDone,
}: {
  cred: CredentialSummary;
  onClose: () => void;
  onDone: () => void;
}) {
  const { t } = useTranslation('access');
  if (isHeldByIntegration(cred)) {
    return (
      <Modal
        title={t('cred.delete.title')}
        onClose={onClose}
        footer={
          <Button variant="outline" onClick={onClose}>
            {t('common:actions.close')}
          </Button>
        }
      >
        <p className="modal-confirm-text">
          <Trans
            t={t}
            i18nKey="cred.delete.held"
            values={{ usage: integrationUsageLabel(cred, t) }}
            components={{ lnk: <ScreenLink to="/settings/integrations" /> }}
          />
        </p>
      </Modal>
    );
  }
  return (
    <ConfirmDeleteModal
      title={t('cred.delete.title')}
      onConfirm={() => api.deleteCredential(cred.id)}
      errorFallback={t('cred.err.delete')}
      onClose={onClose}
      onDone={onDone}
    >
      <Trans
        t={t}
        i18nKey="cred.delete.confirm"
        values={{ name: cred.name }}
        components={{ strong: <strong /> }}
      />{' '}
      {/* The node count on its own, not `usageLabel`: this sentence is about the bindings the
          delete clears, and by this point nothing else uses the credential. */}
      {cred.used_by > 0
        ? t('cred.delete.inUse', { usage: t('cred.usage.count', { count: cred.used_by }) })
        : t('cred.delete.unused')}{' '}
      {t('cred.delete.irreversible')}
    </ConfirmDeleteModal>
  );
}

export function CredentialsPage() {
  const { t } = useTranslation('access');
  // In the URL (ADR-153), so a reload or a shared link opens the same tab (ADR-205 Inc.1).
  const [tab, setTab] = useEnumParam('tab', CREDENTIAL_GROUPS, 'snmp');
  const creds = useLoad(() => api.listCredentials(), [], { initial: [] as CredentialSummary[] });
  const { data: rows, reload: load } = creds;

  const counts = useMemo(() => credentialGroupCounts(rows), [rows]);
  const visible = visibleCredentialGroups(counts);
  // `other` is drawn only while a row needs it; a link to it after the last such row went opens the
  // default tab rather than a tab the bar does not show.
  const active = visible.includes(tab) ? tab : 'snmp';
  const groupRows = useMemo(() => rows.filter((c) => credentialGroupOf(c.kind) === active), [rows, active]);
  // Held across renders, so the table's columns are rebuilt when the list changes and not on every
  // render of the page.
  const allIds = useMemo(() => rows.map((c) => c.id), [rows]);

  return (
    <div>
      <PageHeader
        title={t('nav:nodes.credentials')}
        trail={[{ label: t('nav:sections.nodes') }, { label: t('nav:nodes.credentials') }]}
      />

      <LoadGate load={creds} permission="manage_credentials">
        <Tabs
          tabs={visible.map((g) => ({
            key: g,
            label: t(CREDENTIAL_GROUP_LABEL_KEYS[g]),
            count: counts[g],
          }))}
          active={active}
          onChange={setTab}
        />
        {/* Keyed by the tab so its filter state and dialogs start fresh on a switch. */}
        <CredentialGroupTable
          key={active}
          group={active}
          rows={groupRows}
          allIds={allIds}
          loading={creds.loading}
          onChanged={load}
        />
      </LoadGate>
    </div>
  );
}

/** One tab's list: its toolbar, its "+ Add", its table and the dialogs opened from its rows. */
function CredentialGroupTable({
  group,
  rows,
  allIds,
  loading,
  onChanged,
}: {
  group: CredentialGroup;
  /** This tab's rows, in the try order the server listed them in. */
  rows: CredentialSummary[];
  /** Every credential's id in that order — what a saved order must name (ADR-205 Inc.2). */
  allIds: string[];
  loading: boolean;
  onChanged: () => void;
}) {
  const { t } = useTranslation('access');
  const canCredentials = useCan('manage_credentials');
  const canReveal = useCan('reveal_credentials');
  // In the URL (ADR-153), so a reload keeps the order. One sort for the route: the tabs share it.
  const [sort, setSort] = useSortParams(CREDENTIAL_SORT_KEYS, DEFAULT_CREDENTIAL_SORT);
  const applied = credentialSortIn(group, sort);
  const [adding, setAdding] = useState(false);
  const [editing, setEditing] = useState<CredentialSummary | null>(null);
  const [deleting, setDeleting] = useState<CredentialSummary | null>(null);
  const [revealing, setRevealing] = useState<CredentialSummary | null>(null);
  const closeReveal = useCallback(() => setRevealing(null), []);
  const creatable = creatableKindsIn(group);
  const addTitleKey = CREDENTIAL_ADD_TITLE_KEYS[group];
  const [moving, setMoving] = useState(false);
  const [moveError, setMoveError] = useState<string | null>(null);
  // A row's place in this tab's try order, 1-based, as the server listed it.
  const place = useMemo(() => new Map(rows.map((c, i) => [c.id, i + 1])), [rows]);

  // Whether this tab's rows can be moved. Whether the list is narrowed is only known once the
  // columns exist (`useClientFilters` takes them), so it is mirrored into state by an effect after
  // that call; the arrows follow a filter one render late, which nothing can click in between.
  const [filteredNow, setFilteredNow] = useState(false);
  const reorderable = canCredentials && canReorderCredentials(group, sort, filteredNow);
  // The order just saved, until the re-read that shows it arrives. The arrows are pressable again
  // as soon as the save returns, and a second press in that gap must move on from the saved order —
  // computed from the list on screen, it repeated the first move and the second press was lost.
  const saved = useRef<string[] | null>(null);
  useEffect(() => {
    saved.current = null;
  }, [allIds]);
  const move = useCallback(
    (id: string, delta: -1 | 1) => {
      setMoving(true);
      setMoveError(null);
      const next = moveWithinGroup(saved.current ?? allIds, rows.map((c) => c.id), id, delta);
      api
        .setCredentialOrder(next)
        .then(() => {
          saved.current = next;
        })
        .catch((e: unknown) => {
          saved.current = null;
          setMoveError(errMsg(e, t('cred.err.order')));
        })
        // Re-read either way: on a 409 the list the arrows were drawn from is the stale one.
        .finally(() => {
          setMoving(false);
          onChanged();
        });
    },
    [allIds, rows, t, onChanged],
  );

  const columns = useMemo<Column<CredentialSummary>[]>(() => {
    // Every kind present in this tab, so a kind the old dropdown could not select is filterable.
    const kinds = [...new Set(rows.map((c) => c.kind))].sort();
    const specs = credentialFilters(t, kinds, (k) => kindLabel(k, t));
    const cols: Column<CredentialSummary>[] = [
      ...(group === 'snmp'
        ? [
            {
              key: 'order',
              header: t('cred.cols.order'),
              width: '120px',
              sortable: true,
              render: (c: CredentialSummary) => {
                const n = place.get(c.id) ?? 0;
                return (
                  <span className="cred-order">
                    <span className="cred-order-n mono">{n}</span>
                    {reorderable && (
                      <>
                        <button
                          type="button"
                          className="cred-order-btn"
                          aria-label={t('cred.order.up', { name: c.name })}
                          disabled={moving}
                          style={n === 1 ? { visibility: 'hidden' } : undefined}
                          onClick={() => move(c.id, -1)}
                        >
                          ↑
                        </button>
                        <button
                          type="button"
                          className="cred-order-btn"
                          aria-label={t('cred.order.down', { name: c.name })}
                          disabled={moving}
                          style={n === rows.length ? { visibility: 'hidden' } : undefined}
                          onClick={() => move(c.id, 1)}
                        >
                          ↓
                        </button>
                      </>
                    )}
                  </span>
                );
              },
            },
          ]
        : []),
      {
        key: 'name',
        header: t('cred.cols.name'),
        width: '1.7fr',
        sortable: true,
        render: (c) => {
          const Icon = KIND_ICONS[c.kind] ?? KeyIcon;
          return (
            <span className="yt-name">
              <span className="yt-typeicon" title={kindLabel(c.kind, t)}>
                <Icon />
              </span>
              <span className="yt-name-txt">{c.name}</span>
            </span>
          );
        },
      },
      {
        key: 'type',
        header: t('cred.cols.type'),
        // Sized for the longest label ("SNMP v3 (user/password)"); 150px cut every one of them
        // with no mark, which only showed once the browser walk was given real kinds.
        width: '200px',
        render: (c) => {
          const Icon = KIND_ICONS[c.kind] ?? KeyIcon;
          return (
            <span className="yt-chip cred-kind-chip">
              <Icon />
              <ClippedText text={kindLabel(c.kind, t)} />
            </span>
          );
        },
      },
      {
        key: 'secret',
        header: t('cred.cols.secret'),
        width: '130px',
        render: () => <SealedSecret />,
      },
      {
        key: 'used_by',
        header: t('cred.cols.usedBy'),
        // Sized for an integration's phrase ("3 Meraki organizations"), which is about twice the
        // "184 nodes" the old 110px was cut for. The `title` carries the whole label for a row
        // that still does not fit.
        width: '180px',
        sortable: true,
        render: (c) => {
          const label = usageLabel(c, t);
          return (
            <span className={totalUsage(c) === 0 ? 'yt-usage zero' : 'yt-usage'} title={label}>
              {label}
            </span>
          );
        },
      },
      { key: 'id', header: t('cred.cols.credentialId'), width: '1fr', render: (c) => <CopyableId id={c.id} /> },
      {
        key: 'actions',
        header: t('cred.cols.actions'),
        // Show secret, Edit and Delete on a device row; the integration tab's rows draw two.
        width: rowActionsWidth(3),
        align: 'right',
        render: (c) => {
          // Each item is gated on its own permission, and the menu is drawn while one survives.
          const reveal = canReveal && isRevealableCredentialKind(c.kind);
          if (!canCredentials && !reveal) return null;
          return (
            <span className="ytable-actions">
              <OverflowMenu
                actions={[
                  ...(reveal
                    ? [
                        {
                          label: t('cred.reveal.action'),
                          icon: <LockIcon />,
                          onClick: () => setRevealing(c),
                        },
                      ]
                    : []),
                  ...(canCredentials
                    ? [
                        {
                          label: t('common:actions.edit'),
                          icon: <EditIcon />,
                          onClick: () => setEditing(c),
                        },
                        {
                          label: t('common:actions.delete'),
                          icon: <TrashIcon />,
                          danger: true,
                          onClick: () => setDeleting(c),
                        },
                      ]
                    : []),
                ]}
              />
            </span>
          );
        },
      },
    ];
    for (const c of cols) c.filter = specs[c.key];
    return cols;
  }, [t, canCredentials, canReveal, rows, group, place, reorderable, moving, move]);

  // URL-backed, one table per tab, each under its own key prefix (ADR-153 decision 3).
  const filtering = useClientFilters(columns, rows, { prefix: CREDENTIAL_TABLE_PREFIX[group] });
  const { filters, setFilters, shown: matched, counts, anyFiltered } = filtering;
  useEffect(() => setFilteredNow(anyFiltered), [anyFiltered]);
  // Sorting stays with the caller — `DataTable` draws the arrow and reports the click but never
  // reorders `rows`, so a keyset-paged screen cannot accidentally sort a prefix (`lib/tableSort.ts`).
  const shown = useMemo(
    () => sortRows(matched, applied, credentialSortValues((id) => place.get(id) ?? 0)),
    [matched, applied, place],
  );

  const empty = anyFiltered ? (
    t('cred.empty.filtered')
  ) : group === 'integration' ? (
    // These keys are created where the integration is set up, so the next step is a link there.
    <EmptyState text={t('cred.empty.keys')} action={<ScreenLink to="/settings/integrations" />} />
  ) : (
    <EmptyState
      text={t('cred.empty.none')}
      action={
        canCredentials &&
        addTitleKey &&
        creatable.length > 0 && (
          <Button variant="primary" onClick={() => setAdding(true)}>
            + {t(addTitleKey)}
          </Button>
        )
      }
    />
  );
  // One prop set for the four tags below. Each tag spells its own `tableId`: an id is a name the
  // `tableIds.test.ts` check reads off the tag, so it is never passed through a variable.
  const table = {
    rows: shown,
    columns,
    rowKey: (c: CredentialSummary) => c.id,
    sort: applied,
    onSortChange: setSort,
    filters,
    onFiltersChange: setFilters,
    filterCounts: counts,
    loading,
    empty,
  };

  return (
    <>
      <ListToolbar
        list={filtering}
        labels={columnLabels(columns)}
        count={{
          shown: shown.length,
          total: rows.length,
          noun: (n) => t('common:noun.credential', { count: n }),
        }}
      >
        {canCredentials && addTitleKey && creatable.length > 0 && (
          <Button variant="primary" onClick={() => setAdding(true)}>
            + {t(addTitleKey)}
          </Button>
        )}
      </ListToolbar>

      {moveError && (
        <p className="form-error" role="alert">
          {moveError}
        </p>
      )}
      {group === 'snmp' && <DataTable tableId="settings.credentials" {...table} />}
      {group === 'http' && <DataTable tableId="settings.credentialsHttp" {...table} />}
      {group === 'cli' && <DataTable tableId="settings.credentialsCli" {...table} />}
      {group === 'integration' && <DataTable tableId="settings.credentialsIntegration" {...table} />}
      {group === 'other' && <DataTable tableId="settings.credentialsOther" {...table} />}

      {adding && addTitleKey && (
        <AddCredentialModal
          kinds={creatable}
          title={t(addTitleKey)}
          onClose={() => setAdding(false)}
          onSaved={onChanged}
        />
      )}
      {editing && (
        <EditCredentialModal cred={editing} onClose={() => setEditing(null)} onSaved={onChanged} />
      )}
      {revealing && <RevealCredentialModal cred={revealing} onClose={closeReveal} />}
      {deleting && (
        <DeleteCredentialModal
          cred={deleting}
          onClose={() => setDeleting(null)}
          onDone={() => {
            setDeleting(null);
            onChanged();
          }}
        />
      )}
    </>
  );
}
