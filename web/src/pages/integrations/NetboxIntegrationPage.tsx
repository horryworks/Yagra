// SPDX-License-Identifier: AGPL-3.0-only
// Settings ▸ Integrations ▸ NetBox. The detail page for the NetBox integration (ADR-100 Inc.1),
// reached from the Integrations catalogue.
//
// Register a NetBox deployment, test it before saving, and pull its Region tree and Sites into the
// folder tree under Nodes. Everything here is read-only toward NetBox — nothing this page does can
// change anything in a customer's NetBox.
//
// **Structured after `MerakiIntegrationPage`, deliberately.** Two integration pages that answer the
// same questions in two layouts is how a settings area starts reading as several products. That is
// also why the server list is a plain list inside a `Card` rather than a `DataTable`: it is bounded
// by what an operator typed in (`ui-conventions.md`'s scale test says No), and `DataTable` is
// `flex: 1` with its own scroller, which would need an invented fixed height here.
//
// The judgement that can be got wrong — what a server's three sync columns mean together — is in
// `netboxStatus.ts` where a test can reach it (`testing.md`).

import { useCallback, useEffect, useMemo, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { api, errMsg } from '../../services/api';
import { useCan } from '../../store';
import type { NetboxServer, NetboxTestResult } from '../../types/api';
import { PageHeader } from '../../components/ui/PageHeader';
import { Card } from '../../components/ui/Card';
import { Button } from '../../components/ui/Button';
import { Modal } from '../../components/ui/Modal';
import { TextInput, TextArea } from '../../components/ui/Field';
import { ConfirmDeleteModal } from '../../components/ui/ConfirmDeleteModal';
import { classifyLoadError, type LoadBlock } from '../../lib/loadState';
import { LoadBlockNotice } from '../../components/ui/LoadBlockNotice';
import { syncSummary } from './netboxStatus';
import './NetboxIntegrationPage.css';

/** The form behind both add and edit. One component because the two differ in exactly two things —
 *  whether the token is required, and which call it ends in — and a second copy would be the shape
 *  `extensibility.md` §3 is about. */
function ServerModal({
  existing,
  onClose,
  onSaved,
}: {
  existing: NetboxServer | null;
  onClose: () => void;
  onSaved: () => void;
}) {
  const { t } = useTranslation('system');
  const [name, setName] = useState(existing?.name ?? '');
  const [baseUrl, setBaseUrl] = useState(existing?.base_url ?? '');
  // ⚠️ Never prefilled, and there is nothing to prefill it from: the API does not return the
  // token. Empty on an edit means "keep the sealed one".
  const [token, setToken] = useState('');
  const [caPem, setCaPem] = useState(existing?.ca_cert_pem ?? '');
  const [intervalSecs, setIntervalSecs] = useState(String(existing?.sync_interval_secs ?? 3600));
  const [probe, setProbe] = useState<NetboxTestResult | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const canTest = baseUrl.trim() !== '' && token.trim() !== '';

  const test = () => {
    setBusy(true);
    setError(null);
    setProbe(null);
    api
      .testNetboxConnection({
        base_url: baseUrl.trim(),
        token: token.trim(),
        ca_cert_pem: caPem.trim() === '' ? null : caPem,
      })
      .then(setProbe)
      .catch((e: unknown) => setError(errMsg(e, t('netbox.err.test'))))
      .finally(() => setBusy(false));
  };

  const save = () => {
    setBusy(true);
    setError(null);
    const secs = Number(intervalSecs);
    const ca = caPem.trim() === '' ? null : caPem;
    const done = existing
      ? api.updateNetboxServer(existing.id, {
          name: name.trim(),
          base_url: baseUrl.trim(),
          // Omitted rather than sent empty, so the sealed token survives an unrelated edit.
          ...(token.trim() === '' ? {} : { token: token.trim() }),
          ca_cert_pem: ca,
          enabled: existing.enabled,
          sync_interval_secs: secs,
        })
      : api
          .createNetboxServer({
            name: name.trim(),
            base_url: baseUrl.trim(),
            token: token.trim(),
            ca_cert_pem: ca,
            sync_interval_secs: secs,
          })
          .then(() => undefined);
    done
      .then(onSaved)
      .catch((e: unknown) => {
        setError(errMsg(e, t('netbox.err.save')));
        setBusy(false);
      });
  };

  /** What the probe found, in the operator's terms.
   *
   *  🚨 The three outcomes are distinct because NetBox answers a wrong token and no token with the
   *  same 403 — but sends its `API-Version` header either way. Collapsing "wrong address" and
   *  "wrong token" into one message sends the operator to check the wrong field. */
  const probeLine = () => {
    if (!probe) return null;
    if (!probe.reachable) {
      return <p className="netbox-probe bad">{t('netbox.test.unreachable')}</p>;
    }
    if (!probe.authenticated) {
      return <p className="netbox-probe bad">{t('netbox.test.badToken')}</p>;
    }
    return (
      <p className="netbox-probe ok">
        {t('netbox.test.ok', { version: probe.netbox_version ?? probe.api_version ?? '?' })}
      </p>
    );
  };

  return (
    <Modal
      title={existing ? t('netbox.form.editTitle') : t('netbox.form.addTitle')}
      onClose={onClose}
      footer={
        <>
          <Button onClick={onClose} disabled={busy}>
            {t('common:actions.cancel')}
          </Button>
          <Button onClick={test} disabled={busy || !canTest}>
            {t('netbox.form.test')}
          </Button>
          <Button
            variant="primary"
            onClick={save}
            disabled={busy || name.trim() === '' || baseUrl.trim() === '' || (!existing && token.trim() === '')}
          >
            {t('common:actions.save')}
          </Button>
        </>
      }
    >
      <label className="netbox-field">
        <span>{t('netbox.form.name')}</span>
        <TextInput value={name} onChange={(e) => setName(e.target.value)} />
      </label>
      <label className="netbox-field">
        <span>{t('netbox.form.baseUrl')}</span>
        <TextInput
          value={baseUrl}
          placeholder="https://netbox.example.com"
          onChange={(e) => setBaseUrl(e.target.value)}
        />
        <span className="netbox-hint">{t('netbox.form.baseUrlHint')}</span>
      </label>
      <label className="netbox-field">
        <span>{t('netbox.form.token')}</span>
        <TextInput
          type="password"
          value={token}
          autoComplete="off"
          onChange={(e) => setToken(e.target.value)}
        />
        <span className="netbox-hint">
          {existing ? t('netbox.form.tokenKeepHint') : t('netbox.form.tokenHint')}
        </span>
      </label>
      <label className="netbox-field">
        <span>{t('netbox.form.caCert')}</span>
        <TextArea rows={4} value={caPem} onChange={(e) => setCaPem(e.target.value)} />
        <span className="netbox-hint">{t('netbox.form.caCertHint')}</span>
      </label>
      <label className="netbox-field">
        <span>{t('netbox.form.interval')}</span>
        <TextInput
          type="number"
          min={60}
          max={86400}
          value={intervalSecs}
          onChange={(e) => setIntervalSecs(e.target.value)}
        />
      </label>
      {probeLine()}
      {error && <p className="netbox-probe bad">{error}</p>}
    </Modal>
  );
}

/** One server's row. */
function ServerRow({
  server,
  canConfig,
  onEdit,
  onDelete,
  onSynced,
}: {
  server: NetboxServer;
  canConfig: boolean;
  onEdit: () => void;
  onDelete: () => void;
  onSynced: () => void;
}) {
  const { t } = useTranslation('system');
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const summary = syncSummary(server);

  const sync = () => {
    setBusy(true);
    setError(null);
    api
      .syncNetboxServer(server.id)
      .then(onSynced)
      .catch((e: unknown) => setError(errMsg(e, t('netbox.err.sync'))))
      .finally(() => setBusy(false));
  };

  const toggle = () => {
    api
      .updateNetboxServer(server.id, {
        name: server.name,
        base_url: server.base_url,
        enabled: !server.enabled,
        sync_interval_secs: server.sync_interval_secs,
      })
      .then(onSynced)
      .catch(() => undefined);
  };

  return (
    <div className="netbox-row">
      <div className="netbox-row-main">
        <span className="netbox-row-name">{server.name}</span>
        <span className="netbox-row-url">{server.base_url}</span>
        {server.api_version && (
          <span className="netbox-row-version">{t('netbox.row.version', { version: server.api_version })}</span>
        )}
      </div>

      <div className="netbox-row-sync">
        {summary.kind === 'never' && <span className="muted">{t('netbox.sync.never')}</span>}
        {summary.kind === 'ok' && (
          <>
            <span>{t('netbox.sync.ok', { at: new Date(summary.at).toLocaleString() })}</span>
            {/* Marked, never auto-deleted — ADR-100 decision 5. The operator decides. */}
            {summary.missing > 0 && (
              <span className="netbox-missing">
                {t('netbox.sync.missing', { count: summary.missing })}
              </span>
            )}
          </>
        )}
        {summary.kind === 'failed' && (
          <span className="netbox-failed" title={summary.error ?? undefined}>
            {t('netbox.sync.failed')}
            {summary.error ? `: ${summary.error}` : ''}
          </span>
        )}
        {error && <span className="netbox-failed">{error}</span>}
      </div>

      {canConfig && (
        <div className="netbox-row-actions">
          <label className="netbox-switch">
            <input type="checkbox" checked={server.enabled} onChange={toggle} />
            <span>{server.enabled ? t('netbox.row.enabled') : t('netbox.row.paused')}</span>
          </label>
          <Button onClick={sync} disabled={busy}>
            {t('netbox.row.syncNow')}
          </Button>
          <Button onClick={onEdit}>{t('common:actions.edit')}</Button>
          <Button onClick={onDelete}>{t('common:actions.delete')}</Button>
        </div>
      )}
    </div>
  );
}

export function NetboxIntegrationPage() {
  const { t } = useTranslation('system');
  // The permission the handlers' `RequireManageConfig` checks — never `authed`, never a role
  // (ADR-056). A control the caller may not use is not drawn.
  const canConfig = useCan('manage_config');
  const [servers, setServers] = useState<NetboxServer[]>([]);
  const [block, setBlock] = useState<LoadBlock | null>(null);
  const [adding, setAdding] = useState(false);
  const [editing, setEditing] = useState<NetboxServer | null>(null);
  const [deleting, setDeleting] = useState<NetboxServer | null>(null);

  const load = useCallback(() => {
    api
      .listNetboxServers()
      .then((list) => {
        setServers(list);
        setBlock(null);
      })
      .catch((e: unknown) => setBlock(classifyLoadError(e)));
  }, []);

  useEffect(() => {
    load();
  }, [load]);

  const content = useMemo(() => {
    if (block) {
      return <LoadBlockNotice block={block} unavailable={t('integrations.unavailable')} />;
    }
    return (
      <Card
        title={t('netbox.servers.title')}
        actions={
          canConfig ? (
            <Button variant="primary" onClick={() => setAdding(true)}>
              {t('netbox.servers.add')}
            </Button>
          ) : undefined
        }
      >
        {servers.length === 0 ? (
          // ADR-055 R6: say what this screen is for where someone comes looking for it, rather
          // than showing an empty box.
          <p className="muted">{t('netbox.servers.empty')}</p>
        ) : (
          <div className="netbox-list">
            {servers.map((s) => (
              <ServerRow
                key={s.id}
                server={s}
                canConfig={canConfig}
                onEdit={() => setEditing(s)}
                onDelete={() => setDeleting(s)}
                onSynced={load}
              />
            ))}
          </div>
        )}
        <p className="muted netbox-ownership">{t('netbox.servers.ownership')}</p>
      </Card>
    );
  }, [block, servers, canConfig, load, t]);

  return (
    <div>
      <PageHeader
        title={t('netbox.name')}
        trail={[
          { label: t('nav:sections.settings') },
          { label: t('nav:settings.integrations'), to: '/settings/integrations' },
          { label: t('netbox.name') },
        ]}
        note={t('netbox.note')}
      />
      {content}

      {adding && (
        <ServerModal
          existing={null}
          onClose={() => setAdding(false)}
          onSaved={() => {
            setAdding(false);
            load();
          }}
        />
      )}
      {editing && (
        <ServerModal
          existing={editing}
          onClose={() => setEditing(null)}
          onSaved={() => {
            setEditing(null);
            load();
          }}
        />
      )}
      {deleting && (
        <ConfirmDeleteModal
          title={t('netbox.delete.title')}
          onConfirm={() => api.deleteNetboxServer(deleting.id)}
          errorFallback={t('netbox.err.delete')}
          onClose={() => setDeleting(null)}
          onDone={() => {
            setDeleting(null);
            load();
          }}
        >
          {/* The folders survive on purpose (ADR-100 decision 5) — saying so here is what stops
              this from reading as "this will delete my site tree". */}
          {t('netbox.delete.body', { name: deleting.name })}
        </ConfirmDeleteModal>
      )}
    </div>
  );
}
