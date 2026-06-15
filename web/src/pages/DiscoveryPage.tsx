// Discovery (Nodes ▸ Discovery). Sweep a subnet for live + SNMP-speaking devices, review the
// candidates (classified into a suggested profile from sysDescr), and import the chosen ones as
// nodes. The sweep runs on the poller (raw-socket ICMP); core correlates results by scan id.
// Stored credentials (v2c/v3) are selectable as scan candidates; the one that answers is
// preselected on the row so import binds it automatically.

import { useCallback, useEffect, useRef, useState } from 'react';
import { Link } from 'react-router-dom';
import { api, ApiError } from '../services/api';
import { useAuthStore } from '../store';
import type { CredentialSummary, DiscoveryCandidate, ProfileSummary } from '../types/api';
import { expandTargets } from '../lib/cidr';
import { PageHeader } from '../components/ui/PageHeader';
import { Card } from '../components/ui/Card';
import { Button } from '../components/ui/Button';
import { TextInput, Select } from '../components/ui/Field';
import { Badge } from '../components/ui/Badge';
import { CredentialPicker } from '../components/ui/CredentialPicker';
import './DiscoveryPage.css';

const errMsg = (e: unknown, fallback: string) =>
  e instanceof ApiError ? e.message : fallback;

/** Credential kinds that make sense as SNMP scan candidates. */
const SNMP_KINDS = ['snmp_v2c', 'snmp_v3'];

interface RowState {
  selected: boolean;
  name: string;
  profile_id: string;
  credential_id: string;
  vendor: string;
  model: string;
}

