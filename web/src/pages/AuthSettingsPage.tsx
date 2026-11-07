// SPDX-License-Identifier: AGPL-3.0-only
// Authentication (Settings ▸ Auth): configure an external IdP for SSO (OIDC). The client_secret is
// write-only — the API never returns it — and IdP groups map to Yagra roles via the role map.
// ManageUsers-gated. Local accounts (Settings ▸ Users) keep working alongside SSO.

import { useCallback, useEffect, useState } from 'react';
import { Trans, useTranslation } from 'react-i18next';
import { api, errMsg } from '../services/api';
import { useCan } from '../store';
import { useLoad } from '../lib/useLoad';
import { LoadGate } from '../components/ui/LoadGate';
import {
  ROLES,
  type LdapConfigView,
  type LdapSecurity,
  type LdapTestResult,
  type OidcProviderKind,
  type OidcProviderSummary,
  type OidcProviderInput,
  type Role,
} from '../types/api';
import {
  OIDC_PICKER_ORDER,
  effectiveIssuer,
  issuerParamInvalid,
  paramFromIssuer,
  presetOf,
  providerFormReady,
  roleMapToSend,
} from './oidcPresets';
import {
  canTestLdap,
  connectionUrl,
  defaultPortFor,
  emptyLdapForm,
  savingRevokesSessions,
  toLdapForm,
  toLdapInput,
  validateLdapForm,
  type LdapFormState,
} from './ldapConfigForm';
import { addRoleMapRow, toRoleMapRows, type RoleMapRow } from './roleMapForm';
import { redirectUriMismatch } from './tlsSettingsForm';
import { PageHeader } from '../components/ui/PageHeader';
import { Inline, Page, Stack } from '../components/ui/Page';
import { Card } from '../components/ui/Card';
import { Callout, Disclosure, ItemList, ItemRow } from '../components/ui/Section';
import { CheckField, FieldGroup, FieldValue, Fields, SaveNote } from '../components/ui/FormLayout';
import { Button } from '../components/ui/Button';
import { ConfirmDeleteModal } from '../components/ui/ConfirmDeleteModal';
import { Modal } from '../components/ui/Modal';
import { Field, TextArea, TextInput, Select } from '../components/ui/Field';
import { SecretInput } from '../components/ui/SecretInput';
import { secretToSend } from '../components/ui/secretField';
import { StepFrame } from '../components/ui/StepFrame';
import { ScreenLink } from '../components/ui/ScreenLink';
import { EmptyState } from '../components/ui/EmptyState';
import { OverflowMenu } from '../components/ui/OverflowMenu';
import { EditIcon, TrashIcon } from '../components/ui/icons';
import './AuthSettingsPage.css';
import { asRole } from './roleMapForm';
import { done } from '../lib/submitState';
import { useSubmit } from '../lib/useSubmit';
import { FormError, FormFooter } from '../components/ui/FormFooter';


/** One editable IdP-group → role mapping row. */
interface MapRow {
  group: string;
  role: Role;
}

