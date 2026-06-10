// Rules & thresholds (Alerts ▸ Rules & thresholds). Thresholds resolve by hierarchical
// override — profile → group → node, most-specific wins (§3.3) — so each rule carries a
// scope level + id. CRUD against /thresholds. Rules are evaluated live: the alert engine
// snapshots them (refreshed every ~30s) and checks each matching poll sample through the
// same hysteresis/flapping machinery as liveness, so a breach fires a real alert.

import { useCallback, useEffect, useState } from 'react';
import { api, ApiError } from '../services/api';
import { useAuthStore } from '../store';
import type { Direction, ScopeLevel, StoredThreshold } from '../types/api';
import { PageHeader } from '../components/ui/PageHeader';
import { Card } from '../components/ui/Card';
import { Button } from '../components/ui/Button';
import { TextInput, Select } from '../components/ui/Field';
import { Badge } from '../components/ui/Badge';
import './CrudList.css';
import './ThresholdsPage.css';

const LEVELS: ScopeLevel[] = ['profile', 'group', 'node'];
const DIRECTIONS: Direction[] = ['above', 'below'];
// Metrics the pollers emit today (ICMP every node, SNMP when a community is bound). Offered
// as presets so operators don't have to guess the exact series name; free text is still
// allowed for any other collected metric (e.g. snmp_oid_*).
const METRIC_PRESETS = ['icmp_rtt_ms', 'icmp_loss_pct', 'snmp_sys_uptime_ticks'];

export function ThresholdsPage() {
  const authed = useAuthStore((s) => s.authed);
  const [rows, setRows] = useState<StoredThreshold[]>([]);
  const [unavailable, setUnavailable] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const [level, setLevel] = useState<ScopeLevel>('profile');
  const [scopeId, setScopeId] = useState('');
  const [metric, setMetric] = useState('');
  const [direction, setDirection] = useState<Direction>('above');
  const [warning, setWarning] = useState('');
  const [critical, setCritical] = useState('');
  const [dwell, setDwell] = useState('3');

  const load = useCallback(() => {
    api
      .listThresholds()
      .then((list) => {
        setRows(list);
        setUnavailable(false);
      })
      .catch((e: unknown) => {
        if (e instanceof ApiError && e.code === 'admin_unavailable') setUnavailable(true);
      });
  }, []);

  useEffect(() => {
    load();
  }, [load]);

  const add = () => {
    setError(null);
    const num = (s: string) => (s.trim() === '' ? undefined : Number(s));
    api
      .createThreshold({
        scope_level: level,
        scope_id: scopeId.trim(),
        metric: metric.trim(),
        direction,
        warning: num(warning),
        critical: num(critical),
        dwell_samples: num(dwell),
      })
      .then(() => {
        setScopeId('');
        setMetric('');
        setWarning('');
        setCritical('');
        load();
      })
      .catch((e: unknown) => setError(e instanceof ApiError ? e.message : 'failed to add'));
  };

  const remove = (id: string) =>
    api
      .deleteThreshold(id)
      .then(load)
      .catch((e: unknown) => setError(e instanceof ApiError ? e.message : 'failed to delete'));

  return (
    <div>
      <PageHeader
        title="Rules & thresholds"
        trail={[{ label: 'Alerts' }, { label: 'Rules & thresholds' }]}
        note="Hierarchical overrides: profile → group → node. The most specific scope wins."
      />

      <Card className="thresholds-note-card">
        <p className="thresholds-note">
          ICMP liveness (node up/down) alerts automatically — no rule needed. Rules here add
          metric thresholds on top: each is evaluated live on every matching poll sample, with
          the most-specific scope winning. Warning/critical bounds and a dwell (consecutive
          samples before it commits, anti-flap) are per rule.
        </p>
      </Card>

      {unavailable ? (
        <Card>
          <p className="muted">Threshold management is unavailable in skeleton mode.</p>
        </Card>
      ) : (
        <Card title="Threshold rules">
          {authed && (
            <div className="thresholds-form form-row">
              <Select value={level} onChange={(e) => setLevel(e.target.value as ScopeLevel)}>
                {LEVELS.map((l) => (
                  <option key={l} value={l}>
                    {l}
                  </option>
                ))}
              </Select>
              <TextInput
                className="mono"
                placeholder="scope id"
                value={scopeId}
                onChange={(e) => setScopeId(e.target.value)}
              />
              <TextInput
                className="mono"
                placeholder="metric (e.g. icmp_rtt_ms)"
                list="metric-presets"
                value={metric}
                onChange={(e) => setMetric(e.target.value)}
              />
              <datalist id="metric-presets">
                {METRIC_PRESETS.map((m) => (
                  <option key={m} value={m} />
                ))}
              </datalist>
              <Select
                value={direction}
                onChange={(e) => setDirection(e.target.value as Direction)}
              >
                {DIRECTIONS.map((d) => (
                  <option key={d} value={d}>
                    {d}
                  </option>
                ))}
              </Select>
              <TextInput
                className="thresholds-num"
                placeholder="warn"
                value={warning}
                onChange={(e) => setWarning(e.target.value)}
              />
              <TextInput
                className="thresholds-num"
                placeholder="crit"
                value={critical}
                onChange={(e) => setCritical(e.target.value)}
              />
              <TextInput
                className="thresholds-num"
                placeholder="dwell"
                value={dwell}
                onChange={(e) => setDwell(e.target.value)}
              />
              <Button variant="primary" onClick={add} disabled={!metric.trim() || !scopeId.trim()}>
                Add rule
              </Button>
            </div>
          )}
          {error && <p className="form-error">{error}</p>}

          {rows.length === 0 ? (
            <p className="muted">No threshold rules yet.</p>
          ) : (
            <div className="crud-list">
              {rows.map((t) => (
                <div className="crud-row thresholds-row" key={t.id}>
                  <Badge tone="neutral">{t.level}</Badge>
                  <span className="mono thresholds-scope">{t.scope_id}</span>
                  <span className="mono thresholds-metric">{t.metric}</span>
                  <span className="muted">{t.direction}</span>
                  <span className="thresholds-bounds">
                    {t.warning != null && <Badge tone="warning">W {t.warning}</Badge>}
                    {t.critical != null && <Badge tone="critical">C {t.critical}</Badge>}
                  </span>
                  <span className="muted thresholds-dwell">dwell {t.dwell_samples}</span>
                  {authed && (
                    <Button variant="ghost" onClick={() => remove(t.id)}>
                      Delete
                    </Button>
                  )}
                </div>
              ))}
            </div>
          )}
        </Card>
      )}
    </div>
  );
}
