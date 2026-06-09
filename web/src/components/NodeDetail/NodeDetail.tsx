// Node-detail pane: fetches the latest reading for one metric via the typed API client.

import { useEffect, useState } from 'react';
import { api, ApiError } from '../../services/api';
import { formatRtt } from '../../lib/format';
import type { MetricReading } from '../../types/api';

interface Props {
  nodeId: string;
  metric: string;
}

export function NodeDetail({ nodeId, metric }: Props) {
  const [reading, setReading] = useState<MetricReading | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    setReading(null);
    setError(null);
    api
      .getNodeMetric(nodeId, metric)
      .then((r) => {
        if (!cancelled) setReading(r);
      })
      .catch((e: unknown) => {
        if (!cancelled) {
          setError(e instanceof ApiError ? e.message : 'request failed');
        }
      });
    return () => {
      cancelled = true;
    };
  }, [nodeId, metric]);

  return (
    <section className="pane">
      <h2>Node {nodeId}</h2>
      {error && <p className="muted">{error}</p>}
      {!error && !reading && <p className="muted">Loading…</p>}
      {reading && (
        <p>
          <strong>{reading.metric}</strong>:{' '}
          {reading.metric === 'icmp_rtt_ms' ? formatRtt(reading.value) : reading.value}
        </p>
      )}
    </section>
  );
}