/** Add or edit an OIDC provider. On edit the client_secret is kept unless a new one is typed. */
function ProviderModal({
  provider,
  onClose,
  onSaved,
}: {
  provider: OidcProviderSummary | null;
  onClose: () => void;
  onSaved: () => void;
}) {
  const { t } = useTranslation('settings-auth');
  const editing = provider != null;
  // A new provider starts on the first product in the picker; an existing one reopens as whatever
  // it was saved as, which is the whole reason the kind is stored server-side.
  const initialKind: OidcProviderKind = provider?.kind ?? OIDC_PICKER_ORDER[0];
  const [kind, setKind] = useState<OidcProviderKind>(initialKind);
  const preset = presetOf(kind);
  const [name, setName] = useState(provider?.name ?? '');
  const [issuer, setIssuer] = useState(provider?.issuer ?? '');
  // The one product-specific field the issuer is built from (tenant id / Okta domain).
  const [issuerParam, setIssuerParam] = useState(
    provider ? (paramFromIssuer(initialKind, provider.issuer) ?? '') : '',
  );
  // A stored issuer this product's form cannot represent — an Okta custom authorization server, or
  // a row written through the API. Show the raw URL instead of silently rewriting it.
  const [rawIssuer, setRawIssuer] = useState(
    provider != null &&
      presetOf(initialKind).issuerParam !== null &&
      paramFromIssuer(initialKind, provider.issuer) === null,
  );
  const [clientId, setClientId] = useState(provider?.client_id ?? '');
  // Empty = keep the stored secret (`SecretInput`); a new provider has none to keep.
  const [clientSecret, setClientSecret] = useState('');
  const [redirectUri, setRedirectUri] = useState(
    provider?.redirect_uri ??
      (typeof window !== 'undefined' ? `${window.location.origin}/auth/callback` : ''),
  );
  // Reopening a provider reads back what is stored, never the preset — a value edited around this
  // form must survive being looked at. The preset is applied on create and on switching product.
  const [scopes, setScopes] = useState(provider?.scopes ?? presetOf(initialKind).scopes);
  const [groupsClaim, setGroupsClaim] = useState(
    provider?.groups_claim ?? presetOf(initialKind).groupsClaim,
  );
  const [rows, setRows] = useState<MapRow[]>(
    provider
      ? Object.entries(provider.role_map).map(([group, role]) => ({
          // An unreadable role falls to the least privilege rather than dropping the mapping,
          // which would silently widen the group to `default_role` on the next save.
          group,
          role: asRole(role) ?? 'viewer',
        }))
      : [],
  );
  const [defaultRole, setDefaultRole] = useState<Role | ''>(asRole(provider?.default_role) ?? '');
  const [enabled, setEnabled] = useState(provider?.enabled ?? true);
  const saving = useSubmit({
    errorFallback: t('err.save'),
    onDone: () => {
      onSaved();
      onClose();
    },
  });

  const sentSecret = secretToSend(clientSecret);
  const secretReady = editing || sentSecret !== undefined;
  const sentIssuer = effectiveIssuer(kind, issuerParam, issuer);
  const ready = providerFormReady({
    kind,
    name,
    issuer: sentIssuer,
    clientId,
    redirectUri,
    secretReady,
    defaultRole,
  });

  const setRow = (i: number, patch: Partial<MapRow>) =>
    setRows((rs) => rs.map((r, j) => (j === i ? { ...r, ...patch } : r)));

  /** Switching product re-applies its scopes and claim, and puts the product's own issuer field
   *  back — the operator asked for this product's shape, so stop showing the free-text fallback. */
  const changeKind = (next: OidcProviderKind) => {
    setKind(next);
    setScopes(presetOf(next).scopes);
    setGroupsClaim(presetOf(next).groupsClaim);
    setRawIssuer(false);
  };

  const submit = () => {
    if (!ready) return;
    saving.submit(() => {
      const role_map: Record<string, Role> = {};
      for (const r of rows) {
        const g = r.group.trim();
        if (g) role_map[g] = r.role;
      }
      const body: OidcProviderInput = {
        name: name.trim(),
        kind,
        issuer: sentIssuer,
        client_id: clientId.trim(),
        ...(sentSecret !== undefined ? { client_secret: sentSecret } : {}),
        redirect_uri: redirectUri.trim(),
        scopes: scopes.trim(),
        groups_claim: groupsClaim.trim() || 'groups',
        role_map: roleMapToSend(kind, role_map),
        default_role: defaultRole === '' ? null : defaultRole,
        enabled,
      };
      const call = editing
        ? api.updateOidcProvider(provider.id, body)
        : api.createOidcProvider(body);
      return call.then(() => done());
    });
  };

  return (
    <Modal
      title={editing ? t('edit.title') : t('add.title')}
      onClose={onClose}
      footer={
        <FormFooter
          form={saving}
          onClose={onClose}
          onSubmit={submit}
          submitLabel={t('common:actions.save')}
          canSubmit={ready}
        />
      }
    >
      <Fields>
        <Field label={t('field.product')}>
          <Select value={kind} onChange={(e) => changeKind(e.target.value as OidcProviderKind)}>
            {OIDC_PICKER_ORDER.map((k) => (
              <option key={k} value={k}>
                {t(`idp.${k}`)}
              </option>
            ))}
          </Select>
        </Field>
        {/* What to do in the product's own console, closed until wanted (ADR-200 kind d). It used
            to be a paragraph under the picker, up to 334 characters, read once and then in the way. */}
        <StepFrame summary={t('idpSteps.title')} steps={preset.setupSteps.map((key) => t(key))} />
        <Field label={t('field.name')}>
          <TextInput value={name} onChange={(e) => setName(e.target.value)} autoFocus />
        </Field>
        {/* One issuer, three shapes: built from a single product field, fixed by the product, or
            typed out. The fallback to a raw URL is what keeps reopening a hand-written provider
            from rewriting its issuer. */}
        {preset.fixedIssuer !== null ? (
          <Field label={t('field.issuer')}>
            <TextInput className="mono" value={preset.fixedIssuer} readOnly disabled />
          </Field>
        ) : preset.issuerParam !== null && !rawIssuer ? (
          <Field
            label={t(`field.${preset.issuerParam}`)}
            error={issuerParamInvalid(kind, issuerParam) ? t('field.issuerParamInvalid') : null}
          >
            <TextInput
              id="oidc-issuer-param"
              className="mono"
              placeholder={t(`field.${preset.issuerParam}Placeholder`)}
              value={issuerParam}
              onChange={(e) => setIssuerParam(e.target.value)}
            />
          </Field>
        ) : (
          // Two children, so the field cannot hand its id to "the" control: name it here.
          <Field label={t('field.issuer')} htmlFor="oidc-issuer">
            <TextInput
              id="oidc-issuer"
              className="mono"
              placeholder="https://idp.example.com"
              value={issuer}
              onChange={(e) => setIssuer(e.target.value)}
            />
            {rawIssuer && <span className="form-status">{t('field.issuerUnrecognized')}</span>}
          </Field>
        )}
        <Field label={t('field.clientId')}>
          <TextInput className="mono" value={clientId} onChange={(e) => setClientId(e.target.value)} />
        </Field>
        <Field label={t('field.clientSecret')}>
          <SecretInput
            id="oidc-client-secret"
            stored={editing}
            value={clientSecret}
            onChange={setClientSecret}
          />
        </Field>
        <Field label={t('field.redirectUri')}>
          <TextInput
            className="mono"
            value={redirectUri}
            onChange={(e) => setRedirectUri(e.target.value)}
          />
        </Field>
        {/* For a product these two are decided by the product, but they are still shown rather
            than hidden: what gets requested at the IdP is the thing an operator has to reason about
            when a sign-in is refused. The free-text option keeps them editable, as this form always
            was. */}
        {kind === 'generic' ? (
          <>
            <Field label={t('field.scopes')}>
              <TextInput className="mono" value={scopes} onChange={(e) => setScopes(e.target.value)} />
            </Field>
            {/* Empty sends `groups` (see `submit`), so the placeholder is the real default. */}
            <Field label={t('field.groupsClaim')}>
              <TextInput
                className="mono"
                placeholder="groups"
                value={groupsClaim}
                onChange={(e) => setGroupsClaim(e.target.value)}
              />
            </Field>
          </>
        ) : (
          <Disclosure summary={t('field.requested')}>
            <Fields>
              <Field label={t('field.scopes')}>
                <TextInput className="mono" value={scopes} readOnly disabled />
              </Field>
              {preset.supportsGroups && (
                <Field label={t('field.groupsClaim')}>
                  <TextInput className="mono" value={groupsClaim} readOnly disabled />
                </Field>
              )}
            </Fields>
          </Disclosure>
        )}

        {/* A product that does not put groups in the ID token has no working map — offering one
            would let an operator write rules that quietly never match. The default role below
            becomes required instead, and says why. */}
        {preset.supportsGroups && (
          <Field label={t('field.roleMap.label')} infoKey="settings-auth:field.roleMap.info">
            <Stack gap={8}>
              {rows.map((r, i) => (
                <div className="auth-rolemap-row" key={i}>
                  <TextInput
                    className="mono"
                    placeholder={
                      kind === 'entra'
                        ? t('field.groupPlaceholderEntra')
                        : t('field.groupPlaceholder')
                    }
                    value={r.group}
                    onChange={(e) => setRow(i, { group: e.target.value })}
                  />
                  <Select
                    value={r.role}
                    onChange={(e) => setRow(i, { role: e.target.value as Role })}
                  >
                    {ROLES.map((role) => (
                      <option key={role} value={role}>
                        {t(`common:role.${role}`)}
                      </option>
                    ))}
                  </Select>
                  <Button
                    variant="outline"
                    onClick={() => setRows((rs) => rs.filter((_, j) => j !== i))}
                  >
                    {t('common:actions.remove')}
                  </Button>
                </div>
              ))}
              <Button
                variant="outline"
                onClick={() => setRows((rs) => [...rs, { group: '', role: 'viewer' }])}
              >
                + {t('field.addMapping')}
              </Button>
            </Stack>
          </Field>
        )}

        <Field label={t('field.defaultRole')} htmlFor="oidc-default-role">
          <Select
            id="oidc-default-role"
            value={defaultRole}
            onChange={(e) => setDefaultRole(e.target.value as Role | '')}
          >
            <option value="">{t('field.defaultRoleNone')}</option>
            {ROLES.map((role) => (
              <option key={role} value={role}>
                {t(`common:role.${role}`)}
              </option>
            ))}
          </Select>
          {!preset.supportsGroups && (
            <span className="form-status">{t('field.defaultRoleRequired')}</span>
          )}
        </Field>

        <CheckField checked={enabled} onChange={setEnabled} label={t('field.enabled')} />

        <FormError form={saving} />
      </Fields>
    </Modal>
  );
}

