// SPDX-License-Identifier: AGPL-3.0-only
// The clock's world map (ADR-221 Inc.2): one point per zone in `zoneMapPoints.json`, the night side
// shaded as Topology ▸ Geo map shades it, and a click choosing the nearest point. The coastline is
// the Geo map's own, fetched the first time the map opens; until it arrives the points and the
// night side are drawn on the bare ocean, so a zone can be chosen either way.
//
// What a point is called, which one a click lands on and which share a clock are `lib/zoneMap.ts`.

import { useEffect, useRef, useState } from 'react';
import { MAP_WIDTH, project } from '../../pages/geoProjection';
import { twilightPaths } from '../../pages/geoDayNight';
import { loadOutline, type Outline } from '../../lib/worldOutlineLoad';
import { clockText, zoneAbbreviation } from '../../lib/timeZone';
import {
  ZONE_MAP_BOTTOM,
  ZONE_MAP_POINTS,
  ZONE_MAP_TOP,
  countryName,
  nearestPoint,
  pointLabel,
  pointMatches,
  sameOffset,
  type ZoneMapPoint,
} from '../../lib/zoneMap';

const HEIGHT = ZONE_MAP_BOTTOM - ZONE_MAP_TOP;
/** How far from a point a click still chooses it, and a hover still names it, in screen pixels. */
const CLICK_PX = 40;
const HOVER_PX = 14;

interface Props {
  /** The zone the clock is showing. */
  zone: string;
  now: number;
  query: string;
  lang: string;
  onPick: (zone: string) => void;
}

export function ZoneMap({ zone, now, query, lang, onPick }: Props) {
  const [outline, setOutline] = useState<Outline | null>(null);
  const [hover, setHover] = useState<ZoneMapPoint | null>(null);
  const svgRef = useRef<SVGSVGElement>(null);

  useEffect(() => {
    let live = true;
    loadOutline().then(
      (o) => live && setOutline(o),
      () => {},
    );
    return () => {
      live = false;
    };
  }, []);

  // A screen position in map units, and how many map units a screen pixel is.
  const toMap = (e: React.MouseEvent) => {
    const r = svgRef.current?.getBoundingClientRect();
    if (!r || r.width === 0) return null;
    const unit = MAP_WIDTH / r.width;
    return {
      x: (e.clientX - r.left) * unit,
      y: ZONE_MAP_TOP + (e.clientY - r.top) * unit,
      unit,
    };
  };

  const night = twilightPaths(new Date(now));
  const hoverAt = hover ? project(hover.lat, hover.lon) : null;

  return (
    <div className="zmap">
      <svg
        ref={svgRef}
        className="zmap-svg"
        viewBox={`0 ${ZONE_MAP_TOP} ${MAP_WIDTH} ${HEIGHT}`}
        onClick={(e) => {
          const at = toMap(e);
          const p = at && nearestPoint(at.x, at.y, CLICK_PX * at.unit);
          if (p) onPick(p.zone);
        }}
        onMouseMove={(e) => {
          const at = toMap(e);
          setHover(at && nearestPoint(at.x, at.y, HOVER_PX * at.unit));
        }}
        onMouseLeave={() => setHover(null)}
        aria-hidden
      >
        {outline?.land.map((d, i) => (
          <path key={`l${i}`} className="zmap-land" d={d} />
        ))}
        {outline?.lakes.map((d, i) => (
          <path key={`w${i}`} className="zmap-lake" d={d} />
        ))}
        {night.map((d, i) => (d ? <path key={`n${i}`} className="zmap-night" d={d} /> : null))}
        {ZONE_MAP_POINTS.map((p) => {
          const at = project(p.lat, p.lon);
          const current = p.zone === zone;
          const cls = [
            'zmap-point',
            current && 'is-current',
            hover && !current && sameOffset(p.zone, hover.zone, now) && 'is-same',
            !pointMatches(p, query, lang) && 'is-dim',
          ]
            .filter(Boolean)
            .join(' ');
          return (
            <circle
              key={p.zone}
              className={cls}
              cx={at.x}
              cy={at.y}
              r={current ? 5 : hover?.zone === p.zone ? 4.5 : 3}
              data-zone={p.zone}
            />
          );
        })}
      </svg>
      {hover && hoverAt && (
        <div
          className="zmap-tip"
          style={{
            left: `${(hoverAt.x / MAP_WIDTH) * 100}%`,
            top: `${((hoverAt.y - ZONE_MAP_TOP) / HEIGHT) * 100}%`,
          }}
        >
          {[
            pointLabel(hover),
            countryName(hover.country, lang),
            `${clockText(now, hover.zone)} ${zoneAbbreviation(hover.zone, now)}`,
          ]
            .filter(Boolean)
            .join(' · ')}
        </div>
      )}
    </div>
  );
}
