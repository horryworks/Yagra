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
import type { NetboxServer, NetboxSiteIdFields, NetboxSyncResult, NetboxTestResult } from '../../types/api';
import { PageHeader } from '../../components/ui/PageHeader';
import { Card } from '../../components/ui/Card';
import { Button } from '../../components/ui/Button';
import { Modal } from '../../components/ui/Modal';
import { TextInput, TextArea, Select } from '../../components/ui/Field';
import { ConfirmDeleteModal } from '../../components/ui/ConfirmDeleteModal';
import { classifyLoadError, type LoadBlock } from '../../lib/loadState';
import { LoadBlockNotice } from '../../components/ui/LoadBlockNotice';
import { syncSummary } from './netboxStatus';
import {
  SITE_ID_NONE,
  SITE_ID_OTHER,
  customKeyLooksValid,
  selectionFor,
  siteIdFieldToSend,
  siteIdOptions,
  siteIdOutcome,
} from './siteIdField';
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
  // The site-code sources this NetBox offers. `null` until something has asked — see `loadFields`
  // for why there are two ways to ask and neither is redundant.
  const [fields, setFields] = useState<NetboxSiteIdFields | null>(null);
  const initialSelection = selectionFor(existing?.site_id_field ?? null, null);
  const [siteIdSelected, setSiteIdSelected] = useState(initialSelection.selected);
  const [customKeyInput, setCustomKeyInput] = useState(initialSelection.customKeyInput);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  // A saved server's token is sealed and never returned, so the edit form cannot press "test
  // connection" to learn its fields — this route asks on its behalf. For an unsaved server there
  // is nothing to ask about yet, and the probe below answers instead.
  useEffect(() => {
    if (!existing) return;
    let live = true;
    api
      .netboxSiteFields(existing.id)
      .then((f) => {
        if (!live) return;
        setFields(f);
        // Re-derive the selection now the listing is known: a saved `cf:*` value that the listing
        // does mention should select its own row rather than stay on "Other".
        const s = selectionFor(existing.site_id_field ?? null, f);
        setSiteIdSelected(s.selected);
        setCustomKeyInput(s.customKeyInput);
      })
      // Deliberately silent: not knowing the field list is a degraded picker, not a broken form.
      // The built-ins and the type-it-in row are still there, which is why this is survivable.
      .catch(() => undefined);
    return () => {
      live = false;
    };
  }, [existing]);

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
      .then((r) => {
        setProbe(r);
        // The add form's only chance: the token exists on the server side for this one call.
        if (r.site_id_fields) setFields(r.site_id_fields);
      })
      .catch((e: unknown) => setError(errMsg(e, t('netbox.err.test'))))
      .finally(() => setBusy(false));
  };

  const save = () => {
    setBusy(true);
    setError(null);
    const secs = Number(intervalSecs);
    const ca = caPem.trim() === '' ? null : caPem;
    const siteIdField = siteIdFieldToSend(siteIdSelected, customKeyInput);
    const done = existing
      ? api.updateNetboxServer(existing.id, {
          name: name.trim(),
          base_url: baseUrl.trim(),
          // Omitted rather than sent empty, so the sealed token survives an unrelated edit.
          ...(token.trim() === '' ? {} : { token: token.trim() }),
          ca_cert_pem: ca,
          enabled: existing.enabled,
          sync_interval_secs: secs,
          site_id_field: siteIdField,
        })
      : api
          .createNetboxServer({
            name: name.trim(),
            base_url: baseUrl.trim(),
            token: token.trim(),
            ca_cert_pem: ca,
            sync_interval_secs: secs,
            site_id_field: siteIdField,
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
        <span>{t('netbox.form.siteIdField')}</span>
        <Select
          value={siteIdSelected}
          onChange={(e) => setSiteIdSelected(e.target.value)}
        >
          {siteIdOptions(fields, existing?.site_id_field ?? null).map((o) => {
            switch (o.kind) {
              case 'none':
                return (
                  <option key="none" value={SITE_ID_NONE}>
                    {t('netbox.siteIdField.none')}
                  </option>
                );
              case 'builtIn':
                return (
                  <option key={o.value} value={o.value}>
                    {t(`netbox.siteIdField.${o.value}`)}
                  </option>
                );
              // NetBox's own label, so it is shown verbatim rather than translated.
              case 'custom':
                return (
                  <option key={o.value} value={o.value}>
                    {o.label}
                  </option>
                );
              case 'other':
                return (
                  <option key="other" value={SITE_ID_OTHER}>
                    {t('netbox.siteIdField.other')}
                  </option>
                );
            }
          })}
        </Select>
        {siteIdSelected === SITE_ID_OTHER && (
          <TextInput
            value={customKeyInput}
            placeholder="site_id"
            autoComplete="off"
            onChange={(e) => setCustomKeyInput(e.target.value)}
          />
        )}
        <span className="netbox-hint">
          {siteIdSelected === SITE_ID_OTHER
            ? t('netbox.form.siteIdFieldKeyHint')
            : t('netbox.form.siteIdFieldHint')}
        </span>
        {/* 🚨 Said out loud, because otherwise an empty picker looks like "this NetBox has no
            custom fields" and the operator never finds the row above. */}
        {fields && !fields.custom_fields_readable && (
          <span className="netbox-hint netbox-hint-warn">
            {t('netbox.form.siteIdFieldUnreadable')}
          </span>
        )}
        {siteIdSelected === SITE_ID_OTHER &&
          customKeyInput.trim() !== '' &&
          !customKeyLooksValid(customKeyInput) && (
            <span className="netbox-hint netbox-hint-warn">
              {t('netbox.form.siteIdFieldKeyInvalid')}
            </span>
          )}
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
  const [lastRun, setLastRun] = useState<NetboxSyncResult | null>(null);
  const summary = syncSummary(server);

  const sync = () => {
    setBusy(true);
    setError(null);
    api
      .syncNetboxServer(server.id)
      .then((r) => {
        // Kept, not discarded: the Site ID count below is the only signal that separates "the
        // wrong field is selected" from "the feature does nothing", and this is where it arrives.
        setLastRun(r);
        onSynced();
      })
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
        // 🚨 Resent, not omitted. This is a full-document PUT, so leaving it out would clear the
        // Site ID setting every time someone flipped this switch — a folder tree renamed by an
        // unrelated click, with nothing on screen to connect the two.
        site_id_field: server.site_id_field,
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
        {(() => {
          const outcome = lastRun && siteIdOutcome(lastRun.sites, lastRun.sites_without_site_id);
          return (
            outcome && (
              <span className="netbox-missing">
                {t(`netbox.sync.siteId.${outcome.kind}`, {
                  without: outcome.without,
                  sites: outcome.sites,
                })}
              </span>
            )
          );
        })()}
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
