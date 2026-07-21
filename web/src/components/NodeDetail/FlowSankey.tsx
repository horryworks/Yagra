// SPDX-License-Identifier: AGPL-3.0-only
// FlowSankey (ADR-031 Increment 3): a hand-rolled SVG Sankey of src→dst conversations. Each band's
// thickness is proportional to bytes, so "where is this device's traffic going" reads at a glance —
// the same data as the conversations table, shown as flow. No chart dependency (like TopologyMap /
// the Donut primitive, it's inline SVG); theme-aware via CSS variables; colors from the shared
// palette. The table beside it keeps the exact, sortable numbers.

import { useMemo } from 'react';
import { formatBytes, formatAsn } from '../../lib/format';
import { PALETTE } from '../MetricChart/MetricChart';
import type { FlowConversation } from '../../types/api';
import './FlowSankey.css';

/** Max conversations drawn — keeps the diagram legible; the table shows the full list. */
export const SANKEY_MAX_LINKS = 12;

const VIEW_W = 760;
const LABEL_W = 168; // horizontal room reserved for the address labels on each side
const NODE_W = 12;
const NODE_GAP = 6;

/** Longest AS label drawn on a node before it's clipped with an ellipsis (full text in the title).
 *  Sized to fit the reserved LABEL_W at the AS line's font. */
const AS_LABEL_MAX = 22;

const num = (n: number): string => n.toFixed(1);

const truncate = (s: string, max: number): string => (s.length > max ? `${s.slice(0, max - 1)}…` : s);

/** One src→dst ribbon. */
export interface SankeyBand {
  path: string;
  color: string;
  src: string;
  dst: string;
  bytes: number;
}

/** One column node (a src on the left or a dst on the right). */
export interface SankeyNode {
  key: string;
  label: string;
  /** AS label for this host (`AS15169 · GOOGLE`), shown under the IP; absent when the AS is unknown. */
  sub?: string;
  x: number;
  y: number;
  h: number;
  anchor: 'start' | 'end';
  labelX: number;
}

/** The full diagram model (pure — unit-tested without rendering). */
export interface SankeyModel {
  width: number;
  height: number;
  bands: SankeyBand[];
  nodes: SankeyNode[];
}

/**
 * Lay out a Sankey from conversations. src hosts stack in the left column and dst hosts in the
 * right, each sized by total bytes; a constant vertical scale keeps every ribbon's thickness ∝ its
 * bytes at both ends. Returns `null` when there's nothing to draw.
 */
