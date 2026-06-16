// Interfaces tab of the unified node detail. A flat, scannable table (Interface · Oper · Speed ·
// In/Out) over the node's discovered interfaces; clicking a row expands its throughput / error
// trend charts below the table (the master-detail of the old page, preserved but narrow-pane
// friendly). The list refreshes on an interval. Error counts aren't in the row shape, so the table
// shows real reachable data only — the per-interface error chart lives in the expanded detail.

import { useEffect, useState } from 'react';
import { api } from '../../services/api';
import { formatBps, formatSi, formatUtil } from '../../lib/format';
import type { InterfaceRow, InterfaceSeries } from '../../types/api';
import { StatusDot } from '../ui/StatusDot';
import { Button } from '../ui/Button';
import { TextInput } from '../ui/Field';
import { MetricChart } from '../MetricChart/MetricChart';
import { operState, RANGES } from './OverviewTab';

const STATUS_REFRESH_MS = 15_000;
const CHART_IN = '#4f8cff';
const CHART_OUT = '#34d399';

/** Human oper label from ifOperStatus (1 = up). */
function operLabel(oper: number | null): string {
  if (oper == null) return 'unknown';
  return oper === 1 ? 'up' : 'down';
}

interface Props {
  nodeId: string;
  /** Interface rows (loaded + interval-refreshed by the orchestrator, shared with the tab badge). */
  rows: InterfaceRow[];
  loaded: boolean;
  error: string | null;
}

export function InterfacesTab({ nodeId, rows, loaded, error }: Props) {
  const [filter, setFilter] = useState('');
  const [selected, setSelected] = useState<number | null>(null);

  const q = filter.trim().toLowerCase();
  const shown = q
    ? rows.filter(
        (r) =>
          (r.if_name ?? `if${r.ifindex}`).toLowerCase().includes(q) ||
          (r.if_alias ?? '').toLowerCase().includes(q),
      )
    : rows;
  const up = rows.filter((r) => r.oper_status === 1).length;
  const selectedRow = rows.find((r) => r.ifindex === selected) ?? null;

  if (loaded && rows.length === 0) {
    return (
      <div className="nd-tabpad">
        {error && <p className="form-error">{error}</p>}
        <p className="nd-muted">
          No interfaces discovered yet. They appear after an SNMP table walk runs — the node needs a
          bound SNMP credential and a table metric in its collection set.
        </p>
      </div>
    );
  }

  return (
    <div className="nd-if">
      <div className="nd-if-toolbar">
        <span className="nd-if-summary">
          <b>{up}</b> / {rows.length} up
        </span>
        <TextInput
          className="nd-if-filter"
          value={filter}
          onChange={(e) => setFilter(e.target.value)}
          placeholder="Filter interfaces…"
        />
      </div>
      {error && <p className="form-error nd-tabpad">{error}</p>}
      <div className="nd-if-table">
        <div className="nd-if-head">
          <div className="nd-if-h">Interface</div>
          <div className="nd-if-h">Oper</div>
          <div className="nd-if-h">Speed</div>
          <div className="nd-if-h right">In / Out</div>
        </div>
        {shown.map((r) => {
          const down = r.oper_status != null && r.oper_status !== 1;
          return (
            <button
              type="button"
              key={r.ifindex}
              className={`nd-if-row${r.ifindex === selected ? ' selected' : ''}${down ? ' down' : ''}${
                r.stale ? ' stale' : ''
              }`}
              onClick={() => setSelected((cur) => (cur === r.ifindex ? null : r.ifindex))}
            >
              <span className="nd-if-id">
                <span className="nd-if-name mono">{r.if_name ?? `if${r.ifindex}`}</span>
                {r.if_alias && <span className="nd-if-sub">{r.if_alias}</span>}
              </span>
              <span className="nd-if-oper">
                <StatusDot state={operState(r.oper_status)} withLabel={false} />
                {operLabel(r.oper_status)}
              </span>
              <span className="nd-if-cell mono">{formatBps(r.if_speed_bps)}</span>
              <span className="nd-if-cell right">
                {r.oper_status === 1 ? `${formatBps(r.in_bps)} / ${formatBps(r.out_bps)}` : '—'}
              </span>
            </button>
          );
        })}
        {shown.length === 0 && <p className="nd-muted nd-tabpad">No interfaces match “{filter}”.</p>}
      </div>

      {selectedRow && <InterfaceDetail nodeId={nodeId} row={selectedRow} />}
    </div>
  );
}

