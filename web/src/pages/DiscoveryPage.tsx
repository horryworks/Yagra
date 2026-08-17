// SPDX-License-Identifier: AGPL-3.0-only
// Discovery (Nodes ▸ Discovery). Sweep a subnet for live + SNMP-speaking devices, review the
// candidates (classified into a suggested profile from sysDescr), and import the chosen ones as
// nodes. The sweep runs on the poller (raw-socket ICMP); core correlates results by scan id.
// Stored credentials (v2c/v3) are selectable as scan candidates; the one that answers is
// preselected on the row so import binds it automatically.

import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { Link, useSearchParams } from 'react-router-dom';
import { Trans, useTranslation } from 'react-i18next';
import { api, ApiError, errMsg } from '../services/api';
import { useCan } from '../store';
import type {
  CredentialSummary,
  DiscoveredEndpoint,
  DiscoveredEndpointPage,
  DiscoveryCandidate,
  DiscoveryScan,
  DiscoveryScanSummary,
  PoolOption,
  ProfileSummary,
} from '../types/api';
import {
  canRequestStop,
  isScanInFlight,
  pickDefaultPool,
  poolIsUnrouted,
  SCAN_STATE_SPECS,
  scanState,
  selectInitialScan,
  shouldPollScan,
} from './discoveryScans';
import { expandTargets } from '../lib/cidr';
import { PageHeader } from '../components/ui/PageHeader';
import { Card } from '../components/ui/Card';
import { PermissionHint } from '../components/ui/PermissionHint';
import { Button } from '../components/ui/Button';
import { TextInput, Select, FieldHint } from '../components/ui/Field';
import { Badge } from '../components/ui/Badge';
import { CredentialPicker } from '../components/ui/CredentialPicker';
import { EntityName } from '../components/ui/EntityName';
import { coverageOf } from './discoveredEndpoints';
import {
  candidateColumns,
  candidateLabels,
  endpointColumns,
  endpointLabels,
  ENDPOINT_DEFAULT_MONITORED,
} from './discoveryFilters';
import { TableToolbar, TableSpacer, ResultCount } from '../components/ui/TableToolbar';
import { ColumnFilterRow } from '../components/ui/ColumnFilterRow';
import { ClearFilters } from '../components/ui/ClearFilters';
import { FilterButton, MobileFilterSheet } from '../components/ui/MobileFilterSheet';
import { defaultFilters, isAnyFiltered, type FilterState } from '../lib/columnFilter';
import { facetCounts } from '../lib/filterCounts';
import { buildPredicate } from '../lib/filterPredicate';
import { isSnmpCredentialKind } from '../lib/credentialKinds';
import './DiscoveryPage.css';

interface RowState {
  selected: boolean;
  name: string;
  profile_id: string;
  credential_id: string;
  vendor: string;
  model: string;
}

