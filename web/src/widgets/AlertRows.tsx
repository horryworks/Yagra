// SPDX-License-Identifier: AGPL-3.0-only
// Presentational alert rows (worst-first), shared by the dashboard widget and the Active
// alerts triage screen. Each row: severity dot + node name + what fired + root-cause + flapping
// flag + acked pill + age. Triage-only per §3.2 — NO Ack *control* here (Yagra holds no ack
// action; that's external PagerDuty/JSM). The `acked` pill is a READ-ONLY indicator mirrored
// inbound from the external tool (ADR-015) — its tooltip names the tool and the person. The
// optional actions slot is for suppression (Mute) and diagnosis (Explain), never acknowledgement.
//
// Two things the row deliberately does NOT show as raw ids. The node and its root cause resolve to
// names via the shared `useEntityNames` (ui-conventions: the visible primary is always the name,
// the UUID only on hover). The check id has no name to resolve to — it is a one-way hash of
// (node, metric), which is exactly why the metric is persisted alongside the alert (migration
// 0036) — so the row shows what the check *measured* instead: `icmp_rtt_ms above 100 (was 450)`.
//
// The full triage screen (no `limit`) VIRTUALIZES: a major outage can spike active alerts into
// the thousands, and ui-conventions require alert lists to stay usable at tens of thousands of
// rows (windowed DOM, not "render everything"). The capped dashboard-widget path (`limit`) is a
// small fixed slice and renders inline, unchanged.

import { useMemo, useRef } from 'react';
import type { ReactNode } from 'react';
import { useVirtualizer } from '@tanstack/react-virtual';
import { useTranslation } from 'react-i18next';
import { alertSubject } from '../lib/alertSubject';
import { alertWhatOf, formatTimestamp, severityColorVar } from '../lib/format';
import { useViewportMode } from '../lib/viewport';
import { sortedAlerts, useAlertStore } from '../store';
import { EntityName, useEntityNames } from '../components/ui/EntityName';
import { AlertWhatText } from './AlertWhatText';
import './AlertRows.css';

type SortedAlert = ReturnType<typeof sortedAlerts>[number];

/** Per-row actions. Triage-only — never an Ack. Receives the *resolved* node name alongside the
 *  alert: an action that opens a dialog naming the node (Mute) would otherwise need a second
 *  `useEntityNames()` resolver in the page, duplicating the batch this list already runs. */
type RowActions = (alert: SortedAlert, nodeName: string) => ReactNode;

/** Optional row predicate (Alerts ▸ Active). Receives the id→name *resolver* rather than an
 *  already-resolved name, which is the whole reason the filter can live here: `nodeName()` enqueues
 *  every id it is asked about for the next batch request, so a predicate that only needs names when
 *  something is typed can decline to ask. See `pages/activeAlertFilters.ts`. */
type RowFilter = (alert: SortedAlert, nameOf: NameOf) => boolean;

/** Toolbar rendered above the list. A render prop, not a plain node, because the counts it shows
 *  are derived from the store *and* the filter and are known only here — and because it must keep
 *  rendering when the filter matches nothing, or the controls that would undo the filter vanish
 *  along with the rows. */
type ToolbarSlot = (counts: { shown: number; total: number }) => ReactNode;

interface Props {
  /** Cap the number of rows (e.g. dashboard widget). */
  limit?: number;
  actions?: RowActions;
  filter?: RowFilter;
  toolbar?: ToolbarSlot;
  empty?: ReactNode;
  /** Shown when there are alerts but none match `filter`. Distinguishing the two is safe here —
   *  and only here — because the whole set is in the browser; a server-side list must key its empty
   *  state off the filter instead (`lib/filterQuery.ts`). */
  emptyFiltered?: ReactNode;
}

const rowKey = (a: SortedAlert) => `${a.node}|${a.check}|${a.severity}`;

const ROW_PX = 30; // desktop --row-h; virtualizer size hint (rows are measured, so it's only a hint)
const CARD_PX = 96; // taller wrapped row on mobile before it is measured

/** Virtualizer props threaded to a row when it lives inside the windowed scroll region. */
interface VProps {
  index: number;
  translateY: number;
  measureRef: (el: HTMLElement | null) => void;
}

/** id→name resolver, threaded from the one `useEntityNames()` call at the top. Per-row hooks would
 *  mean one inventory fetch per row; this way the visible window batches into a single request. */
type NameOf = (id: string) => string;

