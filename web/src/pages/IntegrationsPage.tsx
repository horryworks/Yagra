// Settings ▸ Integrations ▸ Cisco Meraki. Onboard Meraki organizations (read-only Dashboard API),
// manage per-org enable/cadence/scope, launch the import wizard, and toggle the global kill switch.
//
// The API key is entered inline (it belongs to one org set, unlike a shared SNMP community); the
// backend seals it into the credentials store as a `meraki_api` credential. Everything here is
// read-only toward Meraki — nothing this page does can change a customer's Meraki configuration.

import { useCallback, useEffect, useMemo, useState } from 'react';
import { api, ApiError } from '../services/api';
import { useAuthStore } from '../store';
import type { MerakiNetwork, MerakiOrg, MerakiOrgOption } from '../types/api';
import { PageHeader } from '../components/ui/PageHeader';
import { Card } from '../components/ui/Card';
import { Button } from '../components/ui/Button';
import { Modal } from '../components/ui/Modal';
import { TextInput, Select } from '../components/ui/Field';
import { MerakiImportModal } from '../components/MerakiImport/MerakiImportModal';
import './IntegrationsPage.css';

const REGIONS: { label: string; base_url: string }[] = [
  { label: 'Global (api.meraki.com)', base_url: 'https://api.meraki.com' },
  { label: 'Canada (api.meraki.ca)', base_url: 'https://api.meraki.ca' },
  { label: 'China (api.meraki.cn)', base_url: 'https://api.meraki.cn' },
  { label: 'US Gov (api.gov-meraki.com)', base_url: 'https://api.gov-meraki.com' },
];

const errMsg = (e: unknown, fallback: string) =>
  e instanceof ApiError ? e.message : fallback;

const tierList = (tiers: string[]) => (tiers.length ? tiers.join(', ') : 'none');

