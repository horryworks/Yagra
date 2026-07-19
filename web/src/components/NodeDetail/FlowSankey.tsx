// FlowSankey (ADR-031 Increment 3): a hand-rolled SVG Sankey of src→dst conversations. Each band's
// thickness is proportional to bytes, so "where is this device's traffic going" reads at a glance —
// the same data as the conversations table, shown as flow. No chart dependency (like TopologyMap /
// the Donut primitive, it's inline SVG); theme-aware via CSS variables; colors from the shared
// palette. The table beside it keeps the exact, sortable numbers.

import { useMemo } from 'react';
import { formatBytes } from '../../lib/format';
import { PALETTE } from '../MetricChart/MetricChart';
import type { FlowConversation } from '../../types/api';
import './FlowSankey.css';

/** Max conversations drawn — keeps the diagram legible; the table shows the full list. */
export const SANKEY_MAX_LINKS = 12;

const VIEW_W = 760;
const LABEL_W = 168; // horizontal room reserved for the address labels on each side
const NODE_W = 12;
const NODE_GAP = 6;

const num = (n: number): string => n.toFixed(1);

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
  ) => {
    const stackH =
      order.reduce((s, k) => s + (totals.get(k) ?? 0) * scale, 0) + NODE_GAP * (order.length - 1);
    let y = Math.max(0, (height - stackH) / 2);
    const boxes = new Map<string, { cursor: number }>();
    const nodes: SankeyNode[] = [];
    for (const k of order) {
      const h = (totals.get(k) ?? 0) * scale;
      boxes.set(k, { cursor: y });
      nodes.push({ key: `${anchor}-${k}`, label: k, x, y, h, anchor, labelX });
      y += h + NODE_GAP;
    }
    return { boxes, nodes };
  };

  const srcCol = placeColumn(srcOrder, srcTotals, leftBarX, 'end', leftBarX - 6);
  const dstCol = placeColumn(dstOrder, dstTotals, rightBarX, 'start', rightBarX + NODE_W + 6);

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

export function FlowSankey({ conversations }: { conversations: FlowConversation[] }) {
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
        <g key={n.key}>
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
            {n.label}
            <title>{n.label}</title>
          </text>
        </g>
      ))}
    </svg>
  );
}
