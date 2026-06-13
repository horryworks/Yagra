// Roles & privileges (Settings ▸ Roles & privileges). Read-only view of the RBAC model: the
// role-vs-privilege matrix (which permission each role grants) plus a short description of every
// role. Sourced from GET /api/v1/roles, which derives the matrix from the backend's authoritative
// Role::grants — so this page can never drift from what the API actually enforces.
//
// Custom (operator-defined) roles are not yet configurable; the built-in roles are shown with a
// note. When custom roles land, the same matrix renders them with `builtin: false`.

import { useEffect, useState } from 'react';
import { api, ApiError } from '../services/api';
import type { RoleMatrix } from '../types/api';
import { PageHeader } from '../components/ui/PageHeader';
import { Card } from '../components/ui/Card';
import './RolesPage.css';

export function RolesPage() {
  const [matrix, setMatrix] = useState<RoleMatrix | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    api
      .listRoles()
      .then(setMatrix)
      .catch((e: unknown) =>
        setError(e instanceof ApiError ? e.message : 'failed to load roles'),
      );
  }, []);

  return (
    <div>
      <PageHeader
        title="Roles & privileges"
        trail={[{ label: 'Settings' }, { label: 'Roles & privileges' }]}
        note="What each role can do. The matrix is derived from the privileges the API enforces."
      />

      {error && (
        <Card>
          <p className="muted">{error}</p>
        </Card>
      )}

      {matrix && (
        <>
          <Card title="Roles">
            <div className="roles-list">
              {matrix.roles.map((r) => (
                <div className="roles-role" key={r.key}>
                  <span className="roles-role-name">{r.label}</span>
                  <span className="roles-role-key mono">{r.key}</span>
                  {r.builtin && <span className="roles-builtin">built-in</span>}
                  <span className="muted roles-role-desc">{r.description}</span>
                </div>
              ))}
            </div>
          </Card>

          <Card title="Privilege matrix" className="roles-matrix-card">
            <div
              className="roles-matrix"
              style={{ gridTemplateColumns: `minmax(220px, 2fr) repeat(${matrix.roles.length}, 1fr)` }}
            >
              <div className="roles-h roles-h-perm">Privilege</div>
              {matrix.roles.map((r) => (
                <div className="roles-h roles-h-role" key={r.key}>
                  {r.label}
                </div>
              ))}

              {matrix.permissions.map((p) => (
                <RowFragment key={p.key} permKey={p.key} label={p.label} description={p.description} roles={matrix.roles} />
              ))}
            </div>
            <p className="muted roles-note">
              Built-in roles are fixed. Custom roles (operator-defined privilege sets) are planned
              — they will appear here as additional columns once configurable.
            </p>
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
}: {
  permKey: string;
  label: string;
  description: string;
  roles: RoleMatrix['roles'];
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
              <span className="roles-granted" title={`${r.label} can ${label.toLowerCase()}`}>
                ✓<span className="roles-sr"> granted</span>
              </span>
            ) : (
              <span className="roles-denied" title={`${r.label} cannot ${label.toLowerCase()}`}>
                —<span className="roles-sr"> not granted</span>
              </span>
            )}
          </div>
        );
      })}
    </>
  );
}
