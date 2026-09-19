// SPDX-License-Identifier: AGPL-3.0-only
// Settings ▸ Integrations ▸ Cisco Meraki. The page for the Meraki integration as a whole (reached
// from the Integrations catalog): onboard organizations (read-only Dashboard API), manage each
// one's enable/cadence/network scope, and toggle the global kill switch.
//
// What an organization *holds* is not here. Its devices, which of them are monitored and how new
// ones are imported live on the organization's own page (`MerakiOrgPage`, ADR-164 Inc.4/5) — each
// row's name and its "Devices" button go there. That page replaced the import wizard this one used
// to open: a modal could list candidates, but it had nowhere to keep a setting.
//
// The API key is entered inline (it belongs to one org set, unlike a shared SNMP community); the
// backend seals it into the credentials store as a `meraki_api` credential. Everything here is
// read-only toward Meraki — nothing this page does can change a customer's Meraki configuration.

import { useCallback, useEffect, useMemo, useState } from 'react';
import { Trans, useTranslation } from 'react-i18next';
import { Link, useNavigate } from 'react-router-dom';
import { api, errMsg } from '../../services/api';
import { useCan } from '../../store';
import type { MerakiNetwork, MerakiOrg, MerakiOrgOption } from '../../types/api';
import { PageHeader } from '../../components/ui/PageHeader';
import { Card } from '../../components/ui/Card';
import { Button } from '../../components/ui/Button';
import { Modal } from '../../components/ui/Modal';
import { ConfirmDeleteModal } from '../../components/ui/ConfirmDeleteModal';
import { TextInput, Select } from '../../components/ui/Field';
import { SELECTABLE_MERAKI_TIERS } from '../merakiTiers';
import './MerakiIntegrationPage.css';
import { classifyLoadError, type LoadBlock } from '../../lib/loadState';
import { LoadBlockNotice } from '../../components/ui/LoadBlockNotice';
import { tierList } from '../merakiTiers';
import { DEFAULT_MERAKI_BASE_URL, MERAKI_REGIONS } from './merakiRegions';
import { canSyncNow, merakiOrgPath } from './merakiOrgRow';
import { MerakiSyncButton, MerakiSyncStatus } from './MerakiSyncStatus';
import { useMerakiSync } from './useMerakiSync';

