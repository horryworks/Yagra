// SPDX-License-Identifier: AGPL-3.0-only
// API tokens (Settings ▸ API tokens): long-lived bearer tokens for non-browser clients — in
// particular an AI/MCP client (Claude Code/Desktop) authenticating against the read-only MCP tool
// surface (ADR-028). ManageUsers-gated. The raw token is returned once on create and never again
// (only its hash is stored, security.md/ADR-018), so the create flow reveals it in a one-time modal.
//
// Data-table standard v2: a toolbar (New + count) over the shared `DataTable`; revoke is a per-row
// OverflowMenu action with a confirm modal. Modeled on AuditPage (table) + AuthSettingsPage (modals).

import { useCallback, useEffect, useMemo, useState } from 'react';
import { useTranslation } from 'react-i18next';
import type { TFunction } from 'i18next';
import { api, errMsg, ApiError } from '../services/api';
import { useAuthStore } from '../store';
import { ROLES, type ApiTokenSummary, type CreatedApiToken, type Role } from '../types/api';
import { PageHeader } from '../components/ui/PageHeader';
import { Card } from '../components/ui/Card';
import { Button } from '../components/ui/Button';
import { ConfirmDeleteModal } from '../components/ui/ConfirmDeleteModal';
import { Badge } from '../components/ui/Badge';
import { Modal } from '../components/ui/Modal';
import { TextInput, Select } from '../components/ui/Field';
import { DataTable, type Column } from '../components/ui/DataTable';
import { TableToolbar, TableSpacer, ResultCount } from '../components/ui/TableToolbar';
import { TimeCell } from '../components/ui/tableCells';
import { OverflowMenu } from '../components/ui/OverflowMenu';
import { TrashIcon } from '../components/ui/icons';
import './ApiTokensPage.css';


/** Elevated roles read as a warmer tone so a non-viewer token stands out in the list. */
const roleTone = (role: Role): 'neutral' | 'info' | 'warning' =>
  role === 'admin' ? 'warning' : role === 'operator' ? 'info' : 'neutral';

/** Table columns. Renderers close over `t`, so the caller rebuilds them on a language switch.
 *  `onRevoke` is `null` when the viewer isn't an admin (no row actions rendered then). */
function tokenColumns(
  t: TFunction,
  onRevoke: ((row: ApiTokenSummary) => void) | null,
): Column<ApiTokenSummary>[] {
  const cols: Column<ApiTokenSummary>[] = [
    { key: 'name', header: t('cols.name'), width: '1fr', render: (r) => <span className="tok-name">{r.name}</span> },
    {
      key: 'role',
      header: t('cols.role'),
      width: '130px',
      render: (r) => <Badge tone={roleTone(r.role)}>{t(`common:role.${r.role}`)}</Badge>,
    },
    {
      key: 'status',
      header: t('cols.status'),
      width: '120px',
      render: (r) =>
        r.revoked_at ? (
          <Badge tone="neutral">{t('status.revoked')}</Badge>
        ) : (
          <Badge tone="up">{t('status.active')}</Badge>
        ),
    },
    { key: 'created', header: t('cols.created'), width: '190px', render: (r) => <TimeCell iso={r.created_at} /> },
    {
      key: 'lastUsed',
      header: t('cols.lastUsed'),
      width: '190px',
      render: (r) =>
        r.last_used_at ? <TimeCell iso={r.last_used_at} /> : <span className="muted">{t('lastUsed.never')}</span>,
    },
  ];
  if (onRevoke) {
    cols.push({
      key: 'actions',
      header: t('cols.actions'),
      width: '96px',
      align: 'right',
      render: (r) =>
        r.revoked_at ? null : (
          <OverflowMenu
            actions={[
              {
                label: t('revoke.action'),
                icon: <TrashIcon />,
                danger: true,
                onClick: () => onRevoke(r),
              },
            ]}
          />
        ),
    });
  }
  return cols;
}

/** Create a token: name + role. Scope is omitted (global/All) — the only scope the MCP surface
 *  accepts in Increment 1. On success the parent reveals the once-shown raw token. */
function CreateTokenModal({
  onClose,
  onCreated,
}: {
  onClose: () => void;
  onCreated: (created: CreatedApiToken) => void;
}) {
  const { t } = useTranslation('settings-tokens');
  const [name, setName] = useState('');
  const [role, setRole] = useState<Role>('viewer');
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const ready = name.trim() !== '';

  const submit = () => {
    if (!ready) return;
    setBusy(true);
    setError(null);
    api
      .createApiToken({ name: name.trim(), role })
      .then((created) => onCreated(created))
      .catch((e: unknown) => {
        setError(
          e instanceof ApiError && e.code === 'duplicate_name'
            ? t('err.duplicate')
            : errMsg(e, t('err.create')),
        );
        setBusy(false);
      });
  };

  return (
    <Modal
      title={t('add.title')}
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
        <TextInput
          value={name}
          placeholder={t('field.namePlaceholder')}
          onChange={(e) => setName(e.target.value)}
          autoFocus
        />
      </div>
      <div className="modal-field">
        <label className="modal-field-label">{t('field.role')}</label>
        <Select value={role} onChange={(e) => setRole(e.target.value as Role)}>
          {ROLES.map((r) => (
            <option key={r} value={r}>
              {t(`common:role.${r}`)}
            </option>
          ))}
        </Select>
        <span className="modal-hint">{t('field.roleHint')}</span>
      </div>
      {error && <p className="form-error">{error}</p>}
    </Modal>
  );
}

