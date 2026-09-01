// SPDX-License-Identifier: AGPL-3.0-only
// Settings ▸ Integrations ▸ Cisco Meraki. The detail page for the Meraki integration (reached from
// the Integrations catalog). Onboard Meraki organizations (read-only Dashboard API), manage per-org
// enable/cadence/scope, launch the import wizard, and toggle the global kill switch.
//
// The API key is entered inline (it belongs to one org set, unlike a shared SNMP community); the
// backend seals it into the credentials store as a `meraki_api` credential. Everything here is
// read-only toward Meraki — nothing this page does can change a customer's Meraki configuration.

import { useCallback, useEffect, useMemo, useState } from 'react';
import { Trans, useTranslation } from 'react-i18next';
import { api, errMsg } from '../../services/api';
import { useCan } from '../../store';
import type { MerakiNetwork, MerakiOrg, MerakiOrgOption } from '../../types/api';
import { PageHeader } from '../../components/ui/PageHeader';
import { Card } from '../../components/ui/Card';
import { Button } from '../../components/ui/Button';
import { Modal } from '../../components/ui/Modal';
import { TextInput, Select } from '../../components/ui/Field';
import { MerakiImportModal } from '../../components/MerakiImport/MerakiImportModal';
import { SELECTABLE_MERAKI_TIERS } from '../merakiTiers';
import './MerakiIntegrationPage.css';
import { classifyLoadError, type LoadBlock } from '../../lib/loadState';
import { LoadBlockNotice } from '../../components/ui/LoadBlockNotice';
import { tierList } from '../merakiTiers';

// The visible region name is a translation key; the base_url (the technical Meraki endpoint) is not.
const REGIONS: { labelKey: string; base_url: string }[] = [
  { labelKey: 'meraki.regions.global', base_url: 'https://api.meraki.com' },
  { labelKey: 'meraki.regions.canada', base_url: 'https://api.meraki.ca' },
  { labelKey: 'meraki.regions.china', base_url: 'https://api.meraki.cn' },
  { labelKey: 'meraki.regions.usGov', base_url: 'https://api.gov-meraki.com' },
];

/** Add one or more organizations under a shared read-only API key (discover → multi-select). */
function AddOrgModal({ onClose, onSaved }: { onClose: () => void; onSaved: () => void }) {
  const { t } = useTranslation('system');
  const [apiKey, setApiKey] = useState('');
  const [baseUrl, setBaseUrl] = useState(REGIONS[0].base_url);
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
              {REGIONS.map((r) => (
                <option key={r.base_url} value={r.base_url}>
                  {t(r.labelKey)}
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
      {numField(t('meraki.cadence.inventoryInterval'), inventory, setInventory, '900–604800')}
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

export function MerakiIntegrationPage() {
  const { t } = useTranslation('system');
  const canConfig = useCan('manage_config');
  const [orgs, setOrgs] = useState<MerakiOrg[]>([]);
  const [loading, setLoading] = useState(true);
  const [block, setBlock] = useState<LoadBlock | null>(null);
  const [pollingOn, setPollingOn] = useState(true);
  const [adding, setAdding] = useState(false);
  const [importing, setImporting] = useState<MerakiOrg | null>(null);
  const [editing, setEditing] = useState<MerakiOrg | null>(null);
  const [scoping, setScoping] = useState<MerakiOrg | null>(null);
  const [deleting, setDeleting] = useState<MerakiOrg | null>(null);

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
    api.setMerakiPolling(next).catch(() => setPollingOn(!next));
  };

  const toggleEnabled = (org: MerakiOrg) => {
    api
      .setMerakiOrgEnabled(org.id, !org.enabled)
      .then(load)
      .catch(() => undefined);
  };

  const content = useMemo(() => {
    if (block) {
      return <LoadBlockNotice block={block} unavailable={t('integrations.unavailable')} />;
    }
    return (
      <>
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
                <div className="meraki-org" key={o.id}>
                  <div className="meraki-org-main">
                    <span className="meraki-org-name">{o.name}</span>
                    <span className="meraki-org-id mono">{t('meraki.orgs.orgId', { id: o.org_id })}</span>
                    <span
                      className={`meraki-org-state ${o.enabled ? 'on' : 'off'}`}
                    >
                      {o.enabled ? t('meraki.orgs.stateEnabled') : t('meraki.orgs.statePaused')}
                    </span>
                    <span className="meraki-org-tiers">
                      {t('meraki.tiersPrefix')} {tierList(o.enabled_tiers, t)}
                    </span>
                  </div>
                  {canConfig && (
                    <div className="meraki-org-actions">
                      <Button variant="outline" onClick={() => setImporting(o)}>
                        {t('meraki.org.import')}
                      </Button>
                      <Button variant="outline" onClick={() => setScoping(o)}>
                        {t('meraki.org.networks')}
                      </Button>
                      <Button variant="outline" onClick={() => setEditing(o)}>
                        {t('meraki.org.cadence')}
                      </Button>
                      <Button variant="outline" onClick={() => toggleEnabled(o)}>
                        {o.enabled ? t('meraki.org.pause') : t('meraki.org.resume')}
                      </Button>
                      <Button variant="danger" onClick={() => setDeleting(o)}>
                        {t('common:actions.delete')}
                      </Button>
                    </div>
                  )}
                </div>
              ))}
            </div>
          )}
        </Card>
      </>
    );
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [orgs, loading, block, pollingOn, canConfig, t]);

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
      {importing && (
        <MerakiImportModal
          org={importing}
          onClose={() => setImporting(null)}
          onImported={() => {
            setImporting(null);
            load();
          }}
        />
      )}
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
        <Modal
          title={t('meraki.delete.title')}
          onClose={() => setDeleting(null)}
          footer={
            <>
              <Button variant="outline" onClick={() => setDeleting(null)}>
                {t('common:actions.cancel')}
              </Button>
              <Button
                variant="danger"
                onClick={() => {
                  const id = deleting.id;
                  setDeleting(null);
                  api.deleteMerakiOrg(id).then(load).catch(() => undefined);
                }}
              >
                {t('common:actions.delete')}
              </Button>
            </>
          }
        >
          <p className="modal-confirm-text">
            <Trans
              t={t}
              i18nKey="meraki.delete.confirmText"
              values={{ name: deleting.name }}
              components={{ b: <strong /> }}
            />
          </p>
        </Modal>
      )}
    </div>
  );
}
