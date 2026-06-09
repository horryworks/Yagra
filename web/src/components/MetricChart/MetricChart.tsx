// Time-series chart pane backed by uPlot (fast at many series). Canvas strokes can't read
// CSS vars, so chart colors come from a small palette constant — exempt from the theme-var
// rule per CLAUDE.md (chart-library color props are data-driven).

import { useEffect, useRef } from 'react';
import uPlot from 'uplot';
import 'uplot/dist/uPlot.min.css';

const CHART_STROKE = '#4f8cff';

interface Props {
  title: string;
  /** X axis: Unix seconds. */
  timestamps: number[];
  /** Y axis values aligned to `timestamps`. */
  values: number[];
}

export function MetricChart({ title, timestamps, values }: Props) {
  const ref = useRef<HTMLDivElement>(null);

  useEffect(() => {
    const el = ref.current;
    if (!el) return;
    const opts: uPlot.Options = {
      title,
      width: 460,
      height: 220,
      series: [{}, { label: title, stroke: CHART_STROKE, width: 2 }],
    };
    const data: uPlot.AlignedData = [timestamps, values];
    const plot = new uPlot(opts, data, el);
    return () => plot.destroy();
  }, [title, timestamps, values]);

  return <div ref={ref} />;
}