/** Add one or more organizations under a shared read-only API key (discover → multi-select). */
function AddOrgModal({ onClose, onSaved }: { onClose: () => void; onSaved: () => void }) {
  const { t } = useTranslation('system');
  const [apiKey, setApiKey] = useState('');
  const [baseUrl, setBaseUrl] = useState<string>(DEFAULT_MERAKI_BASE_URL);
  const [orgs, setOrgs] = useState<MerakiOrgOption[] | null>(null);
  const [selected, setSelected] = useState<Set<string>>(new Set());
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const discover = () => {
    setBusy(true);
    setError(null);
    api
      .merakiDiscover({ api_key: apiKey, base_url: baseUrl })
      .then((list) => {
        setOrgs(list);
        setSelected(new Set(list.map((o) => o.id)));
      })
      .catch((e: unknown) => setError(errMsg(e, t('meraki.err.discover'))))
      .finally(() => setBusy(false));
  };

  const toggle = (id: string) =>
    setSelected((s) => {
      const next = new Set(s);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });

  const create = () => {
    setBusy(true);
    setError(null);
    api
      .createMerakiOrgs({ api_key: apiKey, base_url: baseUrl, org_ids: [...selected] })
      .then(() => {
        onSaved();
        onClose();
      })
      .catch((e: unknown) => {
        setError(errMsg(e, t('meraki.err.addOrgs')));
        setBusy(false);
      });
  };

  return (
    <Modal
      title={t('meraki.addOrg.title')}
      onClose={onClose}
      footer={
        <>
          <Button variant="outline" onClick={onClose} disabled={busy}>
            {t('common:actions.cancel')}
          </Button>
          {orgs === null ? (
            <Button variant="primary" onClick={discover} disabled={!apiKey.trim() || busy}>
              {t('meraki.addOrg.find')}
            </Button>
          ) : (
            <Button variant="primary" onClick={create} disabled={selected.size === 0 || busy}>
              {t('meraki.addOrg.add', { count: selected.size })}
            </Button>
          )}
        </>
      }
    >
      {orgs === null ? (
        <>
          <div className="modal-field">
            <label className="modal-field-label">{t('meraki.addOrg.region')}</label>
            <Select value={baseUrl} onChange={(e) => setBaseUrl(e.target.value)}>
              {MERAKI_REGIONS.map((r) => (
                <option key={r.base_url} value={r.base_url}>
                  {t(`meraki.regions.${r.key}`)}
                </option>
              ))}
            </Select>
          </div>
          <div className="modal-field">
            <label className="modal-field-label">{t('meraki.addOrg.apiKeyLabel')}</label>
            <TextInput
              className="mono"
              type="password"
              placeholder={t('meraki.addOrg.apiKeyPlaceholder')}
              value={apiKey}
              onChange={(e) => setApiKey(e.target.value)}
              autoComplete="new-password"
              autoFocus
            />
            <span className="modal-hint">
              <Trans t={t} i18nKey="meraki.addOrg.apiKeyHint" components={{ b: <strong /> }} />
            </span>
          </div>
        </>
      ) : (
        <>
          <p className="modal-hint">{t('meraki.addOrg.selectHint')}</p>
          <div className="meraki-org-picker">
            {orgs.length === 0 && <p className="muted">{t('meraki.addOrg.noOrgs')}</p>}
            {orgs.map((o) => (
              <label className="meraki-check-row" key={o.id}>
                <input
                  type="checkbox"
                  checked={selected.has(o.id)}
                  onChange={() => toggle(o.id)}
                />
                <span className="meraki-check-name">{o.name || o.id}</span>
                <span className="meraki-check-sub mono">{o.id}</span>
              </label>
            ))}
          </div>
        </>
      )}
      {error && <p className="form-error">{error}</p>}
    </Modal>
  );
}

/** Edit which of an org's networks are monitored (watch/skip scope). */
function NetworksModal({
  org,
  onClose,
  onSaved,
}: {
  org: MerakiOrg;
  onClose: () => void;
  onSaved: () => void;
}) {
  const { t } = useTranslation('system');
  const [networks, setNetworks] = useState<MerakiNetwork[] | null>(null);
  const [monitored, setMonitored] = useState<Set<string>>(new Set());
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    api
      .listMerakiNetworks(org.id)
      .then((nets) => {
        setNetworks(nets);
        setMonitored(new Set(nets.filter((n) => n.monitored).map((n) => n.network_id)));
      })
      .catch((e: unknown) => setError(errMsg(e, t('meraki.err.loadNetworks'))));
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [org.id]);

  const toggle = (id: string) =>
    setMonitored((s) => {
      const next = new Set(s);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });

  const save = () => {
    if (!networks) return;
    const on = networks.filter((n) => monitored.has(n.network_id)).map((n) => n.network_id);
    const off = networks.filter((n) => !monitored.has(n.network_id)).map((n) => n.network_id);
    setBusy(true);
    setError(null);
    Promise.all([
      on.length ? api.setMerakiNetworksMonitored(org.id, on, true) : Promise.resolve(),
      off.length ? api.setMerakiNetworksMonitored(org.id, off, false) : Promise.resolve(),
    ])
      .then(() => {
        onSaved();
        onClose();
      })
      .catch((e: unknown) => {
        setError(errMsg(e, t('meraki.err.saveScope')));
        setBusy(false);
      });
  };

  return (
    <Modal
      title={t('meraki.networks.title', { name: org.name })}
      onClose={onClose}
      footer={
        <>
          <Button variant="outline" onClick={onClose} disabled={busy}>
            {t('common:actions.cancel')}
          </Button>
          <Button variant="primary" onClick={save} disabled={busy || !networks}>
            {t('common:actions.save')}
          </Button>
        </>
      }
    >
      <p className="modal-hint">{t('meraki.networks.hint')}</p>
      {networks === null ? (
        <p className="muted">{t('common:loading')}</p>
      ) : networks.length === 0 ? (
        <p className="muted">{t('meraki.networks.empty')}</p>
      ) : (
        <div className="meraki-org-picker">
          {networks.map((n) => (
            <label className="meraki-check-row" key={n.network_id}>
              <input
                type="checkbox"
                checked={monitored.has(n.network_id)}
                onChange={() => toggle(n.network_id)}
              />
              <span className="meraki-check-name">{n.name || n.network_id}</span>
              <span className="meraki-check-sub mono">{n.network_id}</span>
            </label>
          ))}
        </div>
      )}
      {error && <p className="form-error">{error}</p>}
    </Modal>
  );
}

