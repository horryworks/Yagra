// Authentication (Settings ▸ Auth): configure an external IdP for SSO (OIDC). The client_secret is
// write-only — the API never returns it — and IdP groups map to Yagra roles via the role map.
// ManageUsers-gated. Local accounts (Settings ▸ Users) keep working alongside SSO.

import { useCallback, useEffect, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { api, ApiError } from '../services/api';
import { useAuthStore } from '../store';
import type { OidcProviderSummary, OidcProviderInput, Role } from '../types/api';
import { PageHeader } from '../components/ui/PageHeader';
import { Card } from '../components/ui/Card';
import { Button } from '../components/ui/Button';
import { Modal } from '../components/ui/Modal';
import { TextInput, Select } from '../components/ui/Field';
import { OverflowMenu } from '../components/ui/OverflowMenu';
import { EditIcon, TrashIcon } from '../components/ui/icons';
import './AuthSettingsPage.css';

const ROLES: Role[] = ['viewer', 'operator', 'admin'];

const errMsg = (e: unknown, fallback: string) => (e instanceof ApiError ? e.message : fallback);

/** One editable IdP-group → role mapping row. */
interface MapRow {
  group: string;
  role: Role;
}

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
    provider ? Object.entries(provider.role_map).map(([group, role]) => ({ group, role })) : [],
  );
  const [defaultRole, setDefaultRole] = useState<Role | ''>(provider?.default_role ?? '');
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
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  return (
    <Modal
      title={t('delete.title')}
      onClose={onClose}
      footer={
        <>
          <Button variant="outline" onClick={onClose} disabled={busy}>
            {t('common:actions.cancel')}
          </Button>
          <Button
            variant="danger"
            disabled={busy}
            onClick={() => {
              setBusy(true);
              setError(null);
              api
                .deleteOidcProvider(provider.id)
                .then(onDone)
                .catch((e: unknown) => {
                  setError(errMsg(e, t('err.delete')));
                  setBusy(false);
                });
            }}
          >
            {t('common:actions.delete')}
          </Button>
        </>
      }
    >
      <p className="modal-confirm-text">{t('delete.confirm', { name: provider.name })}</p>
      {error && <p className="form-error">{error}</p>}
    </Modal>
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