export function DiscoveryPage() {
  const { t } = useTranslation('monitoring');
  const canConfig = useCan('manage_config');
  const [searchParams, setSearchParams] = useSearchParams();
  const [targetSpec, setTargetSpec] = useState('192.168.1.0/24');
  const [selectedCredIds, setSelectedCredIds] = useState<string[]>([]);
  const [scanId, setScanId] = useState<string | null>(null);
  const [status, setStatus] = useState<DiscoveryScan | null>(null);
  const [scans, setScans] = useState<DiscoveryScanSummary[]>([]);
  const [pools, setPools] = useState<PoolOption[]>([]);
  const [pool, setPool] = useState<string | null>(null);
  /** Try SNMP on addresses that did not answer ping (ADR-068 Inc.3). Off by default: on a /24 the
   *  unassigned addresses are the overwhelming majority, and asking each of them for its identity
   *  is where a sweep's minutes went. Kept as a choice because a firewall that filters ICMP and
   *  answers SNMP is a real device this would otherwise never find. */
  const [snmpWhenUnreachable, setSnmpWhenUnreachable] = useState(false);
  /** The operator pressed Scan and the first status has not landed. Drives polling on its own —
   *  see `shouldPollScan` for why this cannot be derived from what the server currently says. */
  const [justStarted, setJustStarted] = useState(false);
  const [failures, setFailures] = useState(0);
  /** The selected scan is one this core has no record of (restarted, or it aged out). */
  const [unknownScan, setUnknownScan] = useState(false);
  /** Whether the shown scan was picked up on arrival rather than started here. */
  const [reattached, setReattached] = useState(false);
  /** What the last stop request was told about this fleet's ability to honour it. */
  const [stopOutcome, setStopOutcome] = useState<'requested' | 'unsupported' | null>(null);
  const [stopError, setStopError] = useState<string | null>(null);
  const [candidates, setCandidates] = useState<DiscoveryCandidate[]>([]);
  const [rowState, setRowState] = useState<Record<string, RowState>>({});
  const [imported, setImported] = useState<Record<string, boolean>>({});
  const [importNote, setImportNote] = useState<string | null>(null);
  const [importError, setImportError] = useState<string | null>(null);
  const [profiles, setProfiles] = useState<ProfileSummary[]>([]);
  const [creds, setCreds] = useState<CredentialSummary[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [note, setNote] = useState<string | null>(null);
  // Client-side: a sweep's result set is bounded by the range an operator typed in (ui-conventions).
  const candCols = useMemo(() => candidateColumns(t), [t]);
  const candLabels = useMemo(() => candidateLabels(t), [t]);
  const [filters, setFilters] = useState<FilterState>(() => defaultFilters(candCols));
  const [candSheet, setCandSheet] = useState(false);
  /** The `?scan=` present when the page was opened. Held in a ref because the URL is rewritten from
   *  `scanId` below, so reading the live value during the reattach would race with our own write. */
  const arrivedWith = useRef(searchParams.get('scan'));

  useEffect(() => {
    api.listProfiles().then(setProfiles).catch(() => undefined);
    api
      .listCredentials()
      .then((list) => {
        setCreds(list);
        // Preselect every SNMP credential — the common case is "try all my secrets".
        setSelectedCredIds(list.filter((c) => isSnmpCredentialKind(c.kind)).map((c) => c.id));
      })
      .catch(() => undefined);
    // Which site sweeps from here. A read failure degrades to "no sites offered", never to a
    // blocked page — the sweep still works, it just goes wherever it used to go.
    api
      .listPools()
      .then((r) => {
        setPools(r.pools);
        setPool(pickDefaultPool(r.pools));
      })
      .catch(() => undefined);
  }, []);

  // Reattach on arrival (ADR-068). This is the whole point of the increment: the scan id used to
  // live only in this component's state, so leaving the page abandoned a sweep the poller was still
  // running. ⚠️ Nothing automated covers this effect — Vitest executes no `.tsx` and the browser
  // walk opens each screen once, so "navigate away and come back" is checked by hand.
  useEffect(() => {
    api
      .listDiscoveryScans()
      .then((rows) => {
        setScans(rows);
        const pick = selectInitialScan(rows, arrivedWith.current);
        if (!pick) return;
        setScanId(pick);
        setReattached(true);
      })
      .catch(() => undefined);
  }, []);

  // The one place the URL is written. Two handlers each calling `setSearchParams` would have the
  // second silently undo the first, because both act on the params they captured at render.
  useEffect(() => {
    if ((searchParams.get('scan') ?? null) === scanId) return;
    const next = new URLSearchParams(searchParams);
    if (scanId) next.set('scan', scanId);
    else next.delete('scan');
    setSearchParams(next, { replace: true });
  }, [scanId, searchParams, setSearchParams]);

  const snmpCreds = creds.filter((c) => isSnmpCredentialKind(c.kind));

  // Seed per-row form state when new candidates arrive. The suggested profile is resolved
  // server-side (by sysObjectID/sysDescr → classification rules) and arrives as an id, so we
  // pre-select it directly — robust to profile renames. The matched credential is preselected
  // too, so import binds the working secret automatically.
  const seedRows = useCallback((list: DiscoveryCandidate[]) => {
    setRowState((cur) => {
      const next = { ...cur };
      for (const c of list) {
        if (next[c.address]) continue;
        next[c.address] = {
          selected: false,
          name: c.sysname?.trim() || c.address,
          profile_id: c.suggested_profile_id ?? '',
          credential_id: c.matched_credential_id ?? '',
          vendor: c.vendor ?? '',
          model: c.model ?? '',
        };
      }
      return next;
    });
  }, []);

  const startScan = () => {
    setError(null);
    setNote(null);
    setImportNote(null);
    setImportError(null);
    const targets = expandTargets(targetSpec);
    if (targets.length === 0) {
      setError(t('discovery.err.badTargets'));
      return;
    }
    setCandidates([]);
    setRowState({});
    setImported({});
    setStatus(null);
    setUnknownScan(false);
    setReattached(false);
    setFailures(0);
    setStopOutcome(null);
    setStopError(null);
    // Set before the request: polling is driven by the act of starting, so that a slow or
    // momentarily unanswerable server cannot leave the page silent (`shouldPollScan`).
    setJustStarted(true);
    api
      // `pool` decides which site sweeps. Omitted means "any poller", which is what this screen
      // always did and is still a legitimate choice — just no longer an invisible one.
      .startDiscoveryScan({
        targets,
        credential_ids: selectedCredIds,
        ...(pool ? { pool } : {}),
        snmp_when_unreachable: snmpWhenUnreachable,
      })
      .then(({ scan_id }) => {
        setScanId(scan_id);
        setNote(t('discovery.msg.scanningCount', { count: targets.length }));
        api.listDiscoveryScans().then(setScans).catch(() => undefined);
      })
      .catch((e: unknown) => {
        setJustStarted(false);
        setError(errMsg(e, t('discovery.err.startScan')));
      });
  };

  const poll = useCallback(
    (id: string) => {
      api
        .getDiscoveryScan(id)
        .then((s) => {
          setStatus(s);
          setUnknownScan(false);
          setFailures(0);
          setJustStarted(false);
          setCandidates(s.candidates);
          seedRows(s.candidates);
          if (!isScanInFlight(s.state)) {
            setNote(t('discovery.msg.scanComplete', { count: s.candidates.length }));
          } else {
            const at = s.scanning ? t('discovery.msg.nowAt', { addr: s.scanning }) : '';
            setNote(t('discovery.msg.scanningProgress', { probed: s.probed, total: s.total }) + at);
          }
        })
        .catch((e: unknown) => {
          setFailures((n) => n + 1);
          // A 404 is the specific, expected failure: this core restarted (or the scan aged out)
          // while a poller may still be sweeping. It gets its own message rather than being
          // swallowed — the page used to eat it and sit on a frozen progress line forever.
          if (e instanceof ApiError && e.status === 404) {
            setUnknownScan(true);
            setJustStarted(false);
            setNote(null);
          }
        });
    },
    [seedRows, t],
  );

  const polling = shouldPollScan({ status, justStarted, failures });

  useEffect(() => {
    if (!scanId || !polling) return;
    poll(scanId);
    const timer = setInterval(() => poll(scanId), 2000);
    return () => clearInterval(timer);
    // ⚠️ `status` is deliberately not a dependency: it changes on every tick, and re-running this
    // would tear down and rebuild the interval each time, so the 2s cadence would restart forever.
    // `polling` is the boolean distillation of it, and only flips when the answer changes.
  }, [scanId, polling, poll]);

  /** The scan on screen is still producing results. */
  const inFlight = status ? isScanInFlight(status.state) : justStarted;
  const unroutedPool = poolIsUnrouted(pools, pool);

  const requestStop = () => {
    if (!scanId) return;
    setStopError(null);
    api
      .cancelDiscoveryScan(scanId)
      .then((r) => {
        // Optimistic, and only as far as the truth goes: the request is a fact, the stop is not.
        // The authoritative answer arrives on the next poll as the scan's own state.
        setStatus((cur) => (cur ? { ...cur, state: 'cancelling' } : cur));
        setStopOutcome(r.poller_supports_cancel ? 'requested' : 'unsupported');
      })
      .catch((e: unknown) => setStopError(errMsg(e, t('discovery.scans.stopErr'))));
  };

  const shownCandidates = candidates.filter(buildPredicate(candCols, filters, Date.now()));
  const candCounts = Object.fromEntries(
    candCols
      .filter((c) => c.filter.kind === 'enum')
      .map((c) => [c.key, facetCounts(candidates, candCols, filters, c.key, Date.now())]),
  );

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
      setImportError(t('discovery.err.selectOne'));
      return;
    }
    api
      .importDiscovered(nodes)
      .then(({ created }) => {
        setImportNote(t('discovery.msg.imported', { count: created }));
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
      .catch((e: unknown) => setImportError(errMsg(e, t('discovery.err.import'))));
  };

  const selectedCount = candidates.filter(
    (c) => rowState[c.address]?.selected && !imported[c.address],
  ).length;

  return (
    <div>
      <PageHeader
        title={t('nav:nodes.discovery')}
        trail={[{ label: t('nav:sections.nodes') }, { label: t('nav:nodes.discovery') }]}
        note={t('discovery.note')}
      />

      <Card title={t('discovery.scanTitle')}>
        {canConfig ? (
          <>
            {/* Three inputs, each named and each carrying its own hint directly underneath
                (ADR-055 R1/R2). They were an unlabelled row with all three explanations stacked
                below it, so nothing on screen said which sentence belonged to which control — and
                the pool select in particular read as an unexplained dropdown. */}
            <div className="disco-form form-row">
              <label className="form-label disco-f-targets">
                {t('discovery.targetsLabel')}
                <TextInput
                  className="mono"
                  placeholder={t('discovery.targetPlaceholder')}
                  value={targetSpec}
                  onChange={(e) => setTargetSpec(e.target.value)}
                />
                <FieldHint>
                  <Trans
                    t={t}
                    i18nKey="discovery.examplesHint"
                    components={{ c: <span className="mono" /> }}
                  />
                </FieldHint>
              </label>

              <label className="form-label disco-f-creds">
                {t('discovery.credsLabel')}
                <CredentialPicker
                  options={snmpCreds}
                  selected={selectedCredIds}
                  onChange={setSelectedCredIds}
                  disabled={inFlight}
                />
                <FieldHint>
                  <Trans
                    t={t}
                    i18nKey="discovery.credsLink"
                    components={{ lnk: <Link to="/nodes/credentials" /> }}
                  />
                </FieldHint>
              </label>

              {/* Which site the sweep runs from (ADR-068). Before this, the job went to a subject
                  every poller competes for, so a remote-site poller could sweep head office —
                  reaching nothing and reporting a successful, empty scan. */}
              <label className="form-label disco-f-pool">
                {t('discovery.pool.label')}
                <Select
                  value={pool ?? ''}
                  disabled={inFlight}
                  onChange={(e) => setPool(e.target.value || null)}
                >
                  <option value="">{t('discovery.pool.any')}</option>
                  {pools.map((p) => (
                    <option key={p.name} value={p.name}>
                      {p.name}
                    </option>
                  ))}
                </Select>
                {/* Says what the choice means rather than removing the option: a pool that is
                    briefly down is still the pool the operator means, and hiding it would take
                    away the reason to come back to it. */}
                <FieldHint error={unroutedPool}>
                  {!pool
                    ? t('discovery.pool.anyHint')
                    : unroutedPool
                      ? t('discovery.pool.deadHint')
                      : t('discovery.pool.oneOf')}
                </FieldHint>
              </label>

            </div>

            {/* The ICMP gate (ADR-068 Inc.3). Below the row rather than inside it: it modifies how
                the whole sweep runs rather than describing one of the three inputs, and it is the
                only control here whose cost the operator should read before pressing Scan.
                The input stays enabled mid-sweep (the value is worth reading) — only the button
                that acts on it goes away (ui-conventions). */}
            <label className="disco-opt">
              <input
                type="checkbox"
                checked={snmpWhenUnreachable}
                disabled={inFlight}
                onChange={(e) => setSnmpWhenUnreachable(e.target.checked)}
              />
              <span>
                {t('discovery.icmpGate.label')}
                <FieldHint>{t('discovery.icmpGate.hint')}</FieldHint>
              </span>
            </label>

            {/* Below the row rather than in it. Each field is now three bands tall (label, input,
                hint), and a hint that wraps makes its column taller than the others — so a button
                sharing the row would drift away from the control it acts on, by an amount that
                depends on the text. */}
            <div className="disco-actions">
              <Button variant="primary" onClick={startScan} disabled={inFlight}>
                {inFlight ? t('discovery.scanning') : t('discovery.scan')}
              </Button>
              {/* Drawn only while there is something to stop, never disabled: a disabled button
                  explains itself on hover alone, which is nothing on a touch device
                  (ui-conventions R4 / ADR-056). Once asked, the button goes and the sentence below
                  takes over. */}
              {canRequestStop(status?.state) && (
                <Button onClick={requestStop}>{t('discovery.scans.stop')}</Button>
              )}
            </div>
          </>
        ) : (
          <PermissionHint permission="manage_config" signInHint={t('discovery.signIn')} />
        )}
        {error && <p className="form-error">{error}</p>}
        {/* The 404 case gets its own line, above the progress note. A core that restarted mid-sweep
            answers "no such scan" while a poller may well still be probing, so "nothing here" would
            be the one reading that is definitely wrong. */}
        {unknownScan && <p className="disco-pool-warn">{t('discovery.scans.unknownScan')}</p>}
        {failures >= 5 && !unknownScan && (
          <p className="disco-pool-warn">{t('discovery.scans.pollFailed')}</p>
        )}
        {reattached && !unknownScan && note && (
          <p className="muted">{inFlight ? t('discovery.scans.resumed') : t('discovery.scans.resumedDone')}</p>
        )}
        {stopError && <p className="form-error">{stopError}</p>}
        {/* The four honest readings of a stop, in the order they occur. Note what is missing: any
            claim that the sweep *has* stopped while it is still `cancelling`. Core broadcasts the
            request and cannot know who acted, so until the poller reports, "requested" is the whole
            truth — and `finishedAnyway` is the case where the answer turned out to be "it did not
            stop, it completed". */}
        {stopOutcome === 'unsupported' && status?.state === 'cancelling' && (
          <p className="disco-pool-warn">{t('discovery.scans.stopUnsupported')}</p>
        )}
        {stopOutcome === 'requested' && status?.state === 'cancelling' && (
          <p className="muted">{t('discovery.scans.stopRequested')}</p>
        )}
        {status?.state === 'cancelled' && (
          <p className="muted">{t('discovery.scans.stoppedNote')}</p>
        )}
        {stopOutcome !== null && status?.state === 'done' && (
          <p className="muted">{t('discovery.scans.finishedAnyway')}</p>
        )}
        {note && <p className="muted">{note}</p>}
      </Card>

      {scans.length > 0 && (
        <Card title={t('discovery.scans.title')}>
          <p className="sys-setting-help muted">{t('discovery.scans.note')}</p>
          {/* Deliberately not a table. This screen already carries two, and `MUST_FILTER` treats
              that count as the thing to protect; a third grid would also need a filter row it has
              no use for. The list is capped server-side, so it cannot grow into one. */}
          <ul className="disco-scans">
            {scans.map((s) => {
              const spec = SCAN_STATE_SPECS[scanState(s.state)];
              return (
                <li key={s.scan_id}>
                  <button
                    type="button"
                    className={`disco-scan${s.scan_id === scanId ? ' selected' : ''}`}
                    onClick={() => {
                      setScanId(s.scan_id);
                      setReattached(true);
                      setUnknownScan(false);
                      setFailures(0);
                      setStopOutcome(null);
                      setStopError(null);
                      setStatus(null);
                      setCandidates([]);
                      setRowState({});
                      setImported({});
                    }}
                  >
                    <Badge tone={spec.tone}>{t(spec.labelKey)}</Badge>
                    <span className="muted">
                      {t('discovery.scans.progress', { probed: s.probed, total: s.total })}
                    </span>
                    <span className="muted">
                      {t('discovery.scans.candidates', { count: s.candidate_count })}
                    </span>
                    {/* The route the sweep actually took, not the one that was asked for — the
                        server falls back to "any poller" when a named site has none live. */}
                    <span className="muted">{s.pool ?? t('discovery.pool.any')}</span>
                  </button>
                </li>
              );
            })}
          </ul>
        </Card>
      )}

      {candidates.length > 0 && (
        <Card title={t('discovery.resultsTitle')} className="disco-results-card">
          <TableToolbar>
            <FilterButton
              columns={candCols}
              filters={filters}
              onOpen={() => setCandSheet(true)}
            />
            <ClearFilters
              columns={candCols}
              filters={filters}
              onClear={() => setFilters(defaultFilters(candCols))}
            />
            <TableSpacer />
            <ResultCount
              shown={shownCandidates.length}
              total={isAnyFiltered(candCols, filters) ? candidates.length : undefined}
              noun={t('discovery.filter.candidateNoun')}
            />
          </TableToolbar>
          {candSheet && (
            <MobileFilterSheet
              columns={candCols}
              labels={candLabels}
              filters={filters}
              onChange={setFilters}
              counts={candCounts}
              onClose={() => setCandSheet(false)}
            />
          )}
          <div className="disco-table">
            <div className="disco-head">
              <div className="disco-h" />
              <div className="disco-h">{t('discovery.cols.address')}</div>
              <div className="disco-h">{t('discovery.cols.identity')}</div>
              <div className="disco-h">{t('discovery.cols.name')}</div>
              <div className="disco-h">{t('discovery.cols.profile')}</div>
              <div className="disco-h">{t('discovery.cols.credential')}</div>
            </div>
            {/* Filter row under the header — same CSS grid rule as `.disco-head` and every
                `.disco-row` (ADR-053 Inc.6 decision F). Four of the six tracks carry an empty cell:
                the first is 28px and belongs to the select checkbox, and the last three are the
                operator's *input* for the import about to happen — filtering a field you are typing
                into would take the row out from under the cursor.
                ⚠️ A reachability control was put in that 28px track and shipped unusable, which is
                what a filter row costs when nothing renders it in a test. It is gone rather than
                relocated; `discoveryFilters.ts` says why.
                Not drawn on a phone (`FilterButton` is the other half): mobile re-templates
                `.disco-head` and `.disco-row` to fixed px + `width: max-content` and scrolls them
                sideways, and it named two of the three grids — so this row stayed at container
                width and every control sat somewhere other than under its column. */}
            <ColumnFilterRow
              columns={candCols}
              slots={[null, 'address', 'identity', null, null, null]}
              filters={filters}
              onChange={setFilters}
              counts={candCounts}
              labels={candLabels}
              className="disco-filters"
            />
            {shownCandidates.map((c) => {
              const r = rowState[c.address];
              if (!r) return null;
              const isImported = !!imported[c.address];
              return (
                <div className="disco-row" key={c.address}>
                  {/* Wrapper is display:contents on desktop (input stays the grid cell) and a real
                      sticky cell on mobile so the select column stays pinned during h-scroll. */}
                  <div className="disco-check">
                    <input
                      type="checkbox"
                      checked={r.selected}
                      disabled={isImported}
                      onChange={(e) => patchRow(c.address, { selected: e.target.checked })}
                    />
                  </div>
                  <span className="mono">
                    {c.address}{' '}
                    {isImported ? (
                      <Badge tone="up">{t('discovery.badge.imported')}</Badge>
                    ) : c.reachable ? (
                      <Badge tone="up">{t('discovery.badge.ping')}</Badge>
                    ) : (
                      <span className="muted">{t('discovery.badge.noPing')}</span>
                    )}
                  </span>
                  <span className="disco-identity">
                    {c.sysname && <span className="disco-sysname">{c.sysname}</span>}
                    {c.sysdescr ? (
                      <span className="muted disco-sysdescr" title={c.sysdescr}>
                        {c.sysdescr}
                      </span>
                    ) : (
                      <span className="muted">{t('discovery.noSnmp')}</span>
                    )}
                    {(c.vendor || c.model) && (
                      <span className="disco-makermodel">
                        {[c.vendor, c.model].filter(Boolean).join(' · ')}
                      </span>
                    )}
                    {c.sysobjectid && (
                      <span className="muted mono disco-sysoid" title={t('discovery.sysObjectIdTitle')}>
                        {c.sysobjectid}
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
                    <option value="">{t('discovery.none')}</option>
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
                    <option value="">{t('discovery.none')}</option>
                    {creds.map((cr) => (
                      <option key={cr.id} value={cr.id}>
                        {cr.name}
                        {cr.id === c.matched_credential_id ? t('discovery.matchedSuffix') : ''}
                      </option>
                    ))}
                  </Select>
                </div>
              );
            })}
          </div>
          {canConfig && (
            <div className="disco-import">
              {importNote && <span className="disco-import-ok">✓ {importNote}</span>}
              {importError && <span className="disco-import-err">{importError}</span>}
              <Button variant="primary" onClick={importSelected} disabled={selectedCount === 0}>
                {selectedCount > 0
                  ? t('discovery.importSelectedCount', { count: selectedCount })
                  : t('discovery.importSelectedNone')}
              </Button>
            </div>
          )}
        </Card>
      )}

      <SeenOnNetworkCard canConfig={canConfig} profiles={profiles} creds={creds} />
    </div>
  );
}

/** Discovery ▸ Seen on the network (ADR-043 Increment 3).
 *
 *  The passive half of discovery: addresses monitored routers have resolved on the wire that Yagra
 *  does not monitor. It needs no operator action to produce results and no scan to be running — but
 *  it does need ARP discovery switched on, which it is not by default, so the empty state has to say
 *  which kind of empty it is. That judgement is `coverageOf` in `discoveredEndpoints.ts`.
 *
 *  Importing goes through the same node writer the scan import uses, so classification happens on the
 *  new node's first identity probe exactly as it does for a scanned device. */
function SeenOnNetworkCard({
  canConfig,
  profiles,
  creds,
}: {
  canConfig: boolean;
  profiles: ProfileSummary[];
  creds: CredentialSummary[];
}) {
  const { t } = useTranslation('monitoring');
  const [page, setPage] = useState<DiscoveredEndpointPage | null>(null);
  const [rows, setRows] = useState<Record<string, { profile_id: string; credential_id: string }>>(
    {},
  );
  const [busyId, setBusyId] = useState<string | null>(null);
  const [note, setNote] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const epCols = useMemo(() => endpointColumns(t), [t]);
  const epLabels = useMemo(() => endpointLabels(t), [t]);
  // ⚠️ Not `defaultFilters(epCols)`: this table's default *narrows*. See
  // `ENDPOINT_DEFAULT_MONITORED` for why, and note the consequence — `isAnyFiltered` reports true
  // on the default view, so the row count deliberately always shows the total beside it.
  const epDefaults = useMemo(
    () => ({ ...defaultFilters(epCols), monitored: ENDPOINT_DEFAULT_MONITORED }),
    [epCols],
  );
  const [epFilters, setEpFilters] = useState<FilterState>(epDefaults);
  const [epSheet, setEpSheet] = useState(false);

  const load = useCallback(() => {
    api
      .listDiscoveredEndpoints({ limit: 100 })
      .then(setPage)
      .catch(() => undefined);
  }, []);

  useEffect(() => {
    load();
  }, [load]);

  const promote = (e: DiscoveredEndpoint) => {
    const r = rows[e.id] ?? { profile_id: '', credential_id: '' };
    setBusyId(e.id);
    setNote(null);
    setError(null);
    api
      .importDiscoveredEndpoint(e.id, {
        profile_id: r.profile_id || undefined,
        credential_id: r.credential_id || undefined,
      })
      .then(() => {
        setNote(t('discovery.seen.imported', { addr: e.ip }));
        load();
      })
      .catch((err: unknown) => setError(errMsg(err, t('discovery.seen.err.import'))))
      .finally(() => setBusyId(null));
  };

  const coverage = coverageOf(page?.summary as DiscoveredEndpointPage['summary']);
  const all = page?.endpoints ?? [];
  // The "unmonitored only" default is what this table has always done, unconditionally and with
  // nothing on screen saying so — see `discoveryFilters.ts`. It is a control now, so an endpoint
  // that disappeared because someone else imported it can be told from one no longer being seen.
  const endpoints = all.filter(buildPredicate(epCols, epFilters, Date.now()));
  const epCounts = Object.fromEntries(
    epCols
      .filter((c) => c.filter.kind === 'enum')
      .map((c) => [c.key, facetCounts(all, epCols, epFilters, c.key, Date.now())]),
  );

  return (
    <Card title={t('discovery.seen.title')}>
      <p className="sys-setting-help muted">{t('discovery.seen.note')}</p>
      <p className={coverage === 'sampled' ? 'disco-seen-warn' : 'muted'}>
        {t(`discovery.seen.coverage.${coverage}`, {
          observed: page?.summary?.observed_total ?? 0,
          nodes: page?.summary?.nodes_reporting ?? 0,
          truncated: page?.summary?.truncated_nodes ?? 0,
        })}
      </p>
      {all.length > 0 && (
        <TableToolbar>
          <FilterButton
            columns={epCols}
            filters={epFilters}
            baseline={epDefaults}
            onOpen={() => setEpSheet(true)}
          />
          <ClearFilters
            columns={epCols}
            filters={epFilters}
            // Back to *this table's* default, not to the empty state: "unmonitored only" is the
            // view an operator expects to land on here, and a reset that showed the imported ones
            // would look like the button had done something else.
            //
            // …which is also why `baseline` is the same object: counted against the spec defaults
            // this button read "Clear all filters (1)" on a view nobody had filtered, and pressing
            // it changed nothing on screen.
            baseline={epDefaults}
            onClear={() => setEpFilters(epDefaults)}
          />
          <TableSpacer />
          <ResultCount
            shown={endpoints.length}
            // Always paired with the total, because the default already narrows.
            total={all.length}
            noun={t('discovery.seen.filter.endpointNoun')}
          />
        </TableToolbar>
      )}
      {epSheet && (
        <MobileFilterSheet
          columns={epCols}
          labels={epLabels}
          filters={epFilters}
          onChange={setEpFilters}
          counts={epCounts}
          onClose={() => setEpSheet(false)}
        />
      )}
      {/* ⚠️ Gated on `all`, the UNFILTERED list, not on `endpoints`. Gating on the filtered one made
          the header and the filter row vanish the moment a filter matched nothing — taking the
          controls that would undo it along with the rows. `AlertRows`' toolbar slot carries the
          same warning, and this shipped with the mistake anyway. */}
      {all.length > 0 && (
        <div className="disco-seen-table">
          <div className="disco-seen-head">
            <div className="disco-h">{t('discovery.seen.cols.address')}</div>
            <div className="disco-h">{t('discovery.seen.cols.mac')}</div>
            <div className="disco-h">{t('discovery.seen.cols.seenBy')}</div>
            <div className="disco-h">{t('discovery.cols.profile')}</div>
            <div className="disco-h">{t('discovery.cols.credential')}</div>
            <div className="disco-h" />
          </div>
          {/* Same CSS grid rule as the header and every row, and the same mobile story as the
              candidates table above — this one is hidden on a phone for the same reason. The
              profile and credential columns are the import form's own inputs, and the last is the
              button.
              ⚠️ `epDefaults` as the `baseline`, here and on `FilterButton` and `ClearFilters` —
              all three, the same object. Without it `activeFilterCount` reports 1 before the
              operator has touched anything (the narrowing default counts as a filter), which since
              Inc.9 forces this row open forever and locks the toggle meant to close it. */}
          <ColumnFilterRow
            columns={epCols}
            slots={['ip', 'mac', 'via', null, null, 'monitored']}
            filters={epFilters}
            onChange={setEpFilters}
            counts={epCounts}
            labels={epLabels}
            className="disco-seen-filters"
            baseline={epDefaults}
          />
          {endpoints.map((e) => {
            const r = rows[e.id] ?? { profile_id: '', credential_id: '' };
            return (
              <div className="disco-seen-row" key={e.id}>
                <span className="mono">{e.ip}</span>
                <span className="mono muted">{e.mac ?? t('discovery.seen.noMac')}</span>
                <span className="disco-seen-via">
                  {e.via_node ? (
                    <EntityName name={e.via_node} id={e.via_node} />
                  ) : (
                    <span className="muted">{t('discovery.seen.viaGone')}</span>
                  )}
                  {e.via_ifindex != null && (
                    <span className="muted mono"> · {t('discovery.seen.port', { n: e.via_ifindex })}</span>
                  )}
                </span>
                <Select
                  value={r.profile_id}
                  disabled={!canConfig || busyId != null}
                  onChange={(ev) =>
                    setRows((cur) => ({ ...cur, [e.id]: { ...r, profile_id: ev.target.value } }))
                  }
                >
                  <option value="">{t('discovery.none')}</option>
                  {profiles.map((p) => (
                    <option key={p.id} value={p.id}>
                      {p.name}
                    </option>
                  ))}
                </Select>
                <Select
                  value={r.credential_id}
                  disabled={!canConfig || busyId != null}
                  onChange={(ev) =>
                    setRows((cur) => ({ ...cur, [e.id]: { ...r, credential_id: ev.target.value } }))
                  }
                >
                  <option value="">{t('discovery.none')}</option>
                  {creds.map((cr) => (
                    <option key={cr.id} value={cr.id}>
                      {cr.name}
                    </option>
                  ))}
                </Select>
                <Button
                  variant="primary"
                  disabled={!canConfig || busyId != null}
                  onClick={() => promote(e)}
                >
                  {t('discovery.seen.monitor')}
                </Button>
              </div>
            );
          })}
          {endpoints.length === 0 && (
            <p className="muted disco-seen-empty">{t('common:filter.noMatch')}</p>
          )}
        </div>
      )}
      {!canConfig && (
        <PermissionHint permission="manage_config" signInHint={t('discovery.signIn')} />
      )}
      {error && <p className="form-error">{error}</p>}
      {note && <p className="disco-import-ok">✓ {note}</p>}
    </Card>
  );
}
