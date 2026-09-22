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
// The API key is entered inline, and the backend seals it into the credentials store as a
// `meraki_api` credential — or, since ADR-164 Inc.6, the dialog names one that is stored already,
// so the organizations a key can see share it instead of each batch sealing the same secret again.
// Everything here is read-only toward Meraki — nothing this page does can change a customer's
// Meraki configuration.

import { useCallback, useEffect, useMemo, useState } from 'react';
import { Trans, useTranslation } from 'react-i18next';
import { Link, useNavigate } from 'react-router-dom';
import { api, errMsg } from '../../services/api';
import { useCan } from '../../store';
import type {
  CredentialSummary,
  MerakiNetwork,
  MerakiOrg,
  MerakiOrgOption,
} from '../../types/api';
import { PageHeader } from '../../components/ui/PageHeader';
import { Card } from '../../components/ui/Card';
import { Button } from '../../components/ui/Button';
import { Modal } from '../../components/ui/Modal';
import { ConfirmDeleteModal } from '../../components/ui/ConfirmDeleteModal';
import { TextInput, Select } from '../../components/ui/Field';
import { OPTIONAL_MERAKI_TIERS, tierList, tiersToSave } from '../merakiTiers';
import './MerakiIntegrationPage.css';
import { classifyLoadError, type LoadBlock } from '../../lib/loadState';
import { LoadBlockNotice } from '../../components/ui/LoadBlockNotice';
import { DEFAULT_MERAKI_BASE_URL, MERAKI_REGIONS } from './merakiRegions';
import {
  MERAKI_CADENCE_BOUNDS,
  cadenceRange,
  type CadenceBounds,
  type MerakiCadenceField,
} from './merakiCadence';
import {
  allAlreadyAdded,
  keyFields,
  keyNameFor,
  regionForSavedKey,
  savedMerakiKeys,
  selectableOrgIds,
  unlistedRegion,
  type KeySourceKind,
} from './merakiAddOrg';
import { canSyncNow, merakiOrgPath } from './merakiOrgRow';
import { MerakiSyncButton, MerakiSyncStatus } from './MerakiSyncStatus';
import { useMerakiSync } from './useMerakiSync';

/** Add one or more organizations under a shared read-only API key (discover → multi-select).
 *
 *  The key is typed, or — since ADR-164 Inc.6 — one already stored: a key that can see five
 *  organizations used to be sealed again for each batch added later, leaving several credentials
 *  that were the same secret. `savedKeys` is empty for a caller who may not read credentials and
 *  for a deployment with no Meraki key yet, and the dialog then has no choice to draw.
 *
 *  Which field a request carries, which region follows a key and which discovered rows can be
 *  ticked are all decided in `merakiAddOrg.ts`, where a test reaches them. */