/** Edit an org's per-tier cadence, enabled tiers, and rate budget. */
function CadenceModal({
  org,
  onClose,
  onSaved,
}: {
  org: MerakiOrg;
  onClose: () => void;
  onSaved: () => void;
}) {
  const { t } = useTranslation('system');
  const [availability, setAvailability] = useState(org.availability_secs);
  const [uplink, setUplink] = useState(org.uplink_secs);
  const [traffic, setTraffic] = useState(org.traffic_secs);
  const [inventory, setInventory] = useState(org.inventory_secs);
  const [tiers, setTiers] = useState<Set<string>>(new Set(org.enabled_tiers));
  const [targetRps, setTargetRps] = useState(org.target_rps);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const toggleTier = (tier: string) =>
    setTiers((s) => {
      const next = new Set(s);
      if (next.has(tier)) next.delete(tier);
      else next.add(tier);
      return next;
    });

  const save = () => {
    setBusy(true);
    setError(null);
    api
      .setMerakiOrgCadence(org.id, {
        availability_secs: availability,
        uplink_secs: uplink,
        traffic_secs: traffic,
        inventory_secs: inventory,
        enabled_tiers: [...tiers],
        target_rps: targetRps,
      })
      .then(() => {
        onSaved();
        onClose();
      })
      .catch((e: unknown) => {
        setError(errMsg(e, t('meraki.err.saveCadence')));
        setBusy(false);
      });
  };

  const numField = (label: string, value: number, set: (n: number) => void, hint: string) => (
    <div className="modal-field">
      <label className="modal-field-label">{label}</label>
      <TextInput
        type="number"
        value={value}
        onChange={(e) => set(Number(e.target.value))}
      />
      <span className="modal-hint">{hint}</span>
    </div>
  );

  return (
    <Modal
      title={t('meraki.cadence.title', { name: org.name })}
      onClose={onClose}
      footer={
        <>
          <Button variant="outline" onClick={onClose} disabled={busy}>
            {t('common:actions.cancel')}
          </Button>
          <Button variant="primary" onClick={save} disabled={busy}>
            {t('common:actions.save')}
          </Button>
        </>
      }
    >
      <div className="modal-field">
        <label className="modal-field-label">{t('meraki.cadence.enabledTiers')}</label>
        <div className="meraki-tier-row">
          {SELECTABLE_MERAKI_TIERS.map((tier) => (
            <label className="meraki-chip-check" key={tier}>
              <input type="checkbox" checked={tiers.has(tier)} onChange={() => toggleTier(tier)} />
              <span>{t(`meraki.tier.${tier}`)}</span>
            </label>
          ))}
        </div>
      </div>
      {numField(t('meraki.cadence.availabilityInterval'), availability, setAvailability, '60–3600')}
      {numField(t('meraki.cadence.uplinkInterval'), uplink, setUplink, '60–3600')}
      {numField(t('meraki.cadence.trafficInterval'), traffic, setTraffic, '300–86400')}
      {numField(t('meraki.cadence.inventoryInterval'), inventory, setInventory, '60–604800')}
      {numField(
        t('meraki.cadence.rateBudget'),
        targetRps,
        setTargetRps,
        t('meraki.cadence.rateBudgetHint'),
      )}
      {error && <p className="form-error">{error}</p>}
    </Modal>
  );
}

