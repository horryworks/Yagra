// Discovery (Nodes ▸ Discovery). Sweep a subnet for live + SNMP-speaking devices, review the
// candidates (classified into a suggested profile from sysDescr), and import the chosen ones as
// nodes. The sweep runs on the poller (raw-socket ICMP); core correlates results by scan id.

import { useCallback, useEffect, useRef, useState } from 'react';
import { api, ApiError } from '../services/api';
import { useAuthStore } from '../store';
import type { CredentialSummary, DiscoveryCandidate, ProfileSummary } from '../types/api';
import { expandCidr } from '../lib/cidr';
import { PageHeader } from '../components/ui/PageHeader';
import { Card } from '../components/ui/Card';
import { Button } from '../components/ui/Button';
import { TextInput, Select } from '../components/ui/Field';
import { Badge } from '../components/ui/Badge';
import './DiscoveryPage.css';

const errMsg = (e: unknown, fallback: string) =>
  e instanceof ApiError ? e.message : fallback;

interface RowState {
  selected: boolean;
  name: string;
  profile_id: string;
  credential_id: string;
}

export function DiscoveryPage() {
  const authed = useAuthStore((s) => s.authed);
  const [cidr, setCidr] = useState('192.168.1.0/24');
  const [communities, setCommunities] = useState('public');
  const [scanId, setScanId] = useState<string | null>(null);
  const [done, setDone] = useState(false);
  const [candidates, setCandidates] = useState<DiscoveryCandidate[]>([]);
  const [rowState, setRowState] = useState<Record<string, RowState>>({});
  const [profiles, setProfiles] = useState<ProfileSummary[]>([]);
  const [creds, setCreds] = useState<CredentialSummary[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [note, setNote] = useState<string | null>(null);
  const pollRef = useRef<ReturnType<typeof setInterval> | null>(null);

  useEffect(() => {
    api.listProfiles().then(setProfiles).catch(() => undefined);
    api.listCredentials().then(setCreds).catch(() => undefined);
    return () => {
      if (pollRef.current) clearInterval(pollRef.current);
    };
  }, []);

  // Seed per-row form state when new candidates arrive (suggested profile preselected by name).
  const seedRows = useCallback(
    (list: DiscoveryCandidate[]) => {
      setRowState((cur) => {
        const next = { ...cur };
        for (const c of list) {
          if (next[c.address]) continue;
          const prof = profiles.find((p) => p.name === c.suggested_profile);
          next[c.address] = {
            selected: false,
            name: c.sysname?.trim() || c.address,
            profile_id: prof?.id ?? '',
            credential_id: '',
          };
        }
        return next;
      });
    },
    [profiles],
  );

  const startScan = () => {
    setError(null);
    setNote(null);
    const targets = expandCidr(cidr);
    if (targets.length === 0) {
      setError('Enter a single IP or an IPv4 CIDR of /22 or smaller (≤1024 hosts).');
      return;
    }
    const comms = communities
      .split(',')
      .map((c) => c.trim())
      .filter(Boolean);
    setCandidates([]);
    setRowState({});
    setDone(false);
    api
      .startDiscoveryScan({ targets, communities: comms })
      .then(({ scan_id }) => {
        setScanId(scan_id);
        setNote(`Scanning ${targets.length} addresses…`);
        if (pollRef.current) clearInterval(pollRef.current);
        pollRef.current = setInterval(() => poll(scan_id), 2000);
        poll(scan_id);
      })
      .catch((e: unknown) => setError(errMsg(e, 'failed to start scan')));
  };

  const poll = (id: string) => {
    api
      .getDiscoveryScan(id)
      .then((s) => {
        setCandidates(s.candidates);
        seedRows(s.candidates);
        if (s.done) {
          setDone(true);
          setNote(`Scan complete — ${s.candidates.length} device(s) found.`);
          if (pollRef.current) {
            clearInterval(pollRef.current);
            pollRef.current = null;
          }
        }
      })
      .catch(() => undefined);
  };

  const patchRow = (addr: string, patch: Partial<RowState>) =>
    setRowState((cur) => ({ ...cur, [addr]: { ...cur[addr], ...patch } }));

  const importSelected = () => {
    setError(null);
    const nodes = candidates
      .filter((c) => rowState[c.address]?.selected)
      .map((c) => {
        const r = rowState[c.address];
        return {
          address: c.address,
          name: r.name.trim() || c.address,
          profile_id: r.profile_id || undefined,
          credential_id: r.credential_id || undefined,
        };
      });
    if (nodes.length === 0) {
      setError('Select at least one device to import.');
      return;
    }
    api
      .importDiscovered(nodes)
      .then(({ created }) => {
        setNote(`Imported ${created} node(s). They will start polling shortly.`);
        // Clear the imported rows' selection.
        setRowState((cur) => {
          const next = { ...cur };
          for (const n of nodes) if (next[n.address]) next[n.address].selected = false;
          return next;
        });
      })
      .catch((e: unknown) => setError(errMsg(e, 'failed to import')));
  };

  const selectedCount = candidates.filter((c) => rowState[c.address]?.selected).length;

  return (
    <div>
      <PageHeader
        title="Discovery"
        trail={[{ label: 'Nodes' }, { label: 'Discovery' }]}
        note="Sweep a subnet for live + SNMP devices, then import the ones you want."
      />

      <Card title="Scan a subnet">
        {authed ? (
          <div className="disco-form form-row">
            <TextInput
              className="mono"
              placeholder="CIDR or IP (e.g. 192.168.1.0/24)"
              value={cidr}
              onChange={(e) => setCidr(e.target.value)}
            />
            <TextInput
              className="mono"
              placeholder="SNMP communities (comma-separated)"
              value={communities}
              onChange={(e) => setCommunities(e.target.value)}
            />
            <Button variant="primary" onClick={startScan} disabled={!!scanId && !done}>
              {scanId && !done ? 'Scanning…' : 'Scan'}
            </Button>
          </div>
        ) : (
          <p className="muted">Sign in as an admin to run discovery.</p>
        )}
        {error && <p className="form-error">{error}</p>}
        {note && <p className="muted">{note}</p>}
      </Card>

      {candidates.length > 0 && (
        <Card title="Discovered devices" className="disco-results-card">
          <div className="disco-table">
            <div className="disco-head">
              <div className="disco-h" />
              <div className="disco-h">Address</div>
              <div className="disco-h">Identity</div>
              <div className="disco-h">Name</div>
              <div className="disco-h">Profile</div>
              <div className="disco-h">Credential</div>
            </div>
            {candidates.map((c) => {
              const r = rowState[c.address];
              if (!r) return null;
              return (
                <div className="disco-row" key={c.address}>
                  <input
                    type="checkbox"
                    checked={r.selected}
                    onChange={(e) => patchRow(c.address, { selected: e.target.checked })}
                  />
                  <span className="mono">
                    {c.address}{' '}
                    {c.reachable ? (
                      <Badge tone="up">ping</Badge>
                    ) : (
                      <span className="muted">no ping</span>
                    )}
                  </span>
                  <span className="disco-identity">
                    {c.sysname && <span className="disco-sysname">{c.sysname}</span>}
                    {c.sysdescr ? (
                      <span className="muted disco-sysdescr" title={c.sysdescr}>
                        {c.sysdescr}
                      </span>
                    ) : (
                      <span className="muted">no SNMP</span>
                    )}
                  </span>
                  <TextInput
                    value={r.name}
                    onChange={(e) => patchRow(c.address, { name: e.target.value })}
                  />
                  <Select
                    value={r.profile_id}
                    onChange={(e) => patchRow(c.address, { profile_id: e.target.value })}
                  >
                    <option value="">(none)</option>
                    {profiles.map((p) => (
                      <option key={p.id} value={p.id}>
                        {p.name}
                      </option>
                    ))}
                  </Select>
                  <Select
                    value={r.credential_id}
                    onChange={(e) => patchRow(c.address, { credential_id: e.target.value })}
                  >
                    <option value="">(none)</option>
                    {creds.map((cr) => (
                      <option key={cr.id} value={cr.id}>
                        {cr.name}
                      </option>
                    ))}
                  </Select>
                </div>
              );
            })}
          </div>
          {authed && (
            <div className="disco-import">
              <Button variant="primary" onClick={importSelected} disabled={selectedCount === 0}>
                Import {selectedCount > 0 ? `${selectedCount} ` : ''}selected
              </Button>
            </div>
          )}
        </Card>
      )}
    </div>
  );
}
