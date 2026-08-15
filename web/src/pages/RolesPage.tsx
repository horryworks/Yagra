// SPDX-License-Identifier: AGPL-3.0-only
// Roles & privileges (Settings ▸ Roles & privileges). Read-only view of the RBAC model: the
// role-vs-privilege matrix (which permission each role grants) plus a short description of every
// role. Sourced from GET /api/v1/roles, which derives the matrix from the backend's authoritative
// Role::grants — so this page can never drift from what the API actually enforces.
//
// Custom (operator-defined) roles are not yet configurable; the built-in roles are shown with a
// note. When custom roles land, the same matrix renders them with `builtin: false`.

import { useEffect, useState } from 'react';
import { useTranslation } from 'react-i18next';
import type { TFunction } from 'i18next';
import { api, ApiError } from '../services/api';
import type { Permission, RoleMatrix } from '../types/api';
import { PageHeader } from '../components/ui/PageHeader';
import { Card } from '../components/ui/Card';
import { useIsMobileViewport } from '../lib/viewport';
import './RolesPage.css';

export function RolesPage() {
  const { t } = useTranslation('access');
  const mobile = useIsMobileViewport();
  const [matrix, setMatrix] = useState<RoleMatrix | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    api
      .listRoles()
      .then(setMatrix)
      .catch((e: unknown) =>
        setError(e instanceof ApiError ? e.message : t('roles.err.load')),
      );
  }, [t]);

  return (
    <div>
      <PageHeader
        title={t('nav:settings.roles')}
        trail={[{ label: t('nav:sections.settings') }, { label: t('nav:settings.roles') }]}
        note={t('roles.note')}
      />

      {error && (
        <Card>
          <p className="muted">{error}</p>
        </Card>
      )}

      {matrix && (
        <>
          <Card title={t('roles.section.roles')}>
            <div className="roles-list">
              {matrix.roles.map((r) => (
                <div className="roles-role" key={r.key}>
                  <span className="roles-role-name">{r.label}</span>
                  <span className="roles-role-key mono">{r.key}</span>
                  {r.builtin && <span className="roles-builtin">{t('roles.builtin')}</span>}
                  <span className="muted roles-role-desc">{r.description}</span>
                </div>
              ))}
            </div>
          </Card>

          <Card title={t('roles.matrixTitle')} className="roles-matrix-card">
            {/* On mobile the fr columns would crush together, so widen to px-min tracks and let the
                wrapper scroll sideways (the privilege column is pinned via sticky CSS). */}
            <div className="roles-matrix-scroll">
              <div
                className="roles-matrix"
                style={{
                  gridTemplateColumns: mobile
                    ? `minmax(200px, 1.6fr) repeat(${matrix.roles.length}, minmax(92px, 1fr))`
                    : `minmax(220px, 2fr) repeat(${matrix.roles.length}, 1fr)`,
                }}
              >
                <div className="roles-h roles-h-perm">{t('roles.privilege')}</div>
                {matrix.roles.map((r) => (
                  <div className="roles-h roles-h-role" key={r.key}>
                    {r.label}
                  </div>
                ))}

                {matrix.permissions.map((p) => (
                  <RowFragment
                    key={p.key}
                    permKey={p.key}
                    label={p.label}
                    description={p.description}
                    roles={matrix.roles}
                    t={t}
                  />
                ))}
              </div>
            </div>
            <p className="muted roles-note">{t('roles.customNote')}</p>
          </Card>
        </>
      )}
    </div>
  );
}

/** One permission row across all roles: the privilege (label + description) then a ✓/— per role.
 *  Status is shown with an icon + text, never colour alone (color-blind safe). */
function RowFragment({
  permKey,
  label,
  description,
  roles,
  t,
}: {
  permKey: Permission;
  label: string;
  description: string;
  roles: RoleMatrix['roles'];
  t: TFunction;
}) {
  return (
    <>
      <div className="roles-cell roles-cell-perm">
        <span className="roles-perm-label">{label}</span>
        <span className="muted roles-perm-desc">{description}</span>
      </div>
      {roles.map((r) => {
        const granted = r.permissions.includes(permKey);
        return (
          <div className="roles-cell roles-cell-mark" key={r.key}>
            {granted ? (
              <span
                className="roles-granted"
                title={t('roles.granted.title', { role: r.label, privilege: label.toLowerCase() })}
              >
                ✓<span className="roles-sr">{' '}{t('roles.sr.granted')}</span>
              </span>
            ) : (
              <span
                className="roles-denied"
                title={t('roles.denied.title', { role: r.label, privilege: label.toLowerCase() })}
              >
                —<span className="roles-sr">{' '}{t('roles.sr.notGranted')}</span>
              </span>
            )}
          </div>
        );
      })}
    </>
  );
}
