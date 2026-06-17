// 03 · Performance hotspots. Top ICMP RTT reads the fleet Top-N endpoint (the one rollup
// endpoint built this pass). The now/1h-max window is a per-instance setting, so two copies of
// the widget can show different windows; it lives in the card's view-mode actions slot.

import { Select } from '../../components/ui/Field';
import { formatRtt } from '../../lib/format';
import { api } from '../../services/api';
import type { MetricTopAgg } from '../../types/api';
import { RankedBars } from '../primitives/RankedBars';
import type { WidgetProps } from '../types';
import { usePolled } from '../usePolled';

/** Read the window from instance settings, defaulting to "now". */
function aggOf(settings: WidgetProps['instance']['settings']): MetricTopAgg {
  return settings?.agg === 'max_1h' ? 'max_1h' : 'now';
}

export function TopRttWidget({ instance }: WidgetProps) {
  const agg = aggOf(instance.settings);
  const { data, loading, error } = usePolled(
    () => api.getTopMetrics('icmp_rtt_ms', { agg, limit: 6 }),
    [agg],
  );
  if (error) return <p className="muted">{error}</p>;
  if (loading && !data) return <p className="muted">Loading…</p>;
  const rows = (data ?? []).map((e) => ({
    label: e.name,
    value: e.value,
    valueText: formatRtt(e.value),
  }));
  return <RankedBars rows={rows} empty="No RTT data yet…" />;
}

export function TopRttActions({ instance, setSettings }: WidgetProps) {
  const agg = aggOf(instance.settings);
  return (
    <Select
      value={agg}
      onChange={(e) => setSettings({ agg: e.target.value })}
      aria-label="RTT window"
    >
      <option value="now">Now</option>
      <option value="max_1h">1h max</option>
    </Select>
  );
}