export function buildSankey(conversations: FlowConversation[]): SankeyModel | null {
  const links = conversations.filter((c) => c.bytes > 0).slice(0, SANKEY_MAX_LINKS);
  if (links.length === 0) return null;

  const srcTotals = new Map<string, number>();
  const dstTotals = new Map<string, number>();
  for (const c of links) {
    srcTotals.set(c.src, (srcTotals.get(c.src) ?? 0) + c.bytes);
    dstTotals.set(c.dst, (dstTotals.get(c.dst) ?? 0) + c.bytes);
  }
  const srcOrder = [...srcTotals.entries()].sort((a, b) => b[1] - a[1]).map(([k]) => k);
  const dstOrder = [...dstTotals.entries()].sort((a, b) => b[1] - a[1]).map(([k]) => k);
  const srcIndex = new Map(srcOrder.map((k, i) => [k, i]));
  const dstIndex = new Map(dstOrder.map((k, i) => [k, i]));

  // Per-host AS label (an IP's AS is stable, so first sighting wins). `undefined` ⇒ no AS line.
  const srcAs = new Map<string, string | undefined>();
  const dstAs = new Map<string, string | undefined>();
  for (const c of links) {
    if (!srcAs.has(c.src)) srcAs.set(c.src, formatAsn(c.src_asn, c.src_as_name) ?? undefined);
    if (!dstAs.has(c.dst)) dstAs.set(c.dst, formatAsn(c.dst_asn, c.dst_as_name) ?? undefined);
  }

  const total = links.reduce((s, c) => s + c.bytes, 0);
  const maxNodes = Math.max(srcOrder.length, dstOrder.length);
  const height = Math.max(200, maxNodes * 30);
  const usableSrc = height - NODE_GAP * (srcOrder.length - 1);
  const usableDst = height - NODE_GAP * (dstOrder.length - 1);
  // One scale for both columns ⇒ each ribbon has the same thickness at its src and dst ends.
  const scale = Math.min(usableSrc, usableDst) / total;

  const leftBarX = LABEL_W;
  const rightBarX = VIEW_W - LABEL_W - NODE_W;

  const placeColumn = (
    order: string[],
    totals: Map<string, number>,
    x: number,
    anchor: 'start' | 'end',
    labelX: number,
    asOf: Map<string, string | undefined>,
  ) => {
    const stackH =
      order.reduce((s, k) => s + (totals.get(k) ?? 0) * scale, 0) + NODE_GAP * (order.length - 1);
    let y = Math.max(0, (height - stackH) / 2);
    const boxes = new Map<string, { cursor: number }>();
    const nodes: SankeyNode[] = [];
    for (const k of order) {
      const h = (totals.get(k) ?? 0) * scale;
      boxes.set(k, { cursor: y });
      nodes.push({ key: `${anchor}-${k}`, label: k, sub: asOf.get(k), x, y, h, anchor, labelX });
      y += h + NODE_GAP;
    }
    return { boxes, nodes };
  };

  const srcCol = placeColumn(srcOrder, srcTotals, leftBarX, 'end', leftBarX - 6, srcAs);
  const dstCol = placeColumn(dstOrder, dstTotals, rightBarX, 'start', rightBarX + NODE_W + 6, dstAs);

  const x0 = leftBarX + NODE_W;
  const x1 = rightBarX;
  const midX = (x0 + x1) / 2;

  // Draw grouped by src then dst so ribbons stack neatly rather than crossing arbitrarily.
  const drawOrder = [...links].sort(
    (a, b) =>
      (srcIndex.get(a.src) ?? 0) - (srcIndex.get(b.src) ?? 0) ||
      (dstIndex.get(a.dst) ?? 0) - (dstIndex.get(b.dst) ?? 0),
  );

  const bands: SankeyBand[] = drawOrder.map((c) => {
    const sb = srcCol.boxes.get(c.src);
    const db = dstCol.boxes.get(c.dst);
    const h = c.bytes * scale;
    const sTop = sb ? sb.cursor : 0;
    const dTop = db ? db.cursor : 0;
    if (sb) sb.cursor += h;
    if (db) db.cursor += h;
    const path = [
      `M ${num(x0)} ${num(sTop)}`,
      `C ${num(midX)} ${num(sTop)} ${num(midX)} ${num(dTop)} ${num(x1)} ${num(dTop)}`,
      `L ${num(x1)} ${num(dTop + h)}`,
      `C ${num(midX)} ${num(dTop + h)} ${num(midX)} ${num(sTop + h)} ${num(x0)} ${num(sTop + h)}`,
      'Z',
    ].join(' ');
    return {
      path,
      color: PALETTE[(srcIndex.get(c.src) ?? 0) % PALETTE.length],
      src: c.src,
      dst: c.dst,
      bytes: c.bytes,
    };
  });

  return { width: VIEW_W, height, bands, nodes: [...srcCol.nodes, ...dstCol.nodes] };
}

export function FlowSankey({
  conversations,
  onNodeClick,
}: {
  conversations: FlowConversation[];
  /** When set, clicking a column node (a src/dst host) drills into that peer. */
  onNodeClick?: (ip: string) => void;
}) {
  const model = useMemo(() => buildSankey(conversations), [conversations]);
  if (!model) return null;
  return (
    <svg
      className="flow-sankey"
      viewBox={`0 0 ${model.width} ${model.height}`}
      preserveAspectRatio="xMidYMid meet"
      role="img"
    >
      {model.bands.map((b) => (
        <path key={`${b.src}->${b.dst}`} className="flow-sankey-band" d={b.path} fill={b.color}>
          <title>{`${b.src} → ${b.dst}: ${formatBytes(b.bytes)}`}</title>
        </path>
      ))}
      {model.nodes.map((n) => (
        <g
          key={n.key}
          className={onNodeClick ? 'flow-sankey-g clickable' : undefined}
          onClick={onNodeClick ? () => onNodeClick(n.label) : undefined}
        >
          <rect
            className="flow-sankey-node"
            x={num(n.x)}
            y={num(n.y)}
            width={NODE_W}
            height={num(Math.max(1, n.h))}
          />
          <text
            className="flow-sankey-label"
            x={num(n.labelX)}
            y={num(n.y + n.h / 2)}
            textAnchor={n.anchor}
            dominantBaseline="middle"
          >
            <tspan x={num(n.labelX)} dy={n.sub ? '-0.35em' : '0'}>
              {n.label}
            </tspan>
            {n.sub && (
              <tspan className="flow-sankey-as" x={num(n.labelX)} dy="1.2em">
                {truncate(n.sub, AS_LABEL_MAX)}
              </tspan>
            )}
            <title>{n.sub ? `${n.label} · ${n.sub}` : n.label}</title>
          </text>
        </g>
      ))}
    </svg>
  );
}