function AddOrgModal({
  savedKeys,
  existing,
  onClose,
  onSaved,
}: {
  savedKeys: CredentialSummary[];
  existing: MerakiOrg[];
  onClose: () => void;
  onSaved: () => void;
}) {
  const { t } = useTranslation('system');
  const [source, setSource] = useState<KeySourceKind>('typed');
  const [apiKey, setApiKey] = useState('');
  const [credentialId, setCredentialId] = useState('');
  const [baseUrl, setBaseUrl] = useState<string>(DEFAULT_MERAKI_BASE_URL);
  const [orgs, setOrgs] = useState<MerakiOrgOption[] | null>(null);
  const [selected, setSelected] = useState<Set<string>>(new Set());
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  // The one key field both requests carry. Spread into each body rather than read from the two
  // pieces of state, so a key typed before switching to a saved one cannot ride along.
  const key = keyFields(source, apiKey, credentialId);
  const extraRegion = unlistedRegion(baseUrl);

  // A stored key belongs to one Meraki cloud, so choosing one moves the region to where that key
  // is already used. Only on the choice: the operator can still change the region afterwards.
  const pickSavedKey = (id: string) => {
    setCredentialId(id);
    setBaseUrl((current) => regionForSavedKey(id, existing, current));
  };

  const pickSource = (next: KeySourceKind) => {
    setSource(next);
    // Preselect the first stored key, so "Use a saved key" is usable as soon as it is chosen.
    if (next === 'saved' && !credentialId && savedKeys.length > 0) pickSavedKey(savedKeys[0].id);
  };

  const discover = () => {
    if (!key) return;
    setBusy(true);
    setError(null);
    api
      .merakiDiscover({ ...key, base_url: baseUrl })
      .then((list) => {
        setOrgs(list);
        setSelected(new Set(selectableOrgIds(list)));
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
    if (!key) return;
    setBusy(true);
    setError(null);
    api
      .createMerakiOrgs({ ...key, base_url: baseUrl, org_ids: [...selected] })
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
            <Button variant="primary" onClick={discover} disabled={key === null || busy}>
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
              {/* A stored key can lead to a base URL this list does not offer (an organization
                  added through the API). Without its own option the select would show its first
                  one while the request carried this. */}
              {extraRegion && <option value={extraRegion}>{extraRegion}</option>}
            </Select>
          </div>
          {/* Drawn only when there is a stored key to choose: with none, the dialog is the one
              box it always was. */}
          {savedKeys.length > 0 && (
            <div
              className="meraki-source-row"
              role="radiogroup"
              aria-label={t('meraki.addOrg.apiKeyLabel')}
            >
              <label className="meraki-source-choice">
                <input
                  type="radio"
                  name="meraki-key-source"
                  checked={source === 'typed'}
                  onChange={() => pickSource('typed')}
                />
                <span>{t('meraki.addOrg.sourceTyped')}</span>
              </label>
              <label className="meraki-source-choice">
                <input
                  type="radio"
                  name="meraki-key-source"
                  checked={source === 'saved'}
                  onChange={() => pickSource('saved')}
                />
                <span>{t('meraki.addOrg.sourceSaved')}</span>
              </label>
            </div>
          )}
          {source === 'saved' ? (
            <div className="modal-field">
              <label className="modal-field-label">{t('meraki.addOrg.savedKeyLabel')}</label>
              <Select value={credentialId} onChange={(e) => pickSavedKey(e.target.value)}>
                {savedKeys.map((c) => (
                  <option key={c.id} value={c.id}>
                    {c.name}
                  </option>
                ))}
              </Select>
              <span className="modal-hint">{t('meraki.addOrg.savedKeyHint')}</span>
            </div>
          ) : (
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
          )}
        </>
      ) : (
        <>
          {/* Nothing left to tick: say so, rather than leave a list of dead checkboxes over a
              disabled "Add 0 organizations" to be worked out. */}
          <p className="modal-hint">
            {allAlreadyAdded(orgs) ? t('meraki.addOrg.allAdded') : t('meraki.addOrg.selectHint')}
          </p>
          <div className="meraki-org-picker">
            {orgs.length === 0 && <p className="muted">{t('meraki.addOrg.noOrgs')}</p>}
            {orgs.map((o) => (
              <label className={`meraki-check-row${o.already_added ? ' added' : ''}`} key={o.id}>
                <input
                  type="checkbox"
                  checked={selected.has(o.id)}
                  disabled={o.already_added}
                  onChange={() => toggle(o.id)}
                />
                <span className="meraki-check-name">{o.name || o.id}</span>
                <span className="meraki-check-sub mono">{o.id}</span>
                {o.already_added && (
                  <span className="meraki-check-tag">{t('meraki.addOrg.alreadyAdded')}</span>
                )}
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
  const [switchPorts, setSwitchPorts] = useState(org.switch_ports_secs);
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
        switch_ports_secs: switchPorts,
        enabled_tiers: tiersToSave(tiers),
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

  const numField = (
    label: string,
    value: number,
    set: (n: number) => void,
    hint: string,
    bounds?: CadenceBounds,
  ) => (
    <div className="modal-field">
      <label className="modal-field-label">{label}</label>
      <TextInput
        type="number"
        min={bounds?.min}
        max={bounds?.max}
        value={value}
        onChange={(e) => set(Number(e.target.value))}
      />
      <span className="modal-hint">{hint}</span>
    </div>
  );
  // The range each interval accepts comes from `merakiCadence.ts`, which a Rust test holds to the
  // server's own bounds — the hint and the input's min/max are the same two numbers.
  const intervalField = (
    field: MerakiCadenceField,
    label: string,
    value: number,
    set: (n: number) => void,
  ) => numField(label, value, set, cadenceRange(field), MERAKI_CADENCE_BOUNDS[field]);

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
          {OPTIONAL_MERAKI_TIERS.map((tier) => (
            <label className="meraki-chip-check" key={tier}>
              <input type="checkbox" checked={tiers.has(tier)} onChange={() => toggleTier(tier)} />
              <span>{t(`meraki.tier.${tier}`)}</span>
            </label>
          ))}
        </div>
        {/* Availability has no checkbox: it is the one tier that says whether a device is up, and
            the server refuses a cadence without it (決定 17). The sentence is why it is missing. */}
        <span className="modal-hint">{t('meraki.cadence.availabilityAlways')}</span>
      </div>
      {intervalField(
        'availability',
        t('meraki.cadence.availabilityInterval'),
        availability,
        setAvailability,
      )}
      {intervalField('uplink', t('meraki.cadence.uplinkInterval'), uplink, setUplink)}
      {intervalField(
        'switch_ports',
        t('meraki.cadence.switchPortsInterval'),
        switchPorts,
        setSwitchPorts,
      )}
      {intervalField('traffic', t('meraki.cadence.trafficInterval'), traffic, setTraffic)}
      {intervalField('inventory', t('meraki.cadence.inventoryInterval'), inventory, setInventory)}
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
  creds,
  canConfig,
  pollingOn,
  onSynced,
  onNetworks,
  onCadence,
  onToggle,
  onDelete,
}: {
  org: MerakiOrg;
  /** The stored credentials, or `null` when this caller may not read them. */
  creds: CredentialSummary[] | null;
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
  // Which stored key this organization is polled with. Several organizations can share one since
  // ADR-164 Inc.6, so the name is what tells an operator which rows a key's rotation reaches.
  const keyName = keyNameFor(org, creds);

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
          {keyName !== null && (
            <span className="meraki-org-key">{t('meraki.orgs.key', { name: keyName })}</span>
          )}
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
  // Reading the credential list takes its own permission. Without it the page is whole: the rows
  // leave out which key they use, and the add dialog offers only the box to type one into.
  const canCredentials = useCan('manage_credentials');
  const [orgs, setOrgs] = useState<MerakiOrg[]>([]);
  const [creds, setCreds] = useState<CredentialSummary[] | null>(null);
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

  // Apart from `load` on purpose. Joined to its `Promise.all`, a credentials read that failed
  // would block the whole page over an annotation — so a failure here only means "no names".
  const loadCreds = useCallback(() => {
    if (!canCredentials) return;
    api
      .listCredentials()
      .then(setCreds)
      .catch(() => setCreds(null));
  }, [canCredentials]);

  useEffect(() => {
    loadCreds();
  }, [loadCreds]);

  // After an add: a typed key was sealed as a new credential, so the names are stale as well.
  const reload = useCallback(() => {
    load();
    loadCreds();
  }, [load, loadCreds]);

  const savedKeys = useMemo(() => (creds ? savedMerakiKeys(creds) : []), [creds]);

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
                  creds={creds}
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
  }, [orgs, creds, loading, block, pollingOn, canConfig, actionError, t]);

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

      {adding && (
        <AddOrgModal
          savedKeys={savedKeys}
          existing={orgs}
          onClose={() => setAdding(false)}
          onSaved={reload}
        />
      )}
      {scoping && (
        <NetworksModal
          org={scoping}
          onClose={() => setScoping(null)}
          // Reload, not only close: the row's "N not collected" is counted from which networks are
          // watched, so un-watching one that holds nodes is exactly when it has to change (決定 15).
          onSaved={load}
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