// One source of truth for a row's markup — shared by the capped (dashboard) and the virtualized
// (full triage) paths. In the virtualized path the row is absolutely positioned (see AlertRows.css)
// and carries the `data-index` + measure ref + translate offset the virtualizer drives.
function AlertRow({
  a,
  actions,
  nodeName,
  v,
}: {
  a: SortedAlert;
  actions?: RowActions;
  nodeName: NameOf;
  v?: VProps;
}) {
  const { t } = useTranslation('alerts');
  const subject = alertSubject(a);
  return (
    <div
      className="alertrow"
      data-index={v?.index}
      ref={v?.measureRef}
      style={v ? { transform: `translateY(${v.translateY}px)` } : undefined}
    >
      <span className="alertrow-dot" style={{ background: severityColorVar(a.severity) }} />
      <span className="alertrow-node">
        {subject.kind === 'node' ? (
          <EntityName name={nodeName(subject.nodeId)} id={subject.nodeId} />
        ) : (
          // A pool name is already the human-readable thing — there is no inventory row to resolve
          // it through, and the label is what tells an operator this row is about Yagra's own
          // polling rather than about a device.
          <span title={t('row.poolSubjectHint')}>{t('row.poolSubject', { pool: subject.name })}</span>
        )}
      </span>
      {/* The check id is still the handle the API takes (RCA, ack), so keep it recoverable on
          hover even though it is no longer the cell's text. */}
      <span className="alertrow-check" title={a.check}>
        <AlertWhatText what={alertWhatOf(a)} />
      </span>
      {a.root_cause && (
        <span className="alertrow-cause muted">
          ← <EntityName name={nodeName(a.root_cause)} id={a.root_cause} />
        </span>
      )}
      {a.flapping && <span className="alertrow-flap">{t('row.flapping')}</span>}
      {a.acked && (
        <span
          className="alertrow-acked"
          title={t('acked.title', {
            source: a.acked.source,
            by: a.acked.by,
            note: a.acked.note ? t('acked.note', { note: a.acked.note }) : '',
          })}
        >
          {t('row.acked')}
        </span>
      )}
      <span className="alertrow-time muted">{formatTimestamp(a.at_unix_ms)}</span>
      {actions && (
        <span className="alertrow-actions">
          {actions(a, subject.kind === 'node' ? nodeName(subject.nodeId) : subject.name)}
        </span>
      )}
    </div>
  );
}

// Full triage list: window the DOM with @tanstack/react-virtual (same idiom as the shared
// DataTable) so only the visible slice mounts. Rows are absolutely positioned inside a bounded,
// scrollable region and measured so both the fixed desktop row and the taller wrapped mobile row
// position correctly.
function VirtualAlertRows({
  alerts,
  actions,
  nodeName,
}: {
  alerts: SortedAlert[];
  actions?: RowActions;
  nodeName: NameOf;
}) {
  const scrollRef = useRef<HTMLDivElement>(null);
  const cardMode = useViewportMode() === 'mobile';
  const virtualizer = useVirtualizer({
    count: alerts.length,
    getScrollElement: () => scrollRef.current,
    estimateSize: () => (cardMode ? CARD_PX : ROW_PX),
    overscan: 12,
  });

  return (
    <div className="alertrows-scroll" ref={scrollRef}>
      <div className="alertrows-body" style={{ height: virtualizer.getTotalSize() }}>
        {virtualizer.getVirtualItems().map((vi) => {
          const a = alerts[vi.index];
          return (
            <AlertRow
              key={rowKey(a)}
              a={a}
              actions={actions}
              nodeName={nodeName}
              v={{
                index: vi.index,
                translateY: vi.start,
                measureRef: virtualizer.measureElement,
              }}
            />
          );
        })}
      </div>
    </div>
  );
}

export function AlertRows({ limit, actions, filter, toolbar, empty, emptyFiltered }: Props) {
  const { t } = useTranslation('alerts');
  const alerts = useAlertStore((s) => s.alerts);
  // One resolver for the whole list. `nodeName` enqueues only the ids the current render actually
  // referenced — i.e. the visible window — and batch-resolves them after the commit, so this stays
  // one request per scroll burst even at the thousands-of-rows the triage screen must survive.
  const { nodeName } = useEntityNames();
  // Sort once per store change, not on every SSE-driven render.
  const all = useMemo(() => sortedAlerts(alerts), [alerts]);
  // Filter after sorting, before windowing — the virtualizer must count only the rows that will be
  // drawn. Recomputes when `nodeName` gains a batch of resolutions, which is what lets a search on
  // a node name settle rather than matching only the ids resolved so far.
  const shown = useMemo(
    () => (filter ? all.filter((a) => filter(a, nodeName)) : all),
    [all, filter, nodeName],
  );

  const body =
    all.length === 0 ? (
      <p className="alertrows-empty">{empty ?? t('active.empty')}</p>
    ) : shown.length === 0 ? (
      <p className="alertrows-empty">{emptyFiltered ?? empty ?? t('active.empty')}</p>
    ) : limit ? (
      // Capped use (dashboard widget): a small fixed slice — render inline so the widget keeps its
      // natural height (no bounded scroll region).
      <div className="alertrows">
        {shown.slice(0, limit).map((a) => (
          <AlertRow key={rowKey(a)} a={a} actions={actions} nodeName={nodeName} />
        ))}
      </div>
    ) : (
      <VirtualAlertRows alerts={shown} actions={actions} nodeName={nodeName} />
    );

  // No toolbar (every dashboard widget) ⇒ the same single element as before, not a fragment.
  if (!toolbar) return body;
  return (
    <>
      {toolbar({ shown: shown.length, total: all.length })}
      {body}
    </>
  );
}