/** Confirm + delete a provider. */
function DeleteProviderModal({
  provider,
  onClose,
  onDone,
}: {
  provider: OidcProviderSummary;
  onClose: () => void;
  onDone: () => void;
}) {
  const { t } = useTranslation('settings-auth');
  return (
    <ConfirmDeleteModal
      title={t('delete.title')}
      onConfirm={() => api.deleteOidcProvider(provider.id)}
      errorFallback={t('err.delete')}
      onClose={onClose}
      onDone={onDone}
    >
      {t('delete.confirm', { name: provider.name })}
    </ConfirmDeleteModal>
  );
}

/** Settings ▸ Auth ▸ Directory (LDAP/AD) — ADR-041.
 *
 *  One saved configuration, so this is a form rather than a list. The Test button exercises what is
 *  **stored**, which is why it waits for a save — the first one, and every edit after it
 *  (`canTestLdap`): validating a directory before switching it on is the whole point of it, and a
 *  result taken while the screen holds an unsaved edit would describe something else. The result is rendered stage by stage rather than as a tick, because the check
 *  deliberately never binds as the user — an `ok` alone would be read as "login works". */
function DirectoryCard({ canUsers }: { canUsers: boolean }) {
  const { t } = useTranslation('settings-auth');
  const [stored, setStored] = useState<LdapConfigView | null>(null);
  const [form, setForm] = useState<LdapFormState>(emptyLdapForm());
  const [rows, setRows] = useState<RoleMapRow[]>([]);
  const [busy, setBusy] = useState(false);
  const [saved, setSaved] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [testing, setTesting] = useState(false);
  const [probeUser, setProbeUser] = useState('');
  const [result, setResult] = useState<LdapTestResult | null>(null);
  // Bumped on every load, so the password field forgets a "Replace" that a save has answered.
  const [loadGen, setLoadGen] = useState(0);

  // A refusal is not drawn here: the page's own unavailable notice already covers 401/403. Any
  // other failure is, or the card would show an empty form as if no directory were configured.
  const {
    data: answer,
    error: loadError,
    loading,
    reload: load,
  } = useLoad(() => api.getLdapConfig(), [], {
    initial: null as { config?: LdapConfigView | null } | null,
    errorFallback: t('settings-ai:loadFailed'),
  });

  // Each answer replaces the form: `data` keeps its identity until a new one arrives.
  useEffect(() => {
    if (!answer) return;
    setStored(answer.config ?? null);
    setLoadGen((g) => g + 1);
    if (answer.config) {
      setForm(toLdapForm(answer.config));
      setRows(toRoleMapRows(answer.config.role_map));
    }
  }, [answer]);

  // Any edit invalidates a stale success banner and a stale probe result — the latter matters,
  // because the probe describes the *saved* configuration and would otherwise appear to describe
  // whatever is on screen now.
  const dirty = () => {
    setSaved(false);
    setResult(null);
  };
  const set = (patch: Partial<LdapFormState>) => {
    setForm((f) => ({ ...f, ...patch }));
    dirty();
  };
  const setRow = (i: number, patch: Partial<RoleMapRow>) => {
    setRows((rs) => rs.map((r, j) => (j === i ? { ...r, ...patch } : r)));
    dirty();
  };

  const save = async () => {
    const problem = validateLdapForm(form, rows, stored);
    if (problem) {
      setError(t(`ldap.err.${problem}`));
      return;
    }
    setBusy(true);
    setError(null);
    try {
      await api.saveLdapConfig(toLdapInput(form, rows));
      setSaved(true);
      // Reload rather than trusting the local state: this clears the password field back to
      // "stored", and makes the form match what Test will exercise.
      load();
    } catch (e: unknown) {
      setError(errMsg(e, t('ldap.err.save')));
    } finally {
      setBusy(false);
    }
  };

  const test = async () => {
    setTesting(true);
    setResult(null);
    try {
      setResult(await api.testLdapConfig(probeUser.trim() || undefined));
    } catch (e: unknown) {
      setError(errMsg(e, t('ldap.err.test')));
    } finally {
      setTesting(false);
    }
  };

  if (loading) return null;

  return (
    <Card
      title={t('ldap.title')}
      footer={
        <>
          {(error ?? loadError) && <SaveNote tone="error">{error ?? loadError}</SaveNote>}
          {saved && <SaveNote tone="ok">{t('ldap.saved')}</SaveNote>}
          <Button variant="primary" onClick={() => void save()} disabled={!canUsers || busy}>
            {t('common:actions.save')}
          </Button>
          <TextInput
            className="mono"
            value={probeUser}
            placeholder={t('ldap.test.usernamePlaceholder')}
            onChange={(e) => setProbeUser(e.target.value)}
          />
          <Button
            variant="outline"
            onClick={() => void test()}
            disabled={!canTestLdap(stored, form, rows) || testing || busy}
          >
            {t('ldap.test.run')}
          </Button>
        </>
      }
    >
      {/* Label beside control (`Fields layout="columns"`): a dozen short values - DNs, attribute
          names - stacked one above the other would turn one card into a page of scrolling. */}
      <Fields layout="columns">
        <Field label={t('ldap.field.host')}>
          <TextInput
            className="mono"
            value={form.host}
            onChange={(e) => set({ host: e.target.value })}
            placeholder="dc1.corp.example.com"
          />
        </Field>
        <Field label={t('ldap.field.security')}>
          <Select
            value={form.security}
            onChange={(e) => {
              const security = e.target.value as LdapSecurity;
              // Follow the conventional port unless the operator has moved off it, so switching mode
              // does not silently leave 636 on a StartTLS connection.
              const wasDefault = form.port.trim() === String(defaultPortFor(form.security));
              set({
                security,
                ...(wasDefault ? { port: String(defaultPortFor(security)) } : {}),
              });
            }}
          >
            <option value="ldaps">{t('ldap.security.ldaps')}</option>
            <option value="starttls">{t('ldap.security.starttls')}</option>
          </Select>
        </Field>
        <Field label={t('ldap.field.port')}>
          <TextInput
            className="mono"
            value={form.port}
            onChange={(e) => set({ port: e.target.value })}
          />
        </Field>
        <Field label={t('ldap.field.url')}>
          <FieldValue>
            <span className="mono">{connectionUrl(form)}</span>
          </FieldValue>
        </Field>
        <Field label={t('ldap.field.caCert')}>
          <TextArea
            rows={4}
            value={form.caCert}
            onChange={(e) => set({ caCert: e.target.value })}
            placeholder="-----BEGIN CERTIFICATE-----"
          />
        </Field>
        <Field label={t('ldap.field.bindDn')}>
          <TextInput
            className="mono"
            value={form.bindDn}
            onChange={(e) => set({ bindDn: e.target.value })}
          />
        </Field>
        <Field label={t('ldap.field.bindPassword')}>
          <SecretInput
            key={loadGen}
            id="ldap-bind-password"
            stored={stored?.has_bind_password === true}
            value={form.bindPassword}
            onChange={(bindPassword) => set({ bindPassword })}
          />
        </Field>
        <Field label={t('ldap.field.userBaseDn')}>
          <TextInput
            className="mono"
            value={form.userBaseDn}
            onChange={(e) => set({ userBaseDn: e.target.value })}
          />
        </Field>
        <Field label={t('ldap.field.userFilter')}>
          <TextInput
            className="mono"
            value={form.userFilter}
            onChange={(e) => set({ userFilter: e.target.value })}
          />
        </Field>
        <Field label={t('ldap.field.usernameAttribute')}>
          <TextInput
            className="mono"
            value={form.usernameAttribute}
            onChange={(e) => set({ usernameAttribute: e.target.value })}
          />
        </Field>
        <Field label={t('ldap.field.uidAttribute')}>
          <TextInput
            className="mono"
            value={form.uidAttribute}
            onChange={(e) => set({ uidAttribute: e.target.value })}
          />
        </Field>
        <Field label={t('ldap.field.memberOfAttribute')}>
          <TextInput
            className="mono"
            value={form.memberOfAttribute}
            onChange={(e) => set({ memberOfAttribute: e.target.value })}
          />
        </Field>
        <Field label={t('ldap.field.groupBaseDn')}>
          <TextInput
            className="mono"
            value={form.groupBaseDn}
            onChange={(e) => set({ groupBaseDn: e.target.value })}
          />
        </Field>
        <Field label={t('ldap.field.groupFilter')} infoKey="settings-auth:ldap.groupSearch.info">
          <TextInput
            className="mono"
            value={form.groupFilter}
            onChange={(e) => set({ groupFilter: e.target.value })}
          />
        </Field>
        <FieldGroup label={t('field.roleMap.label')} infoKey="settings-auth:ldap.roleMap.info">
          <Stack gap={8}>
            {rows.map((row, i) => (
              <div className="auth-rolemap-row" key={row.key}>
                <TextInput
                  className="mono"
                  value={row.group}
                  placeholder="CN=NetOps,OU=Groups,DC=corp,DC=example,DC=com"
                  onChange={(e) => setRow(i, { group: e.target.value })}
                />
                <Select
                  value={row.role}
                  onChange={(e) => setRow(i, { role: e.target.value as Role })}
                >
                  {ROLES.map((r) => (
                    <option key={r} value={r}>
                      {t(`common:role.${r}`)}
                    </option>
                  ))}
                </Select>
                <Button
                  variant="outline"
                  onClick={() => {
                    setRows((rs) => rs.filter((_, j) => j !== i));
                    dirty();
                  }}
                >
                  {t('common:actions.remove')}
                </Button>
              </div>
            ))}
            <Button
              variant="outline"
              onClick={() => {
                setRows(addRoleMapRow(rows));
                dirty();
              }}
            >
              + {t('field.addMapping')}
            </Button>
          </Stack>
        </FieldGroup>
        <Field label={t('field.defaultRole')}>
          <Select
            value={form.defaultRole}
            onChange={(e) => set({ defaultRole: e.target.value as Role | '' })}
          >
            <option value="">{t('field.defaultRoleNone')}</option>
            {ROLES.map((r) => (
              <option key={r} value={r}>
                {t(`common:role.${r}`)}
              </option>
            ))}
          </Select>
        </Field>
        <FieldGroup label={t('field.enabled')}>
          <CheckField
            checked={form.enabled}
            onChange={(enabled) => set({ enabled })}
            label={t('ldap.field.signInAllowed')}
          />
        </FieldGroup>
      </Fields>
      {/* Said when the choice is made, not under the box all the time (ADR-200 kind 3): switching
          a live directory off revokes sessions, and nothing else on this card shows that. */}
      {savingRevokesSessions(stored, form) && (
        <Callout tone="warning">{t('ldap.revokeWarning')}</Callout>
      )}

      {result && (
        <div className="auth-test">
          <ul className="auth-stages">
            {result.stages.map((s) => (
              <li key={s.name} className={s.ok ? 'ok' : 'bad'}>
                {t(`ldap.stage.${s.name}`, s.name)}
                {s.detail && <span className="mono muted"> — {s.detail}</span>}
              </li>
            ))}
          </ul>
          {result.user_dn && (
            <p className="mono muted">
              {t('ldap.test.dn')}: {result.user_dn}
            </p>
          )}
          {result.username_resolved && (
            <p className="mono muted">
              {t('ldap.test.username')}: {result.username_resolved}
            </p>
          )}
          {result.groups.length > 0 && (
            <p className="mono muted">
              {t('ldap.test.groups')}: {result.groups.join(', ')}
              {result.groups_truncated ? ' …' : ''}
            </p>
          )}
          {/* The loudest thing on the panel: "connected fine, and this person would be refused" is
              the commonest misconfiguration, and the login form reports it as a wrong password. */}
          <p className={result.role ? 'auth-saved' : 'form-error'}>
            {result.role
              ? t('ldap.test.role', { role: t(`common:role.${result.role}`) })
              : t('ldap.test.denied')}
          </p>
          <p className="form-status">{result.note}</p>
        </div>
      )}
    </Card>
  );
}

