// SPDX-License-Identifier: AGPL-3.0-only
// Active alerts — triage only (§3.2). Per the responsibility split, Yagra detects/correlates/
// suppresses/routes; the acknowledgement *action*, escalation and on-call live in external tools
// (PagerDuty/JSM). So there is NO Ack button — but ack *state* mirrored back from the external
// tool is shown read-only as an "acked" pill (ADR-015, inbound only), whose tooltip already names
// the tool and the person, which is as far as Yagra can honestly point at an external incident.
//
// Two per-alert actions, and both appear only where they would actually work — a control that is
// permanently disabled is a promise the UI cannot keep:
//   - Mute (AckAlerts, operator and above) seeds a node mute with the metric that fired.
//   - Explain (ADR-029) needs a configured provider AND a role that can spend the call.
// Alerts (and acks) arrive live over SSE.

import { useCallback, useEffect, useMemo, useState } from 'react';
import { useSearchParams } from 'react-router-dom';
import { useTranslation } from 'react-i18next';
import { useAlertStream } from '../hooks/useAlertStream';
import { useAlertStore, useAuthStore } from '../store';
import { api } from '../services/api';
import { subjectNodeId } from '../lib/alertSubject';
import { SEVERITY_ORDER } from '../lib/nodeState';
import { muteTargetFromAlert, type AlertMuteSeed } from '../lib/suppression';
import { SEVERITIES } from '../types/api';
import { PageHeader } from '../components/ui/PageHeader';
import { Button } from '../components/ui/Button';
import { ResultCount, TableSpacer, TableToolbar } from '../components/ui/TableToolbar';
import { ClearFilters } from '../components/ui/ClearFilters';
import { FilterBar } from '../components/ui/FilterBar';
import { FilterButton, MobileFilterSheet } from '../components/ui/MobileFilterSheet';
import { defaultFilters, isAnyFiltered, type FilterState } from '../lib/columnFilter';
import { facetCounts } from '../lib/filterCounts';
import { RcaModal } from '../components/Rca/RcaModal';
import { AddMuteModal } from '../components/suppression/AddMuteModal';
import { AlertRows } from '../widgets/AlertRows';
import {
  activeAlertColumns,
  activeAlertLabels,
  alertPredicate,
  readFilters,
  writeFilters,
  type NameOf,
} from './activeAlertFilters';

/** A resolver for the column set that only the URL codec and the labels look at.
 *
 *  It throws rather than returning the id, because returning something plausible would let a future
 *  edit read a row through these columns and get a raw UUID where a name belongs, silently. Only the
 *  free-text column's `readText` calls a resolver, and nothing in the codec path reads a row. */
const urlOnlyResolver: NameOf = () => {
  throw new Error('activeAlertFilters: the URL-codec column set must not read a row');
};

