// SPDX-License-Identifier: AGPL-3.0-only
// Brand mark (§1.1) — seal (朱印) style, unified everywhere: a brand-orange rounded tile with an
// off-white 生成り mark. The mark is the topology fork that also reads as a "Y" (one root node
// branching to two), shared with the Yagra-Website brand assets. No outline-only transparent
// logo. Two renderings of the same seal:
//   - 'seal' (default): the orange tile + mark — top bar / home / app icon.
//   - 'mark': just the off-white mark on transparent — for placing ON a brand-orange
//     surface (the login panel is itself the seal at panel scale).
// The geometry lives in brandMark.ts because the static favicon must draw the same mark.
// Brand colors are the one hardcode exemption (a physical brand surface, fixed in both
// themes — §1.2).

import {
  BRAND,
  MARK,
  MARK_EDGES,
  MARK_NODES,
  MARK_VIEWBOX,
  MARK_WEIGHTS,
  SEAL_TILE,
} from './brandMark';

interface Props {
  size?: number;
  variant?: 'seal' | 'mark';
}

export function Logo({ size = 28, variant = 'seal' }: Props) {
  // The mark is always off-white; only the orange tile is variant-specific.
  const weight = MARK_WEIGHTS.logo;
  return (
    <svg
      width={size}
      height={size}
      viewBox={MARK_VIEWBOX}
      role="img"
      aria-label="Yagra"
      fill="none"
      xmlns="http://www.w3.org/2000/svg"
    >
      {variant === 'seal' && (
        <rect
          x={SEAL_TILE.x}
          y={SEAL_TILE.y}
          width={SEAL_TILE.size}
          height={SEAL_TILE.size}
          rx={SEAL_TILE.rx}
          fill={BRAND}
        />
      )}
      <g stroke={MARK} strokeWidth={weight.stroke} strokeLinecap="round" fill="none">
        {MARK_EDGES.map((d) => (
          <path key={d} d={d} />
        ))}
      </g>
      <g fill={MARK}>
        {MARK_NODES.map((n) => (
          <circle key={`${n.cx},${n.cy}`} cx={n.cx} cy={n.cy} r={weight.node} />
        ))}
      </g>
    </svg>
  );
}
