// SPDX-License-Identifier: AGPL-3.0-only
// Pure helper for MetricChart's live legend. Kept out of the .tsx because Vitest runs
// `environment: 'node'` and never executes .tsx — a rule written at the call site is a rule no test
// can reach.

/** Index the legend reads while the cursor is away.
 *
 *  uPlot's legend is *live*: it reports the value under the cursor, and with no cursor there is no
 *  index, so every row reads `--`. That is the state a chart is in almost all of the time — nobody
 *  is hovering it — so the readout says nothing precisely when it is being glanced at. Feeding
 *  uPlot this index instead pins the idle legend to the most recent sample.
 *
 *  **The most recent sample that *any* series has**, not the last bucket. A window that runs to
 *  `now` normally ends in a bucket no poll has filled yet, so the last column is all gaps and
 *  reading it would put the `--` straight back. Taking the max across series also keeps one number
 *  for the whole chart: every row then reports the same instant, which is what makes the rows
 *  comparable — a per-series "latest" would silently mix timestamps.
 *
 *  Returns `null` when there is nothing to show (no series, or every sample a gap); the caller
 *  leaves uPlot's own `--` in place, which is then the honest answer. */
export function idleLegendIdx(
  series: readonly (readonly (number | null | undefined)[])[],
): number | null {
  let best: number | null = null;
  for (const s of series) {
    for (let i = s.length - 1; i >= 0; i -= 1) {
      if (s[i] != null) {
        if (best == null || i > best) best = i;
        break;
      }
    }
  }
  return best;
}
