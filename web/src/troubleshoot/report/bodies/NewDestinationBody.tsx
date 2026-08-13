// SPDX-License-Identifier: AGPL-3.0-only
// New Destination report body (flow monitoring, ADR-031).
//
// The ONLY report with two genuinely different row shapes in one job: a new destination **AS** and a
// new destination **port**. They share no field but bytes, and the backend caps port findings at
// score 74 so a port row can never reach `warn` — so mixing them in one list with one severity
// filter would be meaningless. Hence two independently-headed sections with their own Top-N, and a
// segmented control to focus either one.
//
// This report never renders `finding.duration`: the backend fills it with the AS *name* or the byte
// count depending on whether the ASN resolved, so it isn't a stable field. Everything comes from the
// tested `classifyNewDestination` classifier instead.

import { useMemo, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { Card } from '../../../components/ui/Card';
import { RankedBars, type RankedRow } from '../../../dashboard/primitives/RankedBars';
import { formatAsn, formatBytes } from '../../../lib/format';
import { portLabel } from '../../../lib/flowLabels';
import { Segmented } from '../../../components/ui/Segmented';
import { EmptyList, FindingRow, MonoLine, NodeRef, RightRail } from '../kit';
import { splitDestinations } from '../newDestination';
import type { ReportBodyProps } from '../types';
import type { AnalysisFinding } from '../../../types/api';

function DestRow({
  finding,
  label,
  sub,
  bytes,
}: {
  finding: AnalysisFinding;
  label: string;
  sub?: string;
  bytes: number;
}) {
  return (
    <FindingRow
      finding={finding}
      identity={
        <>
          <MonoLine title={label}>{label}</MonoLine>
          {sub && <span className="ts-anom-kind">{sub}</span>}
          <span className="ts-anom-metric">
            <NodeRef finding={finding} />
          </span>
        </>
      }
      right={<RightRail when={formatBytes(bytes)} />}
    />
  );
}

export function NewDestinationBody({ findings }: ReportBodyProps) {
  const { t } = useTranslation('troubleshoot');
  const [show, setShow] = useState<'both' | 'as' | 'ports'>('both');
  const { as, ports } = useMemo(() => splitDestinations(findings), [findings]);

  const asBars = useMemo<RankedRow[]>(
    () =>
      as.slice(0, 10).map((x, i) => ({
        label: `${i + 1}. ${formatAsn(x.dest.asn, x.dest.name) ?? `AS${x.dest.asn}`}`,
        value: x.dest.bytes,
        valueText: formatBytes(x.dest.bytes),
        color: 'var(--series-6)',
      })),
    [as],
  );
  const portBars = useMemo<RankedRow[]>(
    () =>
      ports.slice(0, 10).map((x, i) => ({
        label: `${i + 1}. ${portLabel(x.dest.port)}`,
        value: x.dest.bytes,
        valueText: formatBytes(x.dest.bytes),
        color: 'var(--series-3)',
      })),
    [ports],
  );

  // Nothing classifiable at all (including a job that found only unknown-AS rows).
  if (!as.length && !ports.length) return <EmptyList total={0} />;

  return (
    <>
      <div className="ts-res-toolbar">
        <Segmented
          ariaLabel={t('report.new_destination.showAria')}
          value={show}
          onChange={(v) => setShow(v as 'both' | 'as' | 'ports')}
          options={[
            { value: 'both', label: t('report.new_destination.show.both') },
            { value: 'as', label: t('report.new_destination.show.as') },
            { value: 'ports', label: t('report.new_destination.show.ports') },
          ]}
        />
      </div>

      {show !== 'ports' && (
        <section className="tsr-section">
          <h3 className="ts-section-label">
            {t('report.new_destination.asHeading', { count: as.length })}
          </h3>
          {as.length ? (
            <>
              <Card title={t('report.new_destination.topAs')}>
                <RankedBars rows={asBars} />
              </Card>
              <div className="ts-anoms">
                {as.map(({ finding, dest }) => (
                  <DestRow
                    key={finding.id}
                    finding={finding}
                    label={formatAsn(dest.asn, dest.name) ?? `AS${dest.asn}`}
                    sub={t('report.new_destination.newAs')}
                    bytes={dest.bytes}
                  />
                ))}
              </div>
            </>
          ) : (
            <div className="ts-empty-note">{t('report.new_destination.noAs')}</div>
          )}
        </section>
      )}

      {show !== 'as' && (
        <section className="tsr-section">
          <h3 className="ts-section-label">
            {t('report.new_destination.portHeading', { count: ports.length })}
          </h3>
          {ports.length ? (
            <>
              <Card title={t('report.new_destination.topPorts')}>
                <RankedBars rows={portBars} />
              </Card>
              <div className="ts-anoms">
                {ports.map(({ finding, dest }) => (
                  <DestRow
                    key={finding.id}
                    finding={finding}
                    label={portLabel(dest.port)}
                    sub={t('report.new_destination.newPort')}
                    bytes={dest.bytes}
                  />
                ))}
              </div>
            </>
          ) : (
            <div className="ts-empty-note">{t('report.new_destination.noPorts')}</div>
          )}
        </section>
      )}
    </>
  );
}
