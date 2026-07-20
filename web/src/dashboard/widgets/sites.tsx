// SPDX-License-Identifier: AGPL-3.0-only
// 04 · Sites & topology widgets. Each computes from the server-side per-group health rollup
// (`useGroupSummary`, A-1) joined to the node-group tree — so they aggregate the WHOLE fleet per
// group, not the first page of a node slice (the old `useNodes()` path under-counted every group
// past the first 100 nodes). Site matrix = a tile per group (worst direct-member state + up/total);
// region rollup = % healthy per top-level group summing descendants; geo map = a pin per placed
// group coloured by its direct members' worst state.

import { useTranslation } from 'react-i18next';
import { stateColorVar, stateLabel } from '../../lib/format';
import { api } from '../../services/api';
import { RankedBars } from '../primitives/RankedBars';
import { useGroupSummary } from '../useGroupSummary';
import { usePolled } from '../usePolled';
import { countsTotal, topLevelRollupFromCounts, worstStateFromCounts } from './util';

export function SiteHealthMatrixWidget() {
  const { t } = useTranslation('dashboard');
  const { summary, loading, error } = useGroupSummary();
  const groups = usePolled(() => api.listNodeGroups(), []);
  if (error || groups.error) return <p className="muted">{t('widgets.siteMatrix.error')}</p>;
  if ((loading && !summary) || (groups.loading && !groups.data)) {
    return <p className="muted">{t('common:loading')}</p>;
  }
  const counts = summary?.groups ?? {};
  const tiles = (groups.data ?? [])
    .map((g) => ({ id: g.id, name: g.name, c: counts[g.id] }))
    .filter((tile) => tile.c != null && countsTotal(tile.c) > 0);
  if (tiles.length === 0) return <p className="muted">{t('widgets.siteMatrix.empty')}</p>;
  return (
    <div className="matrix">
      {tiles.map((tile) => {
        const c = tile.c!;
        const worst = worstStateFromCounts(c);
        const up = c.ok ?? 0;
        const total = countsTotal(c);
        return (
          <div className="matrix-tile" key={tile.id} title={`${tile.name}: ${stateLabel(worst)}`}>
            <span className="matrix-bar" style={{ background: stateColorVar(worst) }} />
            <span className="matrix-info">
              <span className="matrix-name">{tile.name}</span>
              <span className="matrix-count mono">
                {up}/{total}
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
  const { summary, loading, error } = useGroupSummary();
  const groups = usePolled(() => api.listNodeGroups(), []);
  if (error || groups.error) return <p className="muted">{t('widgets.regionRollup.error')}</p>;
  if ((loading && !summary) || (groups.loading && !groups.data)) {
    return <p className="muted">{t('common:loading')}</p>;
  }
  const regions = topLevelRollupFromCounts(summary?.groups ?? {}, groups.data ?? []);
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
  const { summary, loading, error } = useGroupSummary();
  const groups = usePolled(() => api.listNodeGroups(), []);
  if (error || groups.error) return <p className="muted">{t('widgets.geoMap.error')}</p>;
  if ((loading && !summary) || (groups.loading && !groups.data)) {
    return <p className="muted">{t('common:loading')}</p>;
  }
  const counts = summary?.groups ?? {};
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
        const c = counts[g.id];
        const worst = c ? worstStateFromCounts(c) : 'ok';
        const count = c ? countsTotal(c) : 0;
        const x = 6 + norm(g.longitude as number, minLon, maxLon) * 88;
        const y = 6 + (1 - norm(g.latitude as number, minLat, maxLat)) * 88;
        return (
          <span
            key={g.id}
            className="geo-pin"
            title={t('widgets.geoMap.pinTitle', {
              name: g.name,
              state: stateLabel(worst),
              count,
            })}
            style={{ left: `${x}%`, top: `${y}%`, background: stateColorVar(worst) }}
          />
        );
      })}
    </div>
  );
}