/** One organization's row: what it is, how its last inventory sync went, and what can be done.
 *
 *  Its own component so "Sync now" has somewhere to keep its busy flag and its failure — a sync is
 *  the one write on this page that takes seconds, and a row that gave no sign of it would be
 *  pressed twice. The judgement (which sync column wins, when counts mean anything, whether the
 *  button is drawn) is in `merakiOrgRow.ts`, where a test reaches it; the sync line and the button
 *  are `MerakiSyncStatus`, shared with the organization's own page. */
function OrgRow({
  org,
  canConfig,
  pollingOn,
  onSynced,
  onNetworks,
  onCadence,
  onToggle,
  onDelete,
}: {
  org: MerakiOrg;
  canConfig: boolean;
  pollingOn: boolean;
  onSynced: () => void;
  onNetworks: () => void;
  onCadence: () => void;
  onToggle: () => void;
  onDelete: () => void;
}) {
  const { t } = useTranslation('system');
  const navigate = useNavigate();
  const sync = useMerakiSync(org.id, t('meraki.err.sync'), onSynced);

  return (
    <div className="meraki-org">
      <div className="meraki-org-info">
        <div className="meraki-org-main">
          <Link className="meraki-org-name" to={merakiOrgPath(org.id)}>
            {org.name}
          </Link>
          <span className="meraki-org-id mono">{t('meraki.orgs.orgId', { id: org.org_id })}</span>
          <span className={`meraki-org-state ${org.enabled ? 'on' : 'off'}`}>
            {org.enabled ? t('meraki.orgs.stateEnabled') : t('meraki.orgs.statePaused')}
          </span>
          <span className="meraki-org-tiers">
            {t('meraki.tiersPrefix')} {tierList(org.enabled_tiers, t)}
          </span>
        </div>

        <MerakiSyncStatus org={org} error={sync.error} />
      </div>

      <div className="meraki-org-actions">
        {canConfig && canSyncNow(org, pollingOn) && <MerakiSyncButton sync={sync} />}
        {/* Not behind `canConfig`: it goes to a screen anyone who can read this one can read. The
            writes on that screen are gated there, each by its own control (ADR-056). */}
        <Button variant="outline" onClick={() => navigate(merakiOrgPath(org.id))}>
          {t('meraki.org.devices')}
        </Button>
        {canConfig && (
          <>
            <Button variant="outline" onClick={onNetworks}>
              {t('meraki.org.networks')}
            </Button>
            <Button variant="outline" onClick={onCadence}>
              {t('meraki.org.cadence')}
            </Button>
            <Button variant="outline" onClick={onToggle}>
              {org.enabled ? t('meraki.org.pause') : t('meraki.org.resume')}
            </Button>
            <Button variant="danger" onClick={onDelete}>
              {t('common:actions.delete')}
            </Button>
          </>
        )}
      </div>
    </div>
  );
}

