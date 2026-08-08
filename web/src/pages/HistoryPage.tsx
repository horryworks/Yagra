// SPDX-License-Identifier: AGPL-3.0-only
// Alert history (Alerts ▸ History). Append-only lifecycle log from /alerts/history. Each row
// is a transition (fire / clear). MTTR is open→clear in this model (§3.2: ack/response time is
// external and not measured here). Empty in skeleton mode (no persistent store). Rendered with
// the virtualized DataTable on the v2 table standard.

import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { useTranslation } from 'react-i18next';
import {
  alertWhat,
  formatTimestamp,
  severityColorVar,
  severityLabel,
  stateLabel,
} from '../lib/format';
import { api } from '../services/api';
import { alertSubject } from '../lib/alertSubject';
import type { AlertHistoryRow } from '../types/api';
import { PageHeader } from '../components/ui/PageHeader';
import { Badge } from '../components/ui/Badge';
import { EntityName, useEntityNames } from '../components/ui/EntityName';
import { DataTable, type Column } from '../components/ui/DataTable';
import { TableToolbar, TableSpacer, ResultCount } from '../components/ui/TableToolbar';
import { AlertWhatText } from '../widgets/AlertWhatText';

const PAGE_SIZE = 100;

export function HistoryPage() {
  const { t } = useTranslation('alerts');
  const [rows, setRows] = useState<AlertHistoryRow[]>([]);
  const [loading, setLoading] = useState(true);
  const [exhausted, setExhausted] = useState(false);
  // Re-entrancy guard: DataTable fires onReachEnd on every render while the last row is in view,
  // so coalesce overlapping page loads into one in-flight request.
  const loadingMore = useRef(false);
  const { nodeName } = useEntityNames();

  useEffect(() => {
    api
      .listAlertHistory({ limit: PAGE_SIZE })
      .then((page) => {
        setRows(page);
        setExhausted(page.length < PAGE_SIZE);
      })
      .catch(() => undefined)
      .finally(() => setLoading(false));
  }, []);

  // Keyset "load older": fetch the next page strictly older than the last loaded row. The log is
  // append-only and can grow without bound, so we page on scroll instead of one capped fetch.
  const loadMore = useCallback(() => {
    if (loadingMore.current || exhausted) return;
    const last = rows[rows.length - 1];
    if (!last) return;
    loadingMore.current = true;
    api
      .listAlertHistory({ limit: PAGE_SIZE, before: last.recorded_at })
      .then((page) => {
        setRows((cur) => [...cur, ...page]);
        setExhausted(page.length < PAGE_SIZE);
      })
      .catch(() => undefined)
      .finally(() => {
        loadingMore.current = false;
      });
  }, [rows, exhausted]);

  // Columns close over `nodeName`, so rebuild them when the inventory resolves.
  const columns = useMemo<Column<AlertHistoryRow>[]>(
    () => [
      {
        key: 'sev',
        header: t('history.cols.severity'),
        width: '110px',
        render: (r) => (
          <span className="yt-status">
            <span className="yt-status-dot" style={{ background: severityColorVar(r.severity) }} />
            <span className="muted">{severityLabel(r.severity)}</span>
          </span>
        ),
      },
      {
        key: 'node',
        header: t('history.cols.node'),
        width: '1.4fr',
        // A row whose subject is not a node has nothing to resolve through the inventory — see
        // `lib/alertSubject` for why reading `node` without the kind is the mistake to avoid.
        render: (r) => {
          const s = alertSubject(r);
          return s.kind === 'node' ? (
            <EntityName name={nodeName(s.nodeId)} id={s.nodeId} />
          ) : (
            <span title={t('row.poolSubjectHint')}>{t('row.poolSubject', { pool: s.name })}</span>
          );
        },
      },
      {
        key: 'what',
        header: t('history.cols.what'),
        width: '1.6fr',
        render: (r) => <AlertWhatText what={alertWhat(r)} />,
      },
      {
        key: 'state',
        header: t('history.cols.state'),
        width: '120px',
        render: (r) => stateLabel(r.state),
      },
      {
        key: 'phase',
        header: t('history.cols.event'),
        width: '100px',
        render: (r) =>
          r.resolved ? (
            <Badge tone="up">{t('history.phase.cleared')}</Badge>
          ) : (
            <Badge tone="critical">{t('history.phase.fired')}</Badge>
          ),
      },
      {
        // Read-only ack mirrored from the external tool (ADR-015) — Yagra has no ack action.
        key: 'acked',
        header: t('history.cols.acked'),
        width: '120px',
        render: (r) =>
          r.acked ? (
            <span
              className="muted"
              title={t('acked.title', {
                source: r.acked.source,
                by: r.acked.by,
                note: r.acked.note ? t('acked.note', { note: r.acked.note }) : '',
              })}
            >
              {r.acked.source}
            </span>
          ) : (
            <span className="muted">—</span>
          ),
      },
      {
        key: 'at',
        header: t('history.cols.when'),
        width: '1fr',
        render: (r) => <span className="muted">{formatTimestamp(r.at_unix_ms)}</span>,
      },
    ],
    [nodeName, t],
  );

  return (
    <div className="page-fill">
      <PageHeader
        title={t('nav:alerts.history')}
        trail={[{ label: t('nav:sections.alerts') }, { label: t('nav:alerts.history') }]}
        note={t('history.note')}
      />
      <TableToolbar>
        <TableSpacer />
        <ResultCount
          shown={rows.length}
          noun={exhausted ? t('history.transitions') : t('history.transitionsLoaded')}
        />
      </TableToolbar>
      <DataTable
        rows={rows}
        columns={columns}
        rowKey={(r) =>
          `${r.subject_name ?? r.node}|${r.check}|${r.at_unix_ms}|${r.resolved}`
        }
        onReachEnd={loadMore}
        empty={t('history.empty')}
        loading={loading}
      />
    </div>
  );
}
