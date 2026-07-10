// Meraki import wizard: enumerate an org's networks + devices (read-only), let the operator pick
// which devices to monitor (grouped by network), then import the selection as nodes. The chosen
// devices' networks are set in scope; only imported serials are ever polled (scope at fan-out).

import { useEffect, useMemo, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { api, ApiError } from '../../services/api';
import type { MerakiCandidate, MerakiOrg } from '../../types/api';
import { Modal } from '../ui/Modal';
import { Button } from '../ui/Button';
import './MerakiImportModal.css';

const errMsg = (e: unknown, fallback: string) =>
  e instanceof ApiError ? e.message : fallback;

export function MerakiImportModal({
  org,
  onClose,
  onImported,
}: {
  org: MerakiOrg;
  onClose: () => void;
  onImported: () => void;
}) {
  const { t } = useTranslation('system');
  const [candidates, setCandidates] = useState<MerakiCandidate[] | null>(null);
  const [selected, setSelected] = useState<Set<string>>(new Set());
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    setBusy(true);
    api
      .enumerateMerakiOrg(org.id)
      .then((res) => {
        setCandidates(res.devices);
        setSelected(new Set(res.devices.map((d) => d.serial)));
      })
      .catch((e: unknown) => setError(errMsg(e, t('meraki.import.err.enumerate'))))
      .finally(() => setBusy(false));
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [org.id]);

  // Group candidates by network for a scannable, per-site layout.
  const byNetwork = useMemo(() => {
    const groups = new Map<string, { name: string; devices: MerakiCandidate[] }>();
    for (const d of candidates ?? []) {
      const g = groups.get(d.network_id) ?? { name: d.network_name, devices: [] };
      g.devices.push(d);
      groups.set(d.network_id, g);
    }
    return [...groups.entries()].sort((a, b) => a[1].name.localeCompare(b[1].name));
  }, [candidates]);

  const toggle = (serial: string) =>
    setSelected((s) => {
      const next = new Set(s);
      if (next.has(serial)) next.delete(serial);
      else next.add(serial);
      return next;
    });

  const toggleNetwork = (netId: string, on: boolean) =>
    setSelected((s) => {
      const next = new Set(s);
      for (const d of candidates ?? []) {
        if (d.network_id === netId) {
          if (on) next.add(d.serial);
          else next.delete(d.serial);
        }
      }
      return next;
    });

  const doImport = () => {
    if (!candidates) return;
    const devices = candidates.filter((d) => selected.has(d.serial));
    const monitored_network_ids = [...new Set(devices.map((d) => d.network_id))];
    setBusy(true);
    setError(null);
    api
      .importMerakiDevices({ org_uuid: org.id, monitored_network_ids, devices })
      .then(() => onImported())
      .catch((e: unknown) => {
        setError(errMsg(e, t('meraki.import.err.import')));
        setBusy(false);
      });
  };

  return (
    <Modal
      title={t('meraki.import.title', { name: org.name })}
      onClose={onClose}
      footer={
        <>
          <Button variant="outline" onClick={onClose} disabled={busy}>
            {t('common:actions.cancel')}
          </Button>
          <Button
            variant="primary"
            onClick={doImport}
            disabled={busy || selected.size === 0 || !candidates}
          >
            {t('meraki.import.importBtn', { count: selected.size })}
          </Button>
        </>
      }
    >
      {candidates === null ? (
        <p className="muted">{busy ? t('meraki.import.enumerating') : t('meraki.import.noData')}</p>
      ) : candidates.length === 0 ? (
        <p className="muted">{t('meraki.import.allImported')}</p>
      ) : (
        <div className="meraki-import-list">
          {byNetwork.map(([netId, g]) => {
            const total = g.devices.length;
            const chosen = g.devices.filter((d) => selected.has(d.serial)).length;
            return (
              <div className="meraki-import-net" key={netId}>
                <label className="meraki-import-net-head">
                  <input
                    type="checkbox"
                    checked={chosen === total}
                    ref={(el) => {
                      if (el) el.indeterminate = chosen > 0 && chosen < total;
                    }}
                    onChange={(e) => toggleNetwork(netId, e.target.checked)}
                  />
                  <span className="meraki-import-net-name">{g.name}</span>
                  <span className="meraki-import-net-count">
                    {chosen}/{total}
                  </span>
                </label>
                {g.devices.map((d) => (
                  <label className="meraki-import-dev" key={d.serial}>
                    <input
                      type="checkbox"
                      checked={selected.has(d.serial)}
                      onChange={() => toggle(d.serial)}
                    />
                    <span className="meraki-import-dev-name">{d.name || d.serial}</span>
                    <span className="meraki-import-dev-meta mono">
                      {d.product_type}
                      {d.model ? ` · ${d.model}` : ''}
                    </span>
                    <span className="meraki-import-dev-ip mono">{d.lan_ip ?? ''}</span>
                  </label>
                ))}
              </div>
            );
          })}
        </div>
      )}
      {error && <p className="form-error">{error}</p>}
    </Modal>
  );
}