export function MerakiIntegrationPage() {
  const { t } = useTranslation('system');
  const canConfig = useCan('manage_config');
  const [orgs, setOrgs] = useState<MerakiOrg[]>([]);
  const [loading, setLoading] = useState(true);
  const [block, setBlock] = useState<LoadBlock | null>(null);
  const [pollingOn, setPollingOn] = useState(true);
  const [adding, setAdding] = useState(false);
  const [editing, setEditing] = useState<MerakiOrg | null>(null);
  const [scoping, setScoping] = useState<MerakiOrg | null>(null);
  const [deleting, setDeleting] = useState<MerakiOrg | null>(null);
  // A write made straight from this page (the kill switch, pause/resume) has no dialog to report
  // into, so its failure lands here. All three of this page's inline writes used to swallow theirs
  // (`.catch(() => undefined)`): a refused pause looked exactly like one that worked (ADR-164).
  const [actionError, setActionError] = useState<string | null>(null);

  const load = useCallback(() => {
    Promise.all([api.listMerakiOrgs(), api.getMerakiPolling()])
      .then(([list, polling]) => {
        setOrgs(list);
        setPollingOn(polling.enabled);
        setBlock(null);
      })
      .catch((e: unknown) => setBlock(classifyLoadError(e)))
      .finally(() => setLoading(false));
  }, []);

  useEffect(() => {
    load();
  }, [load]);

  const togglePolling = () => {
    const next = !pollingOn;
    setPollingOn(next);
    setActionError(null);
    api.setMerakiPolling(next).catch((e: unknown) => {
      setPollingOn(!next);
      setActionError(errMsg(e, t('meraki.err.polling')));
    });
  };

  const toggleEnabled = (org: MerakiOrg) => {
    setActionError(null);
    api
      .setMerakiOrgEnabled(org.id, !org.enabled)
      .then(load)
      .catch((e: unknown) => setActionError(errMsg(e, t('meraki.err.toggleOrg'))));
  };

  const content = useMemo(() => {
    if (block) {
      return <LoadBlockNotice block={block} unavailable={t('integrations.unavailable')} />;
    }
    return (
      <>
        {actionError && <p className="form-error meraki-page-note">{actionError}</p>}
        <Card title={t('meraki.polling.title')} className="meraki-killswitch-card">
          <label className="meraki-switch">
            <input
              type="checkbox"
              checked={pollingOn}
              onChange={togglePolling}
              disabled={!canConfig}
            />
            <span>{pollingOn ? t('meraki.polling.enabled') : t('meraki.polling.paused')}</span>
          </label>
          <p className="muted meraki-killswitch-hint">{t('meraki.polling.hint')}</p>
        </Card>

        <Card
          title={t('meraki.orgs.title')}
          actions={
            canConfig ? (
              <Button variant="primary" onClick={() => setAdding(true)}>
                {t('meraki.orgs.add')}
              </Button>
            ) : undefined
          }
        >
          {orgs.length === 0 ? (
            <p className="muted">{loading ? t('common:loading') : t('meraki.orgs.empty')}</p>
          ) : (
            <div className="meraki-org-list">
              {orgs.map((o) => (
                <OrgRow
                  key={o.id}
                  org={o}
                  canConfig={canConfig}
                  pollingOn={pollingOn}
                  onSynced={load}
                  onNetworks={() => setScoping(o)}
                  onCadence={() => setEditing(o)}
                  onToggle={() => toggleEnabled(o)}
                  onDelete={() => setDeleting(o)}
                />
              ))}
            </div>
          )}
        </Card>
      </>
    );
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [orgs, loading, block, pollingOn, canConfig, actionError, t]);

  return (
    <div>
      <PageHeader
        title={t('meraki.name')}
        trail={[
          { label: t('nav:sections.settings') },
          { label: t('nav:settings.integrations'), to: '/settings/integrations' },
          { label: t('meraki.name') },
        ]}
        note={t('meraki.note')}
      />
      {content}

      {adding && <AddOrgModal onClose={() => setAdding(false)} onSaved={load} />}
      {scoping && (
        <NetworksModal
          org={scoping}
          onClose={() => setScoping(null)}
          onSaved={() => setScoping(null)}
        />
      )}
      {editing && (
        <CadenceModal org={editing} onClose={() => setEditing(null)} onSaved={load} />
      )}
      {deleting && (
        // The shared dialog, not a hand-built one: this used to close itself *before* the request
        // and drop the rejection, so a delete the server refused looked like one that worked until
        // the organization was still there on the next load.
        <ConfirmDeleteModal
          title={t('meraki.delete.title')}
          onConfirm={() => api.deleteMerakiOrg(deleting.id)}
          errorFallback={t('meraki.err.delete')}
          onClose={() => setDeleting(null)}
          onDone={() => {
            setDeleting(null);
            load();
          }}
        >
          <Trans
            t={t}
            i18nKey="meraki.delete.confirmText"
            values={{ name: deleting.name }}
            components={{ b: <strong /> }}
          />
        </ConfirmDeleteModal>
      )}
    </div>
  );
}
