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

import { useEffect, useMemo, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { api, errMsg } from '../../services/api';
import { useCan } from '../../store';
import type { NetboxServer, NetboxSiteIdFields, NetboxTestResult } from '../../types/api';
import { PageHeader } from '../../components/ui/PageHeader';
import { Card } from '../../components/ui/Card';
import { Button } from '../../components/ui/Button';
import { Modal } from '../../components/ui/Modal';
import { TextInput, TextArea, Select, FieldError } from '../../components/ui/Field';
import { ConfirmDeleteModal } from '../../components/ui/ConfirmDeleteModal';
import { EmptyState } from '../../components/ui/EmptyState';
import { InfoTip } from '../../components/ui/InfoTip';
import { SecretInput } from '../../components/ui/SecretInput';
import { secretToSend } from '../../components/ui/secretField';
import { useLoad } from '../../lib/useLoad';
import { formatTimestamp } from '../../lib/format';
import { LoadGate } from '../../components/ui/LoadGate';
import { anySyncInProgress, syncProgress, syncSummary } from './netboxStatus';
import { useSyncWatch } from './useSyncWatch';
import { addressChangeNeedsToken } from './netboxBaseUrl';
import { baseUrlRefused, pemIsPrivateKey } from './netboxForm';
import {
  SITE_ID_NONE,
  SITE_ID_OTHER,
  customKeyLooksValid,
  selectionFor,
  siteIdFieldToSend,
  siteIdOptions,
  siteIdOutcome,
} from './siteIdField';
import { done, step } from '../../lib/submitState';
import { useSubmit } from '../../lib/useSubmit';
import { FormError, FormFooter } from '../../components/ui/FormFooter';
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
  // token. Empty on an edit means "keep the sealed one" (`SecretInput`).
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
  // Two actions on one form: testing the connection writes nothing and shows its answer here;
  // saving closes the dialog.
  const form = useSubmit({ errorFallback: t('netbox.err.save'), onDone: onSaved });

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

  // Said on the field rather than under it: the backend refuses these addresses (`netboxForm.ts`).
  const urlRefused = baseUrlRefused(baseUrl);
  const caIsKey = pemIsPrivateKey(caPem);
  const formOk = !urlRefused && !caIsKey;
  const canTest = formOk && baseUrl.trim() !== '' && token.trim() !== '';
  // ADR-178 decision 3: the stored token never goes to a new address — the backend refuses the save,
  // so the form asks for the token before Save rather than after.
  const tokenNeeded =
    !existing || addressChangeNeedsToken(existing.base_url, baseUrl);

  const test = () => {
    setProbe(null);
    form.submit(
      () =>
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
            return step();
          }),
      t('netbox.err.test'),
    );
  };

  const save = () => {
    const secs = Number(intervalSecs);
    const ca = caPem.trim() === '' ? null : caPem;
    const siteIdField = siteIdFieldToSend(siteIdSelected, customKeyInput);
    const typed = secretToSend(token);
    const request = () =>
      existing
        ? api.updateNetboxServer(existing.id, {
            name: name.trim(),
            base_url: baseUrl.trim(),
            // Omitted rather than sent empty, so the sealed token survives an unrelated edit.
            ...(typed === undefined ? {} : { token: typed }),
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
    form.submit(() => request().then(() => done()));
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
      resizeId="netboxIntegration"
      onClose={onClose}
      footer={
        <FormFooter
          form={form}
          onClose={onClose}
          onSubmit={save}
          submitLabel={t('common:actions.save')}
          canSubmit={
            formOk &&
            name.trim() !== '' &&
            baseUrl.trim() !== '' &&
            !(tokenNeeded && token.trim() === '')
          }
          extra={
            <Button type="button" onClick={test} disabled={form.busy || !canTest}>
              {t('netbox.form.test')}
            </Button>
          }
        />
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
        {urlRefused && <FieldError>{t('netbox.form.baseUrlRefused')}</FieldError>}
      </label>
      {/* Not a wrapping <label>: the stored state draws a Replace button, and a button inside a
          label becomes the label's control. */}
      <div className="netbox-field">
        <label htmlFor="netbox-token">{t('netbox.form.token')}</label>
        <SecretInput
          id="netbox-token"
          stored={existing !== null}
          mustReplace={existing !== null && tokenNeeded}
          value={token}
          onChange={setToken}
        />
        {existing && tokenNeeded && (
          <p className="form-warning">{t('netbox.form.tokenNewAddressHint')}</p>
        )}
      </div>
      <label className="netbox-field">
        <span>{t('netbox.form.caCert')}</span>
        <TextArea
          className="mono"
          rows={4}
          value={caPem}
          placeholder="-----BEGIN CERTIFICATE-----"
          onChange={(e) => setCaPem(e.target.value)}
        />
        {caIsKey && <FieldError>{t('netbox.form.caCertIsKey')}</FieldError>}
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
            placeholder={t('netbox.form.siteIdFieldKeyPlaceholder')}
            autoComplete="off"
            onChange={(e) => setCustomKeyInput(e.target.value)}
          />
        )}
        {/* 🚨 Said out loud, because otherwise an empty picker looks like "this NetBox has no
            custom fields" and the operator never finds the row above. */}
        {fields && !fields.custom_fields_readable && (
          <p className="form-warning">{t('netbox.form.siteIdFieldUnreadable')}</p>
        )}
        {siteIdSelected === SITE_ID_OTHER &&
          customKeyInput.trim() !== '' &&
          !customKeyLooksValid(customKeyInput) && (
            <FieldError>{t('netbox.form.siteIdFieldKeyInvalid')}</FieldError>
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
      <FormError form={form} />
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
  const progress = syncProgress(server);

  // Asks, then re-reads: the sync itself runs in the leader's loop (ADR-172 decision 1), so how it
  // went arrives on the row — which is also what survives leaving this page. `busy` covers only
  // the request.
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
    setError(null);
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
      // Into the row's own error slot. Swallowed, the switch simply snapped back, which reads as a
      // broken control rather than a refused write.
      .catch((e: unknown) => setError(errMsg(e, t('netbox.err.toggle'))));
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
        {progress.kind === 'queued' && <span className="muted">{t('netbox.sync.requested')}</span>}
        {progress.kind === 'running' && <span className="muted">{t('netbox.sync.running')}</span>}
        {summary.kind === 'never' && <span className="muted">{t('netbox.sync.never')}</span>}
        {summary.kind === 'ok' && (
          <>
            <span>{t('netbox.sync.ok', { at: formatTimestamp(Date.parse(summary.at)) })}</span>
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
          // From the row, not from the answer to the button: the Site ID count is the only signal
          // that separates "the wrong field is selected" from "the feature does nothing", and it
          // has to outlive leaving the page.
          const outcome =
            server.last_sync_sites != null &&
            siteIdOutcome(server.last_sync_sites, server.last_sync_sites_without_site_id ?? 0);
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
          {/* Not drawn while paused: the loop never runs a paused server, so the endpoint refuses
              the request (409) and a button that can only fail is not offered. */}
          {server.enabled && (
            <Button onClick={sync} disabled={busy || progress.kind !== 'none'}>
              {t('netbox.row.syncNow')}
            </Button>
          )}
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
  const [adding, setAdding] = useState(false);
  const [editing, setEditing] = useState<NetboxServer | null>(null);
  const [deleting, setDeleting] = useState<NetboxServer | null>(null);

  const list = useLoad(() => api.listNetboxServers(), [], { initial: [] as NetboxServer[] });
  const { data: servers, reload: load } = list;

  // A "Sync now" runs in the leader's loop (ADR-172 decision 1), so the rows change by themselves
  // while one is asked for or running. The list is a handful of rows, so the poll and the reload
  // after it are the same read.
  useSyncWatch(anySyncInProgress(servers), load, load);

  const content = useMemo(() => {
    return (
      <LoadGate load={list}>
        <Card
          title={
            <>
              {t('netbox.servers.title')}
              {/* What a sync may overwrite decides whether an operator edits a synced folder. */}
              <InfoTip infoKey="system:netbox.ownership.info" label={t('netbox.servers.title')} />
            </>
          }
          actions={
            canConfig ? (
              <Button variant="primary" onClick={() => setAdding(true)}>
                {t('netbox.servers.add')}
              </Button>
            ) : undefined
          }
        >
          {servers.length === 0 ? (
            <EmptyState
              text={t('netbox.servers.empty')}
              action={
                canConfig ? (
                  <Button type="button" variant="primary" onClick={() => setAdding(true)}>
                    {t('netbox.servers.add')}
                  </Button>
                ) : undefined
              }
            />
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
        </Card>
      </LoadGate>
    );
  }, [list, servers, canConfig, load, t]);

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
