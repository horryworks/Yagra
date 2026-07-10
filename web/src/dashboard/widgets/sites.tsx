// 04 · Sites & topology widgets. Both are computed entirely client-side from the shared node
// list joined to the node-group tree — no new endpoint. Site matrix = a tile per group (worst
// member state + up/total); region rollup = % healthy per top-level group, as ranked bars.

import { useTranslation } from 'react-i18next';
import { stateColorVar, stateLabel } from '../../lib/format';
import { api } from '../../services/api';
import { RankedBars } from '../primitives/RankedBars';
import { useNodes } from '../useNodes';
import { usePolled } from '../usePolled';
import { topLevelRollup, worstState } from './util';

export function SiteHealthMatrixWidget() {
  const { t } = useTranslation('dashboard');
  const { nodes, loading, error } = useNodes();
  const groups = usePolled(() => api.listNodeGroups(), []);
  if (error || groups.error) return <p className="muted">{t('widgets.siteMatrix.error')}</p>;
  if ((loading && nodes.length === 0) || (groups.loading && !groups.data)) {
    return <p className="muted">{t('common:loading')}</p>;
  }
  const tiles = (groups.data ?? [])
    .map((g) => ({ id: g.id, name: g.name, members: nodes.filter((n) => n.group_id === g.id) }))
    .filter((tile) => tile.members.length > 0);
  if (tiles.length === 0) return <p className="muted">{t('widgets.siteMatrix.empty')}</p>;
  return (
    <div className="matrix">
      {tiles.map((tile) => {
        const worst = worstState(tile.members.map((m) => m.state));
        const up = tile.members.filter((m) => m.state === 'ok').length;
        return (
          <div className="matrix-tile" key={tile.id} title={`${tile.name}: ${stateLabel(worst)}`}>
            <span className="matrix-bar" style={{ background: stateColorVar(worst) }} />
            <span className="matrix-info">
              <span className="matrix-name">{tile.name}</span>
              <span className="matrix-count mono">
                {up}/{tile.members.length}
              </span>
            </span>
          </div>
        );
      })}
    </div>
  );
}

export function RegionRollupWidget() {
  const { t } = useTranslation('dashboard');
  const { nodes, loading, error } = useNodes();
  const groups = usePolled(() => api.listNodeGroups(), []);
  if (error || groups.error) return <p className="muted">{t('widgets.regionRollup.error')}</p>;
  if ((loading && nodes.length === 0) || (groups.loading && !groups.data)) {
    return <p className="muted">{t('common:loading')}</p>;
  }
  const regions = topLevelRollup(nodes, groups.data ?? []);
  const rows = regions
    .sort((a, b) => a.pct - b.pct) // worst-first so problems surface at the top
    .map((r) => ({
      label: r.name,
      value: r.pct,
      valueText: `${r.pct}% · ${r.up}/${r.total}`,
      // % up reads as health — use the status channel, green→amber→red by threshold.
      color:
        r.pct >= 90
          ? 'var(--status-up)'
          : r.pct >= 60
            ? 'var(--status-warning)'
            : 'var(--status-critical)',
    }));
  return <RankedBars rows={rows} max={100} empty={t('widgets.regionRollup.empty')} />;
}

export function GeoMapWidget() {
  const { t } = useTranslation('dashboard');
  const { nodes, loading, error } = useNodes();
  const groups = usePolled(() => api.listNodeGroups(), []);
  if (error || groups.error) return <p className="muted">{t('widgets.geoMap.error')}</p>;
  if ((loading && nodes.length === 0) || (groups.loading && !groups.data)) {
    return <p className="muted">{t('common:loading')}</p>;
  }
  const placed = (groups.data ?? []).filter(
    (g) => g.latitude != null && g.longitude != null,
  );
  if (placed.length === 0) {
    return <p className="muted">{t('widgets.geoMap.empty')}</p>;
  }
  // Normalize lon→x, lat→y into the box (lat inverted so north is up). A small pad avoids pins
  // sitting on the edge; a single point lands in the centre.
  const lats = placed.map((g) => g.latitude as number);
  const lons = placed.map((g) => g.longitude as number);
  const minLat = Math.min(...lats);
  const maxLat = Math.max(...lats);
  const minLon = Math.min(...lons);
  const maxLon = Math.max(...lons);
  const norm = (v: number, lo: number, hi: number) => (hi > lo ? (v - lo) / (hi - lo) : 0.5);
  return (
    <div className="geo" role="img" aria-label={t('widgets.geoMap.aria')}>
      {placed.map((g) => {
        const members = nodes.filter((n) => n.group_id === g.id);
        const worst = worstState(members.map((m) => m.state));
        const x = 6 + norm(g.longitude as number, minLon, maxLon) * 88;
        const y = 6 + (1 - norm(g.latitude as number, minLat, maxLat)) * 88;
        return (
          <span
            key={g.id}
            className="geo-pin"
            title={t('widgets.geoMap.pinTitle', {
              name: g.name,
              state: stateLabel(worst),
              count: members.length,
            })}
            style={{ left: `${x}%`, top: `${y}%`, background: stateColorVar(worst) }}
          />
        );
      })}
    </div>
  );
}