/** Charts + stats for one interface: throughput (In/Out bps) and errors (In/Out per second) over a
 *  selectable window, plus the current stat tiles. */
function InterfaceDetail({ nodeId, row }: { nodeId: string; row: InterfaceRow }) {
  const [rangeSecs, setRangeSecs] = useState(RANGES[0].secs);
  const [series, setSeries] = useState<InterfaceSeries | null>(null);

  useEffect(() => {
    let cancelled = false;
    const load = () => {
      const to = Math.floor(Date.now() / 1000);
      api
        .getInterfaceSeries(nodeId, row.ifindex, { from: to - rangeSecs, to })
        .then((s) => !cancelled && setSeries(s))
        .catch(() => undefined);
    };
    setSeries(null);
    load();
    const id = setInterval(load, STATUS_REFRESH_MS);
    return () => {
      cancelled = true;
      clearInterval(id);
    };
  }, [nodeId, row.ifindex, rangeSecs]);

  const ts = series?.timestamps ?? [];
  const hasData = ts.length > 0;

  return (
    <div className="nd-ifdetail">
      <div className="nd-ifdetail-head">
        <StatusDot state={operState(row.oper_status)} withLabel={false} />
        <span className="mono nd-ifdetail-name">{row.if_name ?? `if${row.ifindex}`}</span>
        {row.if_alias && <span className="nd-muted nd-ifdetail-alias">{row.if_alias}</span>}
        <div className="nd-windows nd-ifdetail-windows">
          {RANGES.map((r) => (
            <Button
              key={r.secs}
              variant={rangeSecs === r.secs ? 'primary' : 'outline'}
              onClick={() => setRangeSecs(r.secs)}
            >
              {r.label}
            </Button>
          ))}
        </div>
      </div>

      <div className="nd-ifdetail-section">
        <div className="nd-section-t">
          Throughput (In / Out, <span className="nd-unit">bps</span>)
        </div>
        {hasData ? (
          <MetricChart
            title=""
            timestamps={ts}
            yFormat={formatSi}
            series={[
              { label: 'In', values: series!.in_bps, color: CHART_IN },
              { label: 'Out', values: series!.out_bps, color: CHART_OUT },
            ]}
          />
        ) : (
          <p className="nd-muted">No data for this window yet…</p>
        )}
      </div>

      <div className="nd-ifdetail-section">
        <div className="nd-section-t">
          Errors (In / Out, <span className="nd-unit">/s</span>)
        </div>
        {hasData ? (
          <MetricChart
            title=""
            timestamps={ts}
            yFormat={formatSi}
            series={[
              { label: 'In', values: series!.in_errors, color: CHART_IN },
              { label: 'Out', values: series!.out_errors, color: CHART_OUT },
            ]}
          />
        ) : (
          <p className="nd-muted">No data for this window yet…</p>
        )}
      </div>

      <div className="nd-ifdetail-stats">
        <div>
          <span className="nd-muted">In</span> {formatBps(row.in_bps)}
        </div>
        <div>
          <span className="nd-muted">Out</span> {formatBps(row.out_bps)}
        </div>
        <div>
          <span className="nd-muted">In %</span> {formatUtil(row.in_util_pct)}
        </div>
        <div>
          <span className="nd-muted">Out %</span> {formatUtil(row.out_util_pct)}
        </div>
        <div>
          <span className="nd-muted">Speed</span> {formatBps(row.if_speed_bps)}
        </div>
      </div>
    </div>
  );
}