/** Serve one board to people with no account (ADR-123).
 *
 *  Here rather than under System settings because this is a question about *signing in* — whether
 *  it is required at all — and that shelf is where the answers to that question live (ADR-055 R8).
 *
 *  `manage_system`, not the `manage_users` its neighbours take: removing authentication changes the
 *  deployment rather than who may sign in to it (ADR-057). And never `manage_config`, which
 *  Operator holds — that is the mistake `PUT /api/v1/config` would have been.
 */
function PublicDashboardCard() {
  const { t } = useTranslation('settings-auth');
  const canSystem = useCan('manage_system');
  const [state, setState] = useState<{ enabled: boolean; routes: number } | null>(null);
  const [confirm, setConfirm] = useState<null | boolean>(null);
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState<string | null>(null);

  const load = useCallback(() => {
    api
      .getPublicDashboardSwitch()
      .then((s) => setState({ enabled: s.enabled, routes: s.route_count }))
      .catch((e: unknown) => setErr(errMsg(e, t('publicDashboard.loadFailed'))));
  }, [t]);

  useEffect(() => load(), [load]);

  const apply = (next: boolean) => {
    setBusy(true);
    setErr(null);
    api
      .setPublicDashboardEnabled(next)
      .then((s) => setState({ enabled: s.enabled, routes: s.route_count }))
      .catch((e: unknown) => setErr(errMsg(e, t('publicDashboard.saveFailed'))))
      .finally(() => {
        setBusy(false);
        setConfirm(null);
      });
  };

  return (
    <Card
      title={t('publicDashboard.title')}
      footer={
        // ADR-056: the control is drawn only for someone who may use it — never disabled with a
        // tooltip, which is invisible on touch and reads as broken rather than as forbidden.
        canSystem &&
        state != null && (
          <Button
            variant={state.enabled ? 'danger' : 'primary'}
            onClick={() => setConfirm(!state.enabled)}
            disabled={busy}
          >
            {state.enabled ? t('publicDashboard.turnOff') : t('publicDashboard.turnOn')}
          </Button>
        )
      }
    >
      {err && (
        <p className="form-error" role="alert">
          {err}
        </p>
      )}
      {state == null ? (
        <p className="muted">{t('common:loading')}</p>
      ) : (
        <>
          <p className={state.enabled ? 'form-status is-live' : 'form-status'}>
            {state.enabled
              ? t('publicDashboard.stateOn', { count: state.routes })
              : t('publicDashboard.stateOff')}
          </p>
          <p className="auth-compose">
            <Trans
              t={t}
              i18nKey="publicDashboard.compose"
              components={{ lnk: <ScreenLink to="/dashboard/public" /> }}
            />
          </p>
        </>
      )}

      {confirm !== null && state != null && (
        <Modal
          title={confirm ? t('publicDashboard.confirmOnTitle') : t('publicDashboard.confirmOffTitle')}
          onClose={() => setConfirm(null)}
          footer={
            <FormFooter
              form={{ busy, settled: false }}
              onClose={() => setConfirm(null)}
              onSubmit={() => apply(confirm)}
              submitLabel={confirm ? t('publicDashboard.turnOn') : t('publicDashboard.turnOff')}
              variant={confirm ? 'primary' : 'danger'}
            />
          }
        >
          <p className="modal-confirm-text">
            {confirm
              ? t('publicDashboard.confirmOnBody', { count: state.routes })
              : t('publicDashboard.confirmOffBody')}
          </p>
          {/* The cost, stated before the click rather than discovered after it. */}
          {confirm && state.routes === 0 && (
            <Callout tone="warning">
              <Trans
                t={t}
                i18nKey="publicDashboard.confirmOnEmpty"
                components={{ lnk: <ScreenLink to="/dashboard/public" /> }}
              />
            </Callout>
          )}
        </Modal>
      )}
    </Card>
  );
}

