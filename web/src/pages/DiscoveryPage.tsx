// SPDX-License-Identifier: AGPL-3.0-only
// Discovery (Nodes ▸ Discovery). Sweep a subnet for live + SNMP-speaking devices, review the
// candidates (classified into a suggested profile from sysDescr), and import the chosen ones as
// nodes. The sweep runs on the poller (raw-socket ICMP); core correlates results by scan id.
// Stored credentials (v2c/v3) are selectable as scan candidates; the one that answers is
// preselected on the row so import binds it automatically.

import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { Link } from 'react-router-dom';
import { Trans, useTranslation } from 'react-i18next';
import { api, errMsg } from '../services/api';
import { useAuthStore } from '../store';
import type {
  CredentialSummary,
  DiscoveredEndpoint,
  DiscoveredEndpointPage,
  DiscoveryCandidate,
  ProfileSummary,
} from '../types/api';
import { expandTargets } from '../lib/cidr';
import { PageHeader } from '../components/ui/PageHeader';
import { Card } from '../components/ui/Card';
import { Button } from '../components/ui/Button';
import { TextInput, Select } from '../components/ui/Field';
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
import { ColumnFilterCell } from '../components/ui/ColumnFilterCell';
import { ClearFilters } from '../components/ui/ClearFilters';
import {
  FilterButton,
  MobileFilterSheet,
  useFilterRowVisible,
} from '../components/ui/MobileFilterSheet';
import {
  defaultFilters,
  isAnyFiltered,
  type FilterState,
  type FilterableColumn,
} from '../lib/columnFilter';
import { facetCounts } from '../lib/filterCounts';
import { buildPredicate } from '../lib/filterPredicate';
import { isSnmpCredentialKind } from '../lib/credentialKinds';
import './DiscoveryPage.css';

/**
 * One cell of a hand-rolled filter row.
 *
 * Shared by the two tables on this page rather than written twice. ⚠️ The key→spec lookup is an
 * untyped index, exactly as `DataTable`'s is: rename a key in `discoveryFilters.ts` and the cell
 * quietly becomes an empty one, with nothing to notice it (`.tsx` tests never run).
 */
function filterCell<T>(
  columns: readonly FilterableColumn<T>[],
  filters: FilterState,
  onChange: (next: FilterState) => void,
  counts: Record<string, Record<string, number>>,
  labels: Record<string, string>,
  key: string,
) {
  const col = columns.find((c) => c.key === key);
  if (!col) return <div className="dt-f empty" />;
  return (
    <ColumnFilterCell
      spec={col.filter}
      value={filters[key] ?? ''}
      onChange={(next) => onChange({ ...filters, [key]: next })}
      counts={counts[key]}
      label={labels[key] ?? key}
    />
  );
}

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
  // Client-side: a sweep's result set is bounded by the range an operator typed in (ui-conventions).
  const candCols = useMemo(() => candidateColumns(t), [t]);
  const [filters, setFilters] = useState<FilterState>(() => defaultFilters(candCols));
  const [candSheet, setCandSheet] = useState(false);
  const filterRowVisible = useFilterRowVisible(candCols, filters);
  const pollRef = useRef<ReturnType<typeof setInterval> | null>(null);

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
    return () => {
      if (pollRef.current) clearInterval(pollRef.current);
    };
  }, []);

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
    setDone(false);
    api
      .startDiscoveryScan({ targets, credential_ids: selectedCredIds })
      .then(({ scan_id }) => {
        setScanId(scan_id);
        setNote(t('discovery.msg.scanningCount', { count: targets.length }));
        if (pollRef.current) clearInterval(pollRef.current);
        pollRef.current = setInterval(() => poll(scan_id), 2000);
        poll(scan_id);
      })
      .catch((e: unknown) => setError(errMsg(e, t('discovery.err.startScan'))));
  };

  const poll = (id: string) => {
    api
      .getDiscoveryScan(id)
      .then((s) => {
        setCandidates(s.candidates);
        seedRows(s.candidates);
        if (s.done) {
          setDone(true);
          setNote(t('discovery.msg.scanComplete', { count: s.candidates.length }));
          if (pollRef.current) {
            clearInterval(pollRef.current);
            pollRef.current = null;
          }
        } else {
          const at = s.scanning ? t('discovery.msg.nowAt', { addr: s.scanning }) : '';
          setNote(t('discovery.msg.scanningProgress', { probed: s.probed, total: s.total }) + at);
        }
      })
      .catch(() => undefined);
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
        {authed ? (
          <>
            <div className="disco-form form-row">
              <TextInput
                className="mono"
                placeholder={t('discovery.targetPlaceholder')}
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
                {scanId && !done ? t('discovery.scanning') : t('discovery.scan')}
              </Button>
            </div>
            <p className="disco-target-hint">
              <Trans
                t={t}
                i18nKey="discovery.examplesHint"
                components={{ c: <span className="mono" /> }}
              />
            </p>
            <p className="disco-creds-link">
              <Trans
                t={t}
                i18nKey="discovery.credsLink"
                components={{ lnk: <Link to="/settings/credentials" /> }}
              />
            </p>
          </>
        ) : (
          <p className="muted">{t('discovery.signIn')}</p>
        )}
        {error && <p className="form-error">{error}</p>}
        {note && <p className="muted">{note}</p>}
      </Card>

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
              labels={candidateLabels(t)}
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
            {filterRowVisible && (
              <div className="disco-filters" role="group" aria-label={t('common:filter.row')}>
                <div className="dt-f empty" />
                {filterCell(
                  candCols,
                  filters,
                  setFilters,
                  candCounts,
                  candidateLabels(t),
                  'address',
                )}
                {filterCell(
                  candCols,
                  filters,
                  setFilters,
                  candCounts,
                  candidateLabels(t),
                  'identity',
                )}
                <div className="dt-f empty" />
                <div className="dt-f empty" />
                <div className="dt-f empty" />
              </div>
            )}
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
          {authed && (
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

      <SeenOnNetworkCard authed={authed} profiles={profiles} creds={creds} />
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
  authed,
  profiles,
  creds,
}: {
  authed: boolean;
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
  // ⚠️ `epDefaults` as the baseline, on all three controls. Without it `activeFilterCount` reports 1
  // before the operator has touched anything — the narrowing default counts as a filter — which
  // since Inc.9 would force this row open forever and lock the toggle meant to close it.
  const filterRowVisible = useFilterRowVisible(epCols, epFilters, epDefaults);

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
              button. */}
          {filterRowVisible && (
            <div className="disco-seen-filters" role="group" aria-label={t('common:filter.row')}>
              {filterCell(epCols, epFilters, setEpFilters, epCounts, epLabels, 'ip')}
              {filterCell(epCols, epFilters, setEpFilters, epCounts, epLabels, 'mac')}
              {filterCell(epCols, epFilters, setEpFilters, epCounts, epLabels, 'via')}
              <div className="dt-f empty" />
              <div className="dt-f empty" />
              {filterCell(epCols, epFilters, setEpFilters, epCounts, epLabels, 'monitored')}
            </div>
          )}
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
                  disabled={!authed || busyId != null}
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
                  disabled={!authed || busyId != null}
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
                  disabled={!authed || busyId != null}
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
      {!authed && <p className="muted">{t('discovery.signIn')}</p>}
      {error && <p className="form-error">{error}</p>}
      {note && <p className="disco-import-ok">✓ {note}</p>}
    </Card>
  );
}
