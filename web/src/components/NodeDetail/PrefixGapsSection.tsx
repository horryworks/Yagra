// SPDX-License-Identifier: AGPL-3.0-only
// The folder pane's "subnets missing from the IP prefixes" section (ADR-170). Layout only — what
// each line says is decided in `prefixGaps.ts`.

import { useEffect, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { api, ApiError, errMsg } from '../../services/api';
import { useRefreshTick } from '../../lib/refreshTick';
import { EntityName } from '../ui/EntityName';
import { useEntityNames } from '../ui/entityNames';
import { gapReason, moreDevices, summarizeGaps, type PrefixGapReport } from './prefixGaps';

export function PrefixGaps({ groupId }: { groupId: string }) {
  const { t } = useTranslation('nodes');
  const tick = useRefreshTick();
  const { nodeName } = useEntityNames();
  const [report, setReport] = useState<PrefixGapReport | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    api
      .getPrefixGaps(groupId)
      .then((r) => {
        if (cancelled) return;
        setReport(r);
        setError(null);
      })
      .catch((e: unknown) => {
        if (cancelled) return;
        setReport(null);
        setError(
          e instanceof ApiError && e.code === 'too_many_nodes'
            ? t('prefixGaps.tooMany')
            : errMsg(e, t('prefixGaps.err')),
        );
      });
    return () => {
      cancelled = true;
    };
  }, [groupId, tick, t]);

  // Stale content from the previous folder must not be drawn under this one's title.
  const current = report && report.group_id === groupId ? report : null;

  return (
    <section>
      <div className="nd-section-t">{t('prefixGaps.title')}</div>
      {error && <div className="nd-muted">{error}</div>}
      {!error && !current && <div className="nd-muted">{t('prefixGaps.loading')}</div>}
      {current && <Body report={current} nodeName={nodeName} />}
    </section>
  );
}

function Body({
  report,
  nodeName,
}: {
  report: PrefixGapReport;
  nodeName: (id: string) => string;
}) {
  const { t } = useTranslation('nodes');
  const summary = summarizeGaps(report);
  if (summary.kind === 'noData') {
    return <div className="nd-muted">{t('prefixGaps.noData')}</div>;
  }
  const readLine = t('prefixGaps.read', { read: summary.read, total: summary.total });
  return (
    <>
      <div className="nd-gap-read nd-muted">
        {readLine}
        {report.nodes_truncated > 0 && ` ${t('prefixGaps.truncated', { count: report.nodes_truncated })}`}
      </div>
      {summary.kind === 'clean' ? (
        <div className="nd-muted">{t('prefixGaps.clean')}</div>
      ) : (
        <div className="nd-prefixes">
          {report.gaps.map((gap) => {
            const reason = gapReason(gap);
            const more = moreDevices(gap);
            return (
              <div className="nd-gap" key={gap.subnet}>
                <div className="nd-gap-head">
                  <span className="nd-prefix-cidr mono">{gap.subnet}</span>
                  <span className={`nd-gap-kind nd-gap-kind-${gap.kind}`}>
                    {t(reason.key, reason.values)}
                  </span>
                </div>
                <div className="nd-gap-seen">
                  {gap.seen_on.map((s) => (
                    <span className="nd-gap-where" key={`${s.node_id}-${s.ifindex}-${s.ip}`}>
                      <EntityName name={nodeName(s.node_id)} id={s.node_id} />
                      <span className="mono">
                        {s.if_name ? ` ${s.if_name}` : ''} {s.ip}
                      </span>
                    </span>
                  ))}
                  {more > 0 && (
                    <span className="nd-muted">{t('prefixGaps.more', { count: more })}</span>
                  )}
                </div>
              </div>
            );
          })}
        </div>
      )}
    </>
  );
}