/** Add one or more organizations under a shared read-only API key (discover → multi-select). */
function AddOrgModal({ onClose, onSaved }: { onClose: () => void; onSaved: () => void }) {
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
      .catch((e: unknown) => setError(errMsg(e, 'failed to reach the Meraki API')))
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
        setError(errMsg(e, 'failed to add organizations'));
        setBusy(false);
      });
  };

  return (
    <Modal
      title="Add Meraki organization"
      onClose={onClose}
      footer={
        <>
          <Button variant="outline" onClick={onClose} disabled={busy}>
            Cancel
          </Button>
          {orgs === null ? (
            <Button variant="primary" onClick={discover} disabled={!apiKey.trim() || busy}>
              Find organizations
            </Button>
          ) : (
            <Button variant="primary" onClick={create} disabled={selected.size === 0 || busy}>
              Add {selected.size} organization{selected.size === 1 ? '' : 's'}
            </Button>
          )}
        </>
      }
    >
      {orgs === null ? (
        <>
          <div className="modal-field">
            <label className="modal-field-label">Region</label>
            <Select value={baseUrl} onChange={(e) => setBaseUrl(e.target.value)}>
              {REGIONS.map((r) => (
                <option key={r.base_url} value={r.base_url}>
                  {r.label}
                </option>
              ))}
            </Select>
          </div>
          <div className="modal-field">
            <label className="modal-field-label">API key (read-only)</label>
            <TextInput
              className="mono"
              type="password"
              placeholder="Meraki Dashboard API key"
              value={apiKey}
              onChange={(e) => setApiKey(e.target.value)}
              autoComplete="new-password"
              autoFocus
            />
            <span className="modal-hint">
              Use a <strong>read-only</strong> Dashboard API key. It is sealed (encrypted at rest)
              and never shown again. Yagra only ever issues read-only GET calls to Meraki.
            </span>
          </div>
        </>
      ) : (
        <>
          <p className="modal-hint">
            Select the organizations to monitor. All share this one API key.
          </p>
          <div className="meraki-org-picker">
            {orgs.length === 0 && <p className="muted">This key can't access any organizations.</p>}
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
      .catch((e: unknown) => setError(errMsg(e, 'failed to load networks')));
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
        setError(errMsg(e, 'failed to save network scope'));
        setBusy(false);
      });
  };

  return (
    <Modal
      title={`Network scope — ${org.name}`}
      onClose={onClose}
      footer={
        <>
          <Button variant="outline" onClick={onClose} disabled={busy}>
            Cancel
          </Button>
          <Button variant="primary" onClick={save} disabled={busy || !networks}>
            Save
          </Button>
        </>
      }
    >
      <p className="modal-hint">
        Only monitored networks are collected. Un-checking a network stops narrowing API calls to it
        (existing device nodes are untouched — remove them to stop monitoring a device).
      </p>
      {networks === null ? (
        <p className="muted">Loading…</p>
      ) : networks.length === 0 ? (
        <p className="muted">No networks yet — run “Import devices” to enumerate the org first.</p>
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
  const [availability, setAvailability] = useState(org.availability_secs);
  const [uplink, setUplink] = useState(org.uplink_secs);
  const [traffic, setTraffic] = useState(org.traffic_secs);
  const [inventory, setInventory] = useState(org.inventory_secs);
  const [tiers, setTiers] = useState<Set<string>>(new Set(org.enabled_tiers));
  const [targetRps, setTargetRps] = useState(org.target_rps);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const toggleTier = (t: string) =>
    setTiers((s) => {
      const next = new Set(s);
      if (next.has(t)) next.delete(t);
      else next.add(t);
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
        setError(errMsg(e, 'failed to save cadence'));
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
      title={`Cadence — ${org.name}`}
      onClose={onClose}
      footer={
        <>
          <Button variant="outline" onClick={onClose} disabled={busy}>
            Cancel
          </Button>
          <Button variant="primary" onClick={save} disabled={busy}>
            Save
          </Button>
        </>
      }
    >
      <div className="modal-field">
        <label className="modal-field-label">Enabled tiers</label>
        <div className="meraki-tier-row">
          {(['availability', 'uplink', 'traffic'] as const).map((t) => (
            <label className="meraki-chip-check" key={t}>
              <input type="checkbox" checked={tiers.has(t)} onChange={() => toggleTier(t)} />
              <span>{t}</span>
            </label>
          ))}
        </div>
      </div>
      {numField('Availability interval (s)', availability, setAvailability, '60–3600')}
      {numField('Uplink interval (s)', uplink, setUplink, '60–3600')}
      {numField('Traffic interval (s)', traffic, setTraffic, '300–86400')}
      {numField('Inventory interval (s)', inventory, setInventory, '900–604800')}
      {numField(
        'Rate budget (req/s)',
        targetRps,
        setTargetRps,
        'Conservative — kept well under the org API cap so the customer keeps headroom.',
      )}
      {error && <p className="form-error">{error}</p>}
    </Modal>
  );
}

export function IntegrationsPage() {
  const authed = useAuthStore((s) => s.authed);
  const [orgs, setOrgs] = useState<MerakiOrg[]>([]);
  const [loading, setLoading] = useState(true);
  const [unavailable, setUnavailable] = useState(false);
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
        setUnavailable(false);
      })
      .catch((e: unknown) => {
        if (e instanceof ApiError && e.code === 'admin_unavailable') setUnavailable(true);
      })
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
    if (unavailable) {
      return (
        <Card>
          <p className="muted">Integrations are unavailable in skeleton mode (no store).</p>
        </Card>
      );
    }
    return (
      <>
        <Card title="Meraki polling" className="meraki-killswitch-card">
          <label className="meraki-switch">
            <input
              type="checkbox"
              checked={pollingOn}
              onChange={togglePolling}
              disabled={!authed}
            />
            <span>
              {pollingOn ? 'Polling enabled' : 'Polling paused (kill switch engaged)'}
            </span>
          </label>
          <p className="muted meraki-killswitch-hint">
            Global safeguard — instantly halt all Meraki Dashboard API polling fleet-wide, without
            deleting any organization config.
          </p>
        </Card>

        <Card
          title="Organizations"
          actions={
            authed ? (
              <Button variant="primary" onClick={() => setAdding(true)}>
                + Add organization
              </Button>
            ) : undefined
          }
        >
          {orgs.length === 0 ? (
            <p className="muted">
              {loading ? 'Loading…' : 'No Meraki organizations yet. Add one to start monitoring.'}
            </p>
          ) : (
            <div className="meraki-org-list">
              {orgs.map((o) => (
                <div className="meraki-org" key={o.id}>
                  <div className="meraki-org-main">
                    <span className="meraki-org-name">{o.name}</span>
                    <span className="meraki-org-id mono">org {o.org_id}</span>
                    <span
                      className={`meraki-org-state ${o.enabled ? 'on' : 'off'}`}
                    >
                      {o.enabled ? 'enabled' : 'paused'}
                    </span>
                    <span className="meraki-org-tiers">tiers: {tierList(o.enabled_tiers)}</span>
                  </div>
                  {authed && (
                    <div className="meraki-org-actions">
                      <Button variant="outline" onClick={() => setImporting(o)}>
                        Import devices
                      </Button>
                      <Button variant="outline" onClick={() => setScoping(o)}>
                        Networks
                      </Button>
                      <Button variant="outline" onClick={() => setEditing(o)}>
                        Cadence
                      </Button>
                      <Button variant="outline" onClick={() => toggleEnabled(o)}>
                        {o.enabled ? 'Pause' : 'Resume'}
                      </Button>
                      <Button variant="danger" onClick={() => setDeleting(o)}>
                        Delete
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
  }, [orgs, loading, unavailable, pollingOn, authed]);

  return (
    <div>
      <PageHeader
        title="Integrations"
        trail={[{ label: 'Settings' }, { label: 'Integrations' }]}
        note="Cisco Meraki — read-only Dashboard API monitoring."
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
          title="Delete organization"
          onClose={() => setDeleting(null)}
          footer={
            <>
              <Button variant="outline" onClick={() => setDeleting(null)}>
                Cancel
              </Button>
              <Button
                variant="danger"
                onClick={() => {
                  const id = deleting.id;
                  setDeleting(null);
                  api.deleteMerakiOrg(id).then(load).catch(() => undefined);
                }}
              >
                Delete
              </Button>
            </>
          }
        >
          <p className="modal-confirm-text">
            Delete <strong>{deleting.name}</strong>? Its imported device nodes, network scope, and
            HostTree groups are removed. Metric history in the TSDB is left. This cannot be undone.
          </p>
        </Modal>
      )}
    </div>
  );
}
