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
import { severityLabel, stateLabel } from '../lib/format';
import { SEVERITY_ORDER } from '../lib/nodeState';
import { muteTargetFromAlert, type AlertMuteSeed } from '../lib/suppression';
import { SEVERITIES } from '../types/api';
import { PageHeader } from '../components/ui/PageHeader';
import { Button } from '../components/ui/Button';
import {
  FilterSelect,
  ResultCount,
  SearchInput,
  TableSpacer,
  TableToolbar,
} from '../components/ui/TableToolbar';
import { RcaModal } from '../components/Rca/RcaModal';
import { AddMuteModal } from '../components/suppression/AddMuteModal';
import { AlertRows } from '../widgets/AlertRows';
import {
  ACK_STATES,
  isFiltered,
  matchesActiveAlert,
  readFilters,
  writeFilters,
  type ActiveAlertFilters,
  type FilterableAlert,
  type NameOf,
} from './activeAlertFilters';

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
  const [params, setParams] = useSearchParams();
  const filters = useMemo(() => readFilters(params, SEVERITIES, SEVERITY_ORDER), [params]);
  const set = <K extends keyof ActiveAlertFilters>(key: K, value: ActiveAlertFilters[K]) => {
    const p = new URLSearchParams(params);
    writeFilters(p, { ...filters, [key]: value });
    setParams(p, { replace: true });
  };

  // No debounce on the search box, unlike every other one in the tree — and the difference is the
  // point. Those debounce because a keystroke becomes a request; here the whole alert set is
  // already in the browser (SSE), so a keystroke is an in-memory filter and delaying it would only
  // make the screen feel slow. The one request a term can trigger is the node-name batch, which
  // resolves once and is cached from the second character on.
  const filter = useCallback(
    (a: FilterableAlert, nameOf: NameOf) => matchesActiveAlert(a, filters, nameOf),
    [filters],
  );

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
        toolbar={({ shown, total }) => (
          <TableToolbar>
            <SearchInput
              value={filters.q}
              onChange={(v) => set('q', v)}
              placeholder={t('active.searchPlaceholder')}
              ariaLabel={t('active.searchAria')}
            />
            <FilterSelect
              value={filters.severity}
              onChange={(v) => set('severity', v)}
              options={SEVERITIES.map((s) => ({ value: s, label: severityLabel(s) }))}
              allLabel={t('history.filter.allSeverities')}
              ariaLabel={t('history.filter.severityAria')}
            />
            <FilterSelect
              value={filters.state}
              onChange={(v) => set('state', v)}
              options={SEVERITY_ORDER.map((s) => ({ value: s, label: stateLabel(s) }))}
              allLabel={t('history.filter.allStates')}
              ariaLabel={t('history.filter.stateAria')}
            />
            <FilterSelect
              value={filters.ack}
              onChange={(v) => set('ack', v)}
              // ACK_STATES, not ACK_FILTERS: the `''` option is FilterSelect's own, and offering it
              // twice would put two "no filter" rows in the same dropdown.
              options={ACK_STATES.map((v) => ({ value: v, label: t(`active.ack.${v}`) }))}
              allLabel={t('active.ack.all')}
              ariaLabel={t('active.ack.aria')}
            />
            <TableSpacer />
            <ResultCount
              shown={shown}
              total={isFiltered(filters) ? total : undefined}
              noun={t('common:noun.alert', { count: shown })}
            />
          </TableToolbar>
        )}
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