/** Reveal the raw token exactly once, with copy + a ready-to-paste MCP client command. */
function RevealTokenModal({ created, onClose }: { created: CreatedApiToken; onClose: () => void }) {
  const { t } = useTranslation('settings-tokens');
  const [copied, setCopied] = useState(false);
  const origin = typeof window !== 'undefined' ? window.location.origin : 'https://yagra.example';
  const command = `claude mcp add --transport http yagra ${origin}/mcp --header "Authorization: Bearer ${created.token}"`;

  const copy = (text: string) => {
    void navigator.clipboard?.writeText(text);
    setCopied(true);
    setTimeout(() => setCopied(false), 1200);
  };

  return (
    <Modal
      title={t('token.title')}
      size="wide"
      onClose={onClose}
      footer={
        <Button variant="primary" onClick={onClose}>
          {t('token.done')}
        </Button>
      }
    >
      <p className="modal-confirm-text">{t('token.intro')}</p>
      <div className="modal-field">
        <label className="modal-field-label">{t('token.label')}</label>
        <div className="tok-copyrow">
          <code className="tok-token mono">{created.token}</code>
          <Button variant="outline" onClick={() => copy(created.token)}>
            {copied ? t('common:copy.copied') : t('token.copy')}
          </Button>
        </div>
      </div>
      <div className="modal-field">
        <label className="modal-field-label">{t('token.usageHint')}</label>
        <code className="tok-cmd mono">{command}</code>
      </div>
    </Modal>
  );
}

/** Confirm + revoke a token. */
function RevokeTokenModal({
  token,
  onClose,
  onDone,
}: {
  token: ApiTokenSummary;
  onClose: () => void;
  onDone: () => void;
}) {
  const { t } = useTranslation('settings-tokens');
  return (
    <ConfirmDeleteModal
      title={t('revoke.title')}
      confirmLabel={t('revoke.action')}
      onConfirm={() => api.revokeApiToken(token.id)}
      errorFallback={t('err.revoke')}
      onClose={onClose}
      onDone={onDone}
    >
      {t('revoke.confirm', { name: token.name })}
    </ConfirmDeleteModal>
  );
}

export function ApiTokensPage() {
  const { t } = useTranslation('settings-tokens');
  const authed = useAuthStore((s) => s.authed);
  const [rows, setRows] = useState<ApiTokenSummary[]>([]);
  const [unavailable, setUnavailable] = useState(false);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [adding, setAdding] = useState(false);
  const [created, setCreated] = useState<CreatedApiToken | null>(null);
  const [revoking, setRevoking] = useState<ApiTokenSummary | null>(null);

  const load = useCallback(() => {
    setError(null);
    api
      .listApiTokens()
      .then((list) => {
        setRows(list);
        setUnavailable(false);
      })
      .catch((e: unknown) => {
        if (e instanceof ApiError && (e.status === 401 || e.status === 403)) setUnavailable(true);
        else setError(errMsg(e, t('err.load')));
      })
      .finally(() => setLoading(false));
  }, [t]);

  useEffect(() => {
    if (authed) load();
    else setLoading(false);
  }, [authed, load]);

  const columns = useMemo(
    () => tokenColumns(t, authed ? (r) => setRevoking(r) : null),
    [t, authed],
  );

  return (
    <div className="page-fill">
      <PageHeader
        title={t('nav:settings.apiTokens')}
        trail={[{ label: t('nav:sections.settings') }, { label: t('nav:settings.apiTokens') }]}
        note={t('note')}
      />

      {!authed ? (
        <Card>
          <p className="muted">{t('signInPrompt')}</p>
        </Card>
      ) : unavailable ? (
        <Card>
          <p className="muted">{t('unavailable')}</p>
        </Card>
      ) : (
        <>
          <TableToolbar>
            <Button variant="primary" onClick={() => setAdding(true)}>
              + {t('add.button')}
            </Button>
            <TableSpacer />
            <ResultCount shown={rows.length} noun={t('count', { count: rows.length })} />
          </TableToolbar>

          {error && <p className="form-error">{error}</p>}

          <DataTable
            rows={rows}
            columns={columns}
            rowKey={(r) => r.id}
            loading={loading}
            empty={t('empty')}
          />
        </>
      )}

      {adding && (
        <CreateTokenModal
          onClose={() => setAdding(false)}
          onCreated={(c) => {
            setAdding(false);
            setCreated(c);
            load();
          }}
        />
      )}
      {created && <RevealTokenModal created={created} onClose={() => setCreated(null)} />}
      {revoking && (
        <RevokeTokenModal
          token={revoking}
          onClose={() => setRevoking(null)}
          onDone={() => {
            setRevoking(null);
            load();
          }}
        />
      )}
    </div>
  );
}