export function DiscoveryPage() {
  const authed = useAuthStore((s) => s.authed);
  const [targetSpec, setTargetSpec] = useState('192.168.1.0/24');
  const [selectedCredIds, setSelectedCredIds] = useState<string[]>([]);
  const [scanId, setScanId] = useState<string | null>(null);
  const [done, setDone] = useState(false);
  const [candidates, setCandidates] = useState<DiscoveryCandidate[]>([]);
  const [rowState, setRowState] = useState<Record<string, RowState>>({});
  const [imported, setImported] = useState<Record<string, boolean>>({});
  const [importNote, setImportNote] = useState<string | null>(null);
  const [importError, setImportError] = useState<string | null>(null);
  const [profiles, setProfiles] = useState<ProfileSummary[]>([]);
  const [creds, setCreds] = useState<CredentialSummary[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [note, setNote] = useState<string | null>(null);
  const pollRef = useRef<ReturnType<typeof setInterval> | null>(null);

  useEffect(() => {
    api.listProfiles().then(setProfiles).catch(() => undefined);
    api
      .listCredentials()
      .then((list) => {
        setCreds(list);
        // Preselect every SNMP credential — the common case is "try all my secrets".
        setSelectedCredIds(list.filter((c) => SNMP_KINDS.includes(c.kind)).map((c) => c.id));
      })
      .catch(() => undefined);
    return () => {
      if (pollRef.current) clearInterval(pollRef.current);
    };
  }, []);

  const snmpCreds = creds.filter((c) => SNMP_KINDS.includes(c.kind));

  // Seed per-row form state when new candidates arrive (suggested profile + matched
  // credential preselected so import binds the working secret automatically).
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
            credential_id: c.matched_credential_id ?? '',
            vendor: c.vendor ?? '',
            model: c.model ?? '',
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
    setImportNote(null);
    setImportError(null);
    const targets = expandTargets(targetSpec);
    if (targets.length === 0) {
      setError(
        'Enter IPs, CIDRs (≤/22), or ranges (e.g. 192.168.1.10-20), comma-separated — ≤1024 hosts total.',
      );
      return;
    }
    setCandidates([]);
    setRowState({});
    setImported({});
    setDone(false);
    api
      .startDiscoveryScan({ targets, credential_ids: selectedCredIds })
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
        } else {
          const at = s.scanning ? ` — now at ${s.scanning}` : '';
          setNote(`Scanning… ${s.probed}/${s.total} addresses probed${at}`);
        }
      })
      .catch(() => undefined);
  };

  const patchRow = (addr: string, patch: Partial<RowState>) =>
    setRowState((cur) => ({ ...cur, [addr]: { ...cur[addr], ...patch } }));

  const importSelected = () => {
    setImportNote(null);
    setImportError(null);
    const nodes = candidates
      .filter((c) => rowState[c.address]?.selected && !imported[c.address])
      .map((c) => {
        const r = rowState[c.address];
        return {
          address: c.address,
          name: r.name.trim() || c.address,
          profile_id: r.profile_id || undefined,
          credential_id: r.credential_id || undefined,
          vendor: r.vendor.trim() || undefined,
          model: r.model.trim() || undefined,
        };
      });
    if (nodes.length === 0) {
      setImportError('Select at least one device to import.');
      return;
    }
    api
      .importDiscovered(nodes)
      .then(({ created }) => {
        setImportNote(`Imported ${created} node(s) — they will start polling shortly.`);
        // Mark the imported rows and clear their selection (no double-import).
        setImported((cur) => {
          const next = { ...cur };
          for (const n of nodes) next[n.address] = true;
          return next;
        });
        setRowState((cur) => {
          const next = { ...cur };
          for (const n of nodes) if (next[n.address]) next[n.address].selected = false;
          return next;
        });
      })
      .catch((e: unknown) => setImportError(errMsg(e, 'failed to import')));
  };

  const selectedCount = candidates.filter(
    (c) => rowState[c.address]?.selected && !imported[c.address],
  ).length;

  return (
    <div>
      <PageHeader
        title="Discovery"
        trail={[{ label: 'Nodes' }, { label: 'Discovery' }]}
        note="Sweep a subnet for live + SNMP devices, then import the ones you want."
      />

      <Card title="Scan a subnet">
        {authed ? (
          <>
            <div className="disco-form form-row">
              <TextInput
                className="mono"
                placeholder="CIDR, range or list (e.g. 192.168.1.0/24, 10.0.0.5, 192.168.1.10-20)"
                value={targetSpec}
                onChange={(e) => setTargetSpec(e.target.value)}
              />
              <CredentialPicker
                options={snmpCreds}
                selected={selectedCredIds}
                onChange={setSelectedCredIds}
                disabled={!!scanId && !done}
              />
              <Button variant="primary" onClick={startScan} disabled={!!scanId && !done}>
                {scanId && !done ? 'Scanning…' : 'Scan'}
              </Button>
            </div>
            <p className="disco-target-hint">
              Examples: <span className="mono">192.168.1.0/24</span> ·{' '}
              <span className="mono">10.0.0.5</span> ·{' '}
              <span className="mono">192.168.1.10-20</span> ·{' '}
              <span className="mono">192.168.1.10-192.168.1.20</span> — comma-separate for multiple.
            </p>
            <p className="disco-creds-link">
              Discovery uses your stored credentials.{' '}
              <Link to="/settings/credentials">Manage credentials →</Link>
            </p>
          </>
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
              const isImported = !!imported[c.address];
              return (
                <div className="disco-row" key={c.address}>
                  <input
                    type="checkbox"
                    checked={r.selected}
                    disabled={isImported}
                    onChange={(e) => patchRow(c.address, { selected: e.target.checked })}
                  />
                  <span className="mono">
                    {c.address}{' '}
                    {isImported ? (
                      <Badge tone="up">imported</Badge>
                    ) : c.reachable ? (
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
                    {(c.vendor || c.model) && (
                      <span className="disco-makermodel">
                        {[c.vendor, c.model].filter(Boolean).join(' · ')}
                      </span>
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
                        {cr.id === c.matched_credential_id ? ' ✓ matched' : ''}
                      </option>
                    ))}
                  </Select>
                </div>
              );
            })}
          </div>
          {authed && (
            <div className="disco-import">
              {importNote && <span className="disco-import-ok">✓ {importNote}</span>}
              {importError && <span className="disco-import-err">{importError}</span>}
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
