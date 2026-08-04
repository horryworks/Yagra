// SPDX-License-Identifier: AGPL-3.0-only
// Authentication (Settings ▸ Auth): configure an external IdP for SSO (OIDC). The client_secret is
// write-only — the API never returns it — and IdP groups map to Yagra roles via the role map.
// ManageUsers-gated. Local accounts (Settings ▸ Users) keep working alongside SSO.

import { useCallback, useEffect, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { api, errMsg, ApiError } from '../services/api';
import { useAuthStore } from '../store';
import {
  ROLES,
  type LdapConfigView,
  type LdapSecurity,
  type LdapTestResult,
  type OidcProviderSummary,
  type OidcProviderInput,
  type Role,
} from '../types/api';
import {
  connectionUrl,
  defaultPortFor,
  emptyLdapForm,
  passwordIsEditable,
  toLdapForm,
  toLdapInput,
  validateLdapForm,
  type LdapFormState,
} from './ldapConfigForm';
import { addRoleMapRow, toRoleMapRows, type RoleMapRow } from './roleMapForm';
import { redirectUriMismatch } from './tlsSettingsForm';
import { PageHeader } from '../components/ui/PageHeader';
import { Card } from '../components/ui/Card';
import { Button } from '../components/ui/Button';
import { ConfirmDeleteModal } from '../components/ui/ConfirmDeleteModal';
import { Modal } from '../components/ui/Modal';
import { TextInput, Select } from '../components/ui/Field';
import { OverflowMenu } from '../components/ui/OverflowMenu';
import { EditIcon, TrashIcon } from '../components/ui/icons';
import './AuthSettingsPage.css';


/** One editable IdP-group → role mapping row. */
interface MapRow {
  group: string;
  role: Role;
}

/** Narrow a role as the API reports it: Rust types `role_map` values and `default_role` as bare
 *  `String`s, so the contract cannot promise the union the pickers below are keyed by (the write
 *  path does reject an unknown role, so this only ever fires on data written around the API). */
const asRole = (value: string | null | undefined): Role | null =>
  ROLES.find((r) => r === value) ?? null;

/** Add or edit an OIDC provider. On edit the client_secret is left intact unless "replace" is set. */
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
  const [name, setName] = useState(provider?.name ?? '');
  const [issuer, setIssuer] = useState(provider?.issuer ?? '');
  const [clientId, setClientId] = useState(provider?.client_id ?? '');
  const [replaceSecret, setReplaceSecret] = useState(!editing);
  const [clientSecret, setClientSecret] = useState('');
  const [redirectUri, setRedirectUri] = useState(
    provider?.redirect_uri ??
      (typeof window !== 'undefined' ? `${window.location.origin}/auth/callback` : ''),
  );
  const [scopes, setScopes] = useState(provider?.scopes ?? 'openid profile email groups');
  const [groupsClaim, setGroupsClaim] = useState(provider?.groups_claim ?? 'groups');
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
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const secretReady = !replaceSecret ? true : clientSecret !== '';
  const ready =
    name.trim() !== '' &&
    issuer.trim() !== '' &&
    clientId.trim() !== '' &&
    redirectUri.trim() !== '' &&
    secretReady;

  const setRow = (i: number, patch: Partial<MapRow>) =>
    setRows((rs) => rs.map((r, j) => (j === i ? { ...r, ...patch } : r)));

  const submit = () => {
    if (!ready) return;
    setBusy(true);
    setError(null);
    const role_map: Record<string, Role> = {};
    for (const r of rows) {
      const g = r.group.trim();
      if (g) role_map[g] = r.role;
    }
    const body: OidcProviderInput = {
      name: name.trim(),
      issuer: issuer.trim(),
      client_id: clientId.trim(),
      ...(replaceSecret ? { client_secret: clientSecret } : {}),
      redirect_uri: redirectUri.trim(),
      scopes: scopes.trim(),
      groups_claim: groupsClaim.trim() || 'groups',
      role_map,
      default_role: defaultRole === '' ? null : defaultRole,
      enabled,
    };
    const call = editing
      ? api.updateOidcProvider(provider.id, body)
      : api.createOidcProvider(body).then(() => undefined);
    call
      .then(() => {
        onSaved();
        onClose();
      })
      .catch((e: unknown) => {
        setError(errMsg(e, t('err.save')));
        setBusy(false);
      });
  };

  return (
    <Modal
      title={editing ? t('edit.title') : t('add.title')}
      onClose={onClose}
      footer={
        <>
          <Button variant="outline" onClick={onClose} disabled={busy}>
            {t('common:actions.cancel')}
          </Button>
          <Button variant="primary" onClick={submit} disabled={!ready || busy}>
            {t('common:actions.save')}
          </Button>
        </>
      }
    >
      <div className="modal-field">
        <label className="modal-field-label">{t('field.name')}</label>
        <TextInput value={name} onChange={(e) => setName(e.target.value)} autoFocus />
      </div>
      <div className="modal-field">
        <label className="modal-field-label">{t('field.issuer')}</label>
        <TextInput
          className="mono"
          placeholder="https://idp.example.com"
          value={issuer}
          onChange={(e) => setIssuer(e.target.value)}
        />
        <span className="modal-hint">{t('field.issuerHint')}</span>
      </div>
      <div className="modal-field">
        <label className="modal-field-label">{t('field.clientId')}</label>
        <TextInput
          className="mono"
          value={clientId}
          onChange={(e) => setClientId(e.target.value)}
        />
      </div>
      {editing && (
        <label className="auth-replace">
          <input
            type="checkbox"
            checked={replaceSecret}
            onChange={(e) => setReplaceSecret(e.target.checked)}
          />
          <span>{t('field.replaceSecret')}</span>
        </label>
      )}
      {replaceSecret && (
        <div className="modal-field">
          <label className="modal-field-label">{t('field.clientSecret')}</label>
          <TextInput
            className="mono"
            type="password"
            value={clientSecret}
            onChange={(e) => setClientSecret(e.target.value)}
            autoComplete="new-password"
          />
          <span className="modal-hint">{t('field.clientSecretHint')}</span>
        </div>
      )}
      <div className="modal-field">
        <label className="modal-field-label">{t('field.redirectUri')}</label>
        <TextInput
          className="mono"
          value={redirectUri}
          onChange={(e) => setRedirectUri(e.target.value)}
        />
        <span className="modal-hint">{t('field.redirectUriHint')}</span>
      </div>
      <div className="modal-field">
        <label className="modal-field-label">{t('field.scopes')}</label>
        <TextInput className="mono" value={scopes} onChange={(e) => setScopes(e.target.value)} />
      </div>
      <div className="modal-field">
        <label className="modal-field-label">{t('field.groupsClaim')}</label>
        <TextInput
          className="mono"
          value={groupsClaim}
          onChange={(e) => setGroupsClaim(e.target.value)}
        />
        <span className="modal-hint">{t('field.groupsClaimHint')}</span>
      </div>

      <div className="modal-field">
        <label className="modal-field-label">{t('field.roleMap')}</label>
        <span className="modal-hint">{t('field.roleMapHint')}</span>
        <div className="auth-rolemap">
          {rows.map((r, i) => (
            <div className="auth-rolemap-row" key={i}>
              <TextInput
                className="mono"
                placeholder={t('field.groupPlaceholder')}
                value={r.group}
                onChange={(e) => setRow(i, { group: e.target.value })}
              />
              <Select value={r.role} onChange={(e) => setRow(i, { role: e.target.value as Role })}>
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
        </div>
      </div>

      <div className="modal-field">
        <label className="modal-field-label">{t('field.defaultRole')}</label>
        <Select
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
        <span className="modal-hint">{t('field.defaultRoleHint')}</span>
      </div>

      <label className="auth-replace">
        <input type="checkbox" checked={enabled} onChange={(e) => setEnabled(e.target.checked)} />
        <span>{t('field.enabled')}</span>
      </label>

      {error && <p className="form-error">{error}</p>}
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
 *  **stored**, which is why it is disabled until the first save: validating a directory before
 *  switching it on is the whole point of it, but there is nothing to validate until something has
 *  been written. The result is rendered stage by stage rather than as a tick, because the check
 *  deliberately never binds as the user — an `ok` alone would be read as "login works". */
function DirectoryCard({ authed }: { authed: boolean }) {
  const { t } = useTranslation('settings-auth');
  const [stored, setStored] = useState<LdapConfigView | null>(null);
  const [form, setForm] = useState<LdapFormState>(emptyLdapForm());
  const [rows, setRows] = useState<RoleMapRow[]>([]);
  const [loading, setLoading] = useState(true);
  const [busy, setBusy] = useState(false);
  const [saved, setSaved] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [testing, setTesting] = useState(false);
  const [probeUser, setProbeUser] = useState('');
  const [result, setResult] = useState<LdapTestResult | null>(null);

  const load = useCallback(() => {
    api
      .getLdapConfig()
      .then((res) => {
        setStored(res.config ?? null);
        if (res.config) {
          setForm(toLdapForm(res.config));
          setRows(toRoleMapRows(res.config.role_map));
        }
      })
      .catch(() => {
        /* The page's own unavailable notice already covers 401/403. */
      })
      .finally(() => setLoading(false));
  }, []);

  useEffect(() => {
    load();
  }, [load]);

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
      await api.saveLdapConfig(toLdapInput(form, rows, stored));
      setSaved(true);
      // Reload rather than trusting the local state: this clears the password field and the replace
      // checkbox, which is what makes the next edit ask for the credential correctly.
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
    <Card title={t('ldap.title')}>
      <p className="modal-hint">{t('ldap.note')}</p>

      <div className="auth-grid">
        <label className="modal-field-label">{t('ldap.field.host')}</label>
        <TextInput
          className="mono"
          value={form.host}
          onChange={(e) => set({ host: e.target.value })}
          placeholder="dc1.corp.example.com"
        />

        <label className="modal-field-label">{t('ldap.field.security')}</label>
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

        <label className="modal-field-label">{t('ldap.field.port')}</label>
        <TextInput
          className="mono"
          value={form.port}
          onChange={(e) => set({ port: e.target.value })}
        />

        <label className="modal-field-label">{t('ldap.field.url')}</label>
        <span className="mono muted">{connectionUrl(form)}</span>

        <label className="modal-field-label">{t('ldap.field.caCert')}</label>
        <textarea
          className="mono"
          rows={4}
          value={form.caCert}
          onChange={(e) => set({ caCert: e.target.value })}
          placeholder="-----BEGIN CERTIFICATE-----"
        />

        <label className="modal-field-label">{t('ldap.field.bindDn')}</label>
        <TextInput
          className="mono"
          value={form.bindDn}
          onChange={(e) => set({ bindDn: e.target.value })}
        />

        {stored?.has_bind_password && (
          <>
            <label className="modal-field-label">{t('ldap.field.replacePassword')}</label>
            <label className="modal-check">
              <input
                type="checkbox"
                checked={form.replacePassword}
                onChange={(e) => set({ replacePassword: e.target.checked, bindPassword: '' })}
              />
              <span className="modal-hint">{t('ldap.field.replacePasswordHint')}</span>
            </label>
          </>
        )}

        {passwordIsEditable(stored, form) && (
          <>
            <label className="modal-field-label">{t('ldap.field.bindPassword')}</label>
            <TextInput
              type="password"
              autoComplete="new-password"
              value={form.bindPassword}
              onChange={(e) => set({ bindPassword: e.target.value })}
            />
          </>
        )}

        <label className="modal-field-label">{t('ldap.field.userBaseDn')}</label>
        <TextInput
          className="mono"
          value={form.userBaseDn}
          onChange={(e) => set({ userBaseDn: e.target.value })}
        />

        <label className="modal-field-label">{t('ldap.field.userFilter')}</label>
        <TextInput
          className="mono"
          value={form.userFilter}
          onChange={(e) => set({ userFilter: e.target.value })}
        />

        <label className="modal-field-label">{t('ldap.field.usernameAttribute')}</label>
        <TextInput
          className="mono"
          value={form.usernameAttribute}
          onChange={(e) => set({ usernameAttribute: e.target.value })}
        />

        <label className="modal-field-label">{t('ldap.field.uidAttribute')}</label>
        <TextInput
          className="mono"
          value={form.uidAttribute}
          onChange={(e) => set({ uidAttribute: e.target.value })}
        />

        <label className="modal-field-label">{t('ldap.field.memberOfAttribute')}</label>
        <TextInput
          className="mono"
          value={form.memberOfAttribute}
          onChange={(e) => set({ memberOfAttribute: e.target.value })}
        />

        <label className="modal-field-label">{t('ldap.field.groupBaseDn')}</label>
        <TextInput
          className="mono"
          value={form.groupBaseDn}
          onChange={(e) => set({ groupBaseDn: e.target.value })}
        />

        <label className="modal-field-label">{t('ldap.field.groupFilter')}</label>
        <TextInput
          className="mono"
          value={form.groupFilter}
          onChange={(e) => set({ groupFilter: e.target.value })}
        />
      </div>
      <span className="modal-hint">{t('ldap.field.groupSearchHint')}</span>

      <label className="modal-field-label">{t('field.roleMap')}</label>
      <span className="modal-hint">{t('ldap.field.roleMapHint')}</span>
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

      <div className="auth-grid">
        <label className="modal-field-label">{t('field.defaultRole')}</label>
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

        <label className="modal-field-label">{t('field.enabled')}</label>
        <label className="modal-check">
          <input
            type="checkbox"
            checked={form.enabled}
            onChange={(e) => set({ enabled: e.target.checked })}
          />
          <span className="modal-hint">{t('ldap.field.enabledHint')}</span>
        </label>
      </div>

      {error && <p className="form-error">{error}</p>}
      {saved && <p className="auth-saved">{t('ldap.saved')}</p>}

      <div className="auth-toolbar">
        <Button variant="primary" onClick={() => void save()} disabled={!authed || busy}>
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
          disabled={stored == null || testing || busy}
        >
          {t('ldap.test.run')}
        </Button>
      </div>
      {stored == null && <span className="modal-hint">{t('ldap.test.saveFirst')}</span>}

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
          <p className="modal-hint">{result.note}</p>
        </div>
      )}
    </Card>
  );
}

export function AuthSettingsPage() {
  const { t } = useTranslation('settings-auth');
  const authed = useAuthStore((s) => s.authed);
  const [rows, setRows] = useState<OidcProviderSummary[]>([]);
  const [unavailable, setUnavailable] = useState(false);
  const [loading, setLoading] = useState(true);
  const [adding, setAdding] = useState(false);
  const [editing, setEditing] = useState<OidcProviderSummary | null>(null);
  const [deleting, setDeleting] = useState<OidcProviderSummary | null>(null);

  const load = useCallback(() => {
    api
      .listOidcProviders()
      .then((list) => {
        setRows(list);
        setUnavailable(false);
      })
      .catch((e: unknown) => {
        if (e instanceof ApiError && (e.status === 401 || e.status === 403)) setUnavailable(true);
      })
      .finally(() => setLoading(false));
  }, []);

  useEffect(() => {
    load();
  }, [load]);

  return (
    <div>
      <PageHeader
        title={t('nav:settings.auth')}
        trail={[{ label: t('nav:sections.settings') }, { label: t('nav:settings.auth') }]}
        note={t('note')}
      />

      {/* ADR-044 moved the WebUI to HTTPS on a new port, and a stored redirect URI is an absolute
          URL that has to agree with what is registered at the IdP. A stale one fails at the token
          exchange, which reads as "SSO is broken" with nothing pointing at the upgrade. Yagra will
          not rewrite it — changing where an IdP may send an authorization code is not something an
          upgrade should do on somebody's behalf — so it says so instead. */}
      {rows.some((r) => redirectUriMismatch(window.location.origin, r.redirect_uri)) && (
        <Card>
          <p className="auth-redirect-warning">{t('redirectUriMismatch')}</p>
        </Card>
      )}

      {unavailable ? (
        <Card>
          <p className="muted">{t('unavailable')}</p>
        </Card>
      ) : (
        <>
          <div className="auth-toolbar">
            {authed && (
              <Button variant="primary" onClick={() => setAdding(true)}>
                + {t('add.title')}
              </Button>
            )}
          </div>

          {rows.length === 0 ? (
            <Card>
              <p className="muted">{loading ? t('common:loading') : t('empty')}</p>
            </Card>
          ) : (
            <div className="auth-list">
              {rows.map((p) => (
                <Card key={p.id}>
                  <div className="auth-provider">
                    <div className="auth-provider-main">
                      <div className="auth-provider-name">
                        {p.name}
                        <span className={p.enabled ? 'auth-badge on' : 'auth-badge off'}>
                          {p.enabled ? t('badge.enabled') : t('badge.disabled')}
                        </span>
                      </div>
                      <div className="auth-provider-meta mono">{p.issuer}</div>
                      <div className="auth-provider-meta">
                        {t('mappedGroups', { count: Object.keys(p.role_map).length })}
                      </div>
                    </div>
                    {authed && (
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
                    )}
                  </div>
                </Card>
              ))}
            </div>
          )}

          {/* The directory lives on this page rather than one of its own (ADR-041). "Who may sign
              in" is one subject with two sources, and a separate *Directory* nav item would be the
              second settings screen for one concept that decision 2 exists to prevent. */}
          <DirectoryCard authed={authed} />
        </>
      )}

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
    </div>
  );
}
