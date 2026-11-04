// SPDX-License-Identifier: AGPL-3.0-only
// Reading what a device printed for "compare configuration" into two columns (ADR-222 Inc.9).
//
// Huawei VRP prints the running configuration from its first difference under a
// "====== Current configuration line N ======" heading, then the startup file's lines under
// "====== Configuration file line N ======". The Config tab shows the two side by side, a row
// marked where they differ. Output in any other shape is shown as it came, in one column.

/** One side of the comparison: where it starts and its lines. */
export interface CompareSide {
  line: number;
  lines: string[];
}

/** The two sides, or `null` when the output is not in the two-heading shape. */
export interface CompareColumns {
  running: CompareSide;
  saved: CompareSide;
}

const RUNNING = /^\s*=+\s*Current configuration line (\d+)\s*=+\s*$/i;
const SAVED = /^\s*=+\s*Configuration file line (\d+)\s*=+\s*$/i;

/** Drop the blank lines a block ends with, so the two columns line up on content. */
function trimEnd(lines: string[]): string[] {
  let end = lines.length;
  while (end > 0 && lines[end - 1].trim() === '') end -= 1;
  return lines.slice(0, end);
}

/** Split the output into its running and saved blocks. Anything before the first heading (the
 *  "comparing" and "not the same" lines) is dropped: the screen already says the node is unsaved. */
export function compareColumns(text: string): CompareColumns | null {
  const lines = text.replace(/\r/g, '').split('\n');
  const r = lines.findIndex((l) => RUNNING.test(l));
  const s = lines.findIndex((l) => SAVED.test(l));
  if (r < 0 || s < 0 || s < r) return null;
  const rLine = Number(RUNNING.exec(lines[r])?.[1]);
  const sLine = Number(SAVED.exec(lines[s])?.[1]);
  return {
    running: { line: rLine, lines: trimEnd(lines.slice(r + 1, s)) },
    saved: { line: sLine, lines: trimEnd(lines.slice(s + 1)) },
  };
}

/** One row of the side-by-side view; `undefined` on a side that has no line at that row. */
export interface CompareRow {
  running: string | undefined;
  saved: string | undefined;
  differs: boolean;
}

/** Past this many lines a side, rows are paired by position rather than aligned: the device prints
 *  a few lines from its first difference, so this only bounds the work on an output it never
 *  sends. */
export const ALIGN_MAX_LINES = 400;

const same = (a: string, b: string) => a.trimEnd() === b.trimEnd();

/** The two columns as rows, aligned on the lines they share (a longest common subsequence), so a
 *  line one side has and the other lacks sits on a row of its own instead of shifting every row
 *  below it. Each row is marked when its two sides are not the same text. */
export function compareRows(c: CompareColumns): CompareRow[] {
  const a = c.running.lines;
  const b = c.saved.lines;
  if (a.length > ALIGN_MAX_LINES || b.length > ALIGN_MAX_LINES) {
    return Array.from({ length: Math.max(a.length, b.length) }, (_, i) => ({
      running: a[i],
      saved: b[i],
      differs: a[i] === undefined || b[i] === undefined || !same(a[i], b[i]),
    }));
  }
  // lcs[i][j] = the longest common run of a[i..] and b[j..].
  const lcs = Array.from({ length: a.length + 1 }, () => new Array<number>(b.length + 1).fill(0));
  for (let i = a.length - 1; i >= 0; i -= 1) {
    for (let j = b.length - 1; j >= 0; j -= 1) {
      lcs[i][j] = same(a[i], b[j]) ? lcs[i + 1][j + 1] + 1 : Math.max(lcs[i + 1][j], lcs[i][j + 1]);
    }
  }
  const rows: CompareRow[] = [];
  let i = 0;
  let j = 0;
  while (i < a.length || j < b.length) {
    if (i < a.length && j < b.length && same(a[i], b[j])) {
      rows.push({ running: a[i], saved: b[j], differs: false });
      i += 1;
      j += 1;
    } else if (j < b.length && (i >= a.length || lcs[i][j + 1] > lcs[i + 1][j])) {
      rows.push({ running: undefined, saved: b[j], differs: true });
      j += 1;
    } else if (i < a.length && j < b.length && lcs[i + 1][j] === lcs[i][j + 1] && lcs[i + 1][j + 1] === lcs[i][j]) {
      // Neither line is shared further on: one changed line, shown on one row.
      rows.push({ running: a[i], saved: b[j], differs: true });
      i += 1;
      j += 1;
    } else {
      rows.push({ running: a[i], saved: undefined, differs: true });
      i += 1;
    }
  }
  return rows;
}