export function AuthSettingsPage() {
  const { t } = useTranslation('settings-auth');
  const canUsers = useCan('manage_users');
  const [adding, setAdding] = useState(false);
  const [editing, setEditing] = useState<OidcProviderSummary | null>(null);
  const [deleting, setDeleting] = useState<OidcProviderSummary | null>(null);

  const providers = useLoad(() => api.listOidcProviders(), [], {
    initial: [] as OidcProviderSummary[],
  });
  const { data: rows, loading, reload: load } = providers;

  return (
    <Page layout="sections">
      <PageHeader
        title={t('nav:settings.auth')}
        trail={[{ label: t('nav:sections.settings') }, { label: t('nav:settings.auth') }]}
      />

      {/* ADR-044 moved the WebUI to HTTPS on a new port, and a stored redirect URI is an absolute
          URL that has to agree with what is registered at the IdP. A stale one fails at the token
          exchange, which reads as "SSO is broken" with nothing pointing at the upgrade. Yagra will
          not rewrite it — changing where an IdP may send an authorization code is not something an
          upgrade should do on somebody's behalf — so it says so instead. */}
      {rows.some((r) => redirectUriMismatch(window.location.origin, r.redirect_uri)) && (
        <Callout tone="warning">
          <Stack gap={8}>
            <div>{t('redirectUriMismatch')}</div>
            <StepFrame
              summary={t('redirectFix.title')}
              steps={[
                <Trans
                  key="s1"
                  t={t}
                  i18nKey="redirectFix.s1"
                  values={{ uri: `${window.location.origin}/auth/callback` }}
                  components={{ c: <span className="mono" /> }}
                />,
                t('redirectFix.s2'),
              ]}
            />
          </Stack>
        </Callout>
      )}

      <LoadGate load={providers} permission="manage_users">
        <Card
          actions={
            canUsers && (
              <Button variant="primary" onClick={() => setAdding(true)}>
                + {t('add.title')}
              </Button>
            )
          }
        >
          {rows.length > 0 ? (
            <ItemList>
              {rows.map((p) => (
                <ItemRow
                  key={p.id}
                  title={
                    <Inline gap={8}>
                      {p.name}
                      <span className="auth-badge product">{t(`idp.${p.kind}`)}</span>
                      <span className={p.enabled ? 'auth-badge on' : 'auth-badge off'}>
                        {p.enabled ? t('badge.enabled') : t('badge.disabled')}
                      </span>
                    </Inline>
                  }
                  detail={
                    <Stack gap={2}>
                      <span className="mono">{p.issuer}</span>
                      <span>{t('mappedGroups', { count: Object.keys(p.role_map).length })}</span>
                    </Stack>
                  }
                  actions={
                    canUsers && (
                      <OverflowMenu
                        actions={[
                          {
                            label: t('common:actions.edit'),
                            icon: <EditIcon />,
                            onClick: () => setEditing(p),
                          },
                          {
                            label: t('common:actions.delete'),
                            icon: <TrashIcon />,
                            danger: true,
                            onClick: () => setDeleting(p),
                          },
                        ]}
                      />
                    )
                  }
                />
              ))}
            </ItemList>
          ) : loading ? (
            <p className="muted">{t('common:loading')}</p>
          ) : (
            // Where "local accounts keep working" is said now: the state in which an operator
            // wonders about it, rather than the page note on every visit (ADR-200).
            <EmptyState
              text={t('empty')}
              action={
                canUsers && (
                  <Button type="button" variant="primary" onClick={() => setAdding(true)}>
                    + {t('add.title')}
                  </Button>
                )
              }
            />
          )}
        </Card>

        {/* The directory lives on this page rather than one of its own (ADR-041). "Who may sign
            in" is one subject with two sources, and a separate *Directory* nav item would be the
            second settings screen for one concept that decision 2 exists to prevent. */}
        <DirectoryCard canUsers={canUsers} />
        <PublicDashboardCard />
      </LoadGate>

      {adding && <ProviderModal provider={null} onClose={() => setAdding(false)} onSaved={load} />}
      {editing && (
        <ProviderModal provider={editing} onClose={() => setEditing(null)} onSaved={load} />
      )}
      {deleting && (
        <DeleteProviderModal
          provider={deleting}
          onClose={() => setDeleting(null)}
          onDone={() => {
            setDeleting(null);
            load();
          }}
        />
      )}
    </Page>
  );
}
