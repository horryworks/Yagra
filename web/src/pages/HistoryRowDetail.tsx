// SPDX-License-Identifier: AGPL-3.0-only
// One opened row of Alerts ▸ History (ADR-270): the outage it was grouped into, the nodes an
// outage held, and what that transition sent. What is asked for and kept is `historyRow.ts`.

import type { ReactNode } from 'react';
import { useTranslation } from 'react-i18next';
import { api } from '../services/api';
import type { AlertHistoryRow, DeliveryRow } from '../types/api';
import { Badge } from '../components/ui/Badge';
import { EntityName } from '../components/ui/EntityName';
import { Facts, type Fact } from '../components/ui/Facts';
import { Stack } from '../components/ui/Page';
import { TimeCell } from '../components/ui/tableCells';
import { useLoad } from '../lib/useLoad';
import { useCan } from '../store';
import { nodeHref } from '../lib/entityHref';
import { channelLabel, resultTone, routeText } from './deliveryLogQuery';
import { DeliveryDetail } from './DeliveryLog';
import './DeliveryLog.css';
import {
  deliveriesOf,
  deliveryQueryOf,
  groupedQueryOf,
  groupedUnderOf,
  mayHoldGrouped,
  memberNodes,
  membersCut,
} from './historyRow';

export function HistoryRowDetail({
  row,
  rows,
  nodeName,
}: {
  row: AlertHistoryRow;
  /** Every row on screen, for the same check's next transition (`deliveriesOf`). */
  rows: readonly AlertHistoryRow[];
  nodeName: (id: string) => string;
}) {
  const { t } = useTranslation('alerts');
  // The delivery log's own wording, as its tab draws it.
  const { t: tc } = useTranslation('alertsConfig');
  // The delivery log is read with ManageSystem, as it always was: it names where pages went.
  const canDeliveries = useCan('manage_system');
  const under = groupedUnderOf(row);
  const holds = mayHoldGrouped(row);
  const unread = t('history.detail.unread');
  const members = useLoad(() => api.listAlertHistory(groupedQueryOf(row)), [row.id], {
    initial: [] as AlertHistoryRow[],
    enabled: holds,
    errorFallback: unread,
  });
  const query = deliveryQueryOf(row, rows);
  const asks = canDeliveries && query !== null;
  const sent = useLoad(
    () => (query ? api.listNotificationDeliveries(query) : Promise.resolve([] as DeliveryRow[])),
    [row.id],
    { initial: [] as DeliveryRow[], enabled: asks, errorFallback: unread },
  );
  // Nothing is said about a part still being read: "none sent" before the answer is a wrong answer.
  const pending = (holds && members.loading) || (asks && sent.loading);
  const failed = (l: { block: unknown; error: string | null }) => l.block !== null || l.error !== null;

  const nodeLink = (id: string) => <EntityName key={id} name={nodeName(id)} id={id} to={nodeHref(id)} />;
  const nodes = memberNodes(members.data);
  const items: Fact[] = [];
  if (under) items.push({ key: 'under', label: t('history.detail.groupedUnder'), value: nodeLink(under) });
  if (holds && !members.loading) {
    if (failed(members)) {
      items.push({
        key: 'grouped',
        label: t('history.detail.groupedNodes'),
        value: <span className="muted">{members.error ?? unread}</span>,
      });
    } else if (nodes.length > 0) {
      items.push({
        key: 'grouped',
        label: t('history.detail.grouped', { count: nodes.length }),
        value: (
          <Stack gap={4}>
            {nodes.map((n) => nodeLink(n))}
            {/* The server stopped at its limit: there are more than are listed. */}
            {membersCut(members.data) && <span className="muted">…</span>}
          </Stack>
        ),
      });
    }
  }
  if (asks && !sent.loading) {
    const mine = deliveriesOf(row, rows, sent.data);
    let value: ReactNode;
    if (failed(sent)) {
      value = <span className="muted">{sent.error ?? unread}</span>;
    } else if (mine.length > 0) {
      value = (
        <Stack gap={8}>
          {mine.map((d) => (
            <Stack key={d.id} gap={4}>
              <span>
                <TimeCell iso={d.at} /> {channelLabel(tc, d)}{' '}
                <Badge tone="neutral">{tc(`routing.log.event.${d.event}`)}</Badge>{' '}
                <Badge tone={resultTone(d.result)}>
                  {tc(`routing.log.result.${d.result}`)}
                </Badge>{' '}
                <span className="muted">{routeText(tc, d)}</span>
              </span>
              {d.result === 'failed' && <DeliveryDetail row={d} nested />}
            </Stack>
          ))}
        </Stack>
      );
    } else {
      value = (
        <span className="muted">{under ? t('history.detail.notSentGrouped') : t('history.detail.noneSent')}</span>
      );
    }
    items.push({ key: 'sent', label: t('history.detail.notifications'), value });
  }
  if (items.length === 0) {
    if (pending) return null;
    return (
      <div className="routing-log-detail">
        <span className="muted">{t('history.detail.nothing')}</span>
      </div>
    );
  }
  // The same frame as an opened row of the Notifications view beside it.
  return (
    <div className="routing-log-detail">
      <Facts items={items} />
    </div>
  );
}