export function ActiveAlertsPage() {
  const { t } = useTranslation('alerts');
  useAlertStream();
  const count = useAlertStore((s) => Object.keys(s.alerts).length);
  const role = useAuthStore((s) => s.role);
  const [rcaEnabled, setRcaEnabled] = useState(false);
  const [explaining, setExplaining] = useState<{ node: string; check: string } | null>(null);
  const [muting, setMuting] = useState<AlertMuteSeed | null>(null);

  // The filters live in the URL — nothing else holds them, so a narrowed triage view survives a
  // reload and can be pasted to whoever is looking at the same incident. `replace: true` because a
  // settled filter is not a place you navigated to: pushing one per change would make Back walk
  // back through every intermediate state instead of leaving the screen.
  //
  // The columns are built with a **throwing** resolver, and only for the URL codec and the labels:
  // neither reads a row, so neither can call it. The controls and the predicate are built inside the
  // toolbar slot with the real resolver `AlertRows` owns — see the slot's own note on why the page
  // does not call `useEntityNames()` a second time.
  const [params, setParams] = useSearchParams();
  const codecColumns = useMemo(
    () => activeAlertColumns(t, urlOnlyResolver, SEVERITIES, SEVERITY_ORDER),
    [t],
  );
  const labels = useMemo(() => activeAlertLabels(t), [t]);
  const [sheet, setSheet] = useState(false);

  const filters = useMemo(
    () => readFilters(codecColumns, params),
    [codecColumns, params],
  );
  const setFilters = useCallback(
    (next: FilterState) => {
      const p = new URLSearchParams(params);
      writeFilters(codecColumns, p, next);
      setParams(p, { replace: true });
    },
    [codecColumns, params, setParams],
  );

  // No debounce on the search box, unlike every other one in the tree — and the difference is the
  // point. Those debounce because a keystroke becomes a request; here the whole alert set is
  // already in the browser (SSE), so a keystroke is an in-memory filter and delaying it would only
  // make the screen feel slow. The one request a term can trigger is the node-name batch, which
  // resolves once and is cached from the second character on.
  const filter = useCallback(
    (nameOf: NameOf) =>
      alertPredicate(activeAlertColumns(t, nameOf, SEVERITIES, SEVERITY_ORDER), filters),
    [filters, t],
  );
  const narrowed = isAnyFiltered(codecColumns, filters);

  // `rca_enabled` is the server's own answer to "would this button work" — an installation with no
  // provider would 503, so the affordance simply isn't offered there.
  useEffect(() => {
    let cancelled = false;
    api
      .getConfig()
      .then((cfg) => !cancelled && setRcaEnabled(cfg.rca_enabled === true))
      .catch(() => undefined);
    return () => {
      cancelled = true;
    };
  }, []);

  // Both actions are AckAlerts-gated server-side (operator and above) — a viewer would get a 403,
  // so don't show them a button that only ever fails. Explain additionally needs a provider.
  const canSuppress = role === 'operator' || role === 'admin';
  const canExplain = rcaEnabled && canSuppress;

  return (
    <div>
      <PageHeader
        title={t('nav:alerts.active')}
        trail={[{ label: t('nav:sections.alerts') }, { label: t('nav:alerts.active') }]}
        note={t('active.note', { count })}
      />
      {/* No `Card` around the list: the data-table standard is header → toolbar → rows, and this
          was the one list screen still wrapping its rows in a titled panel (§4.1). */}
      {/* A viewer can do neither, so the actions slot is omitted rather than rendered empty — on
          mobile the cell takes a full-width row of its own, which would otherwise be blank. */}
      <AlertRows
        filter={filter}
        emptyFiltered={t('active.emptyFiltered')}
        // The toolbar is a slot of the list rather than a sibling because the counts come from the
        // sorted-and-filtered set, which only the list has. Fixed order (design-system §4.1):
        // 検索 → フィルタ → spacer → 件数 → 主アクション (there is no primary action here — creating
        // an alert is not a thing an operator does).
        toolbar={({ shown, total, nameOf, rows }) => {
          // Built here, with the resolver the list already owns. `severity` / `state` / `ack` never
          // read the free-text column's strings, so this costs no name resolution unless something
          // is typed — the property `buildPredicate` gives for free and the old hand-written
          // predicate had to spell out as an early return.
          const cols = activeAlertColumns(t, nameOf, SEVERITIES, SEVERITY_ORDER);
          const counts = Object.fromEntries(
            cols
              .filter((c) => c.filter.kind === 'enum')
              .map((c) => [c.key, facetCounts(rows, cols, filters, c.key, 0)]),
          );
          return (
            <>
              <TableToolbar>
                <FilterButton columns={cols} filters={filters} onOpen={() => setSheet(true)} />
                <ClearFilters
                  columns={cols}
                  filters={filters}
                  onClear={() => setFilters(defaultFilters(cols))}
                />
                <TableSpacer />
                <ResultCount
                  shown={shown}
                  total={narrowed ? total : undefined}
                  noun={t('common:noun.alert', { count: shown })}
                />
              </TableToolbar>
              {/* No header row on this list, so the controls sit in a bar with their names beside
                  them rather than under columns that do not exist (ADR-053 Inc.6 decision E). */}
              <FilterBar
                columns={cols}
                labels={labels}
                filters={filters}
                onChange={setFilters}
                counts={counts}
              />
              {sheet && (
                <MobileFilterSheet
                  columns={cols}
                  labels={labels}
                  filters={filters}
                  onChange={setFilters}
                  counts={counts}
                  onClose={() => setSheet(false)}
                />
              )}
            </>
          );
        }}
        actions={
          canSuppress
            ? (a, subjectName) => {
                // Both actions are node-scoped server-side (a mute names a node, RCA takes a node
                // id), so a pool-coverage alert gets neither rather than a button that only fails.
                const node = subjectNodeId(a);
                if (node === null) return null;
                return (
                  <>
                    {canExplain && (
                      <Button
                        variant="ghost"
                        aria-label={t('rca:actionHint')}
                        title={t('rca:actionHint')}
                        onClick={() => setExplaining({ node, check: a.check })}
                      >
                        {t('rca:action')}
                      </Button>
                    )}
                    <Button
                      variant="ghost"
                      aria-label={t('active.muteHint')}
                      title={t('active.muteHint')}
                      onClick={() => setMuting(muteTargetFromAlert(a, node, subjectName))}
                    >
                      {t('active.mute')}
                    </Button>
                  </>
                );
              }
            : undefined
        }
      />
      {explaining && (
        <RcaModal
          node={explaining.node}
          check={explaining.check}
          onClose={() => setExplaining(null)}
        />
      )}
      {/* The scope is locked to the alert's node, so the dialog never needs the group list. Mutes
          don't change alert state (they suppress *notification*), so there is nothing to reload
          here — the list is SSE-driven and unaffected. */}
      {muting && (
        <AddMuteModal
          initialScope={muting.target}
          initialMetric={muting.metric}
          onClose={() => setMuting(null)}
          onSaved={() => undefined}
        />
      )}
    </div>
  );
}
