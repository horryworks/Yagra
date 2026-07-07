// Alert rules (Alerts ▸ Alert rules). Thresholds resolve by hierarchical
// override — profile → group → node, most-specific wins (§3.3) — so each rule carries a
// scope level + id. CRUD against /thresholds. Rules are evaluated live: the alert engine
// snapshots them (refreshed every ~30s) and checks each matching poll sample through the
// same hysteresis/flapping machinery as liveness, so a breach fires a real alert.
//
// Data-table standard v2: a toolbar (count + "+ Add rule") over the shared `.ytable`; the
// add form and delete confirmation both go through modals. The blue-left-border note card
// above the toolbar keeps the "what a rule is" explainer in view.

import { useCallback, useEffect, useState } from 'react';
import { api, ApiError } from '../services/api';
import { useAuthStore } from '../store';
import type { Direction, ScopeLevel, StoredThreshold } from '../types/api';
import { PageHeader } from '../components/ui/PageHeader';
import { Card } from '../components/ui/Card';
import { Button } from '../components/ui/Button';
import { Modal } from '../components/ui/Modal';
import { TextInput, Select } from '../components/ui/Field';
import { Badge } from '../components/ui/Badge';
import { EntityName, useEntityNames } from '../components/ui/EntityName';
import { IconButton } from '../components/ui/IconButton';
import { TableToolbar, TableSpacer, ResultCount } from '../components/ui/TableToolbar';
import { TrashIcon } from '../components/ui/icons';
import './ThresholdsPage.css';

const LEVELS: ScopeLevel[] = ['profile', 'group', 'node'];
const DIRECTIONS: Direction[] = ['above', 'below'];
// Metrics the pollers emit today (ICMP every node, SNMP when a community is bound). Offered
// as presets so operators don't have to guess the exact series name; free text is still
// allowed for any other collected metric (e.g. snmp_oid_*).
const METRIC_PRESETS = ['icmp_rtt_ms', 'icmp_loss_pct', 'snmp_sys_uptime_ticks'];

const COLS = '1.6fr 1.4fr 110px 170px 100px 92px';

const errMsg = (e: unknown, fallback: string) => (e instanceof ApiError ? e.message : fallback);

/** The scope-id placeholder + explanatory hint vary by level. */
const scopeIdPlaceholder = (level: ScopeLevel) =>
  level === 'profile' ? 'profile id' : level === 'group' ? 'group tag value' : 'node id';

const scopeIdNoun = (level: ScopeLevel) =>
  level === 'profile'
    ? 'device-profile id'
    : level === 'group'
      ? 'group tag value (e.g. a site name)'
      : 'node id';

/** Create a threshold rule (focused-editing modal). */
function AddThresholdModal({ onClose, onSaved }: { onClose: () => void; onSaved: () => void }) {
  const [level, setLevel] = useState<ScopeLevel>('profile');
  const [scopeId, setScopeId] = useState('');
  const [metric, setMetric] = useState('');
  const [direction, setDirection] = useState<Direction>('above');
  const [warning, setWarning] = useState('');
  const [critical, setCritical] = useState('');
  const [dwell, setDwell] = useState('3');
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const ready = metric.trim() !== '' && scopeId.trim() !== '';

  const submit = () => {
    if (!ready) return;
    setBusy(true);
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
        onSaved();
        onClose();
      })
      .catch((e: unknown) => {
        setError(errMsg(e, 'failed to add'));
        setBusy(false);
      });
  };

  return (
    <Modal
      title="Add threshold rule"
      onClose={onClose}
      footer={
        <>
          <Button variant="outline" onClick={onClose} disabled={busy}>
            Cancel
          </Button>
          <Button variant="primary" onClick={submit} disabled={!ready || busy}>
            Add rule
          </Button>
        </>
      }
    >
      <div className="modal-field">
        <label className="modal-field-label">Scope level</label>
        <Select value={level} onChange={(e) => setLevel(e.target.value as ScopeLevel)}>
          {LEVELS.map((l) => (
            <option key={l} value={l}>
              {l}
            </option>
          ))}
        </Select>
      </div>
      <div className="modal-field">
        <label className="modal-field-label">Scope id</label>
        <TextInput
          className="mono"
          placeholder={scopeIdPlaceholder(level)}
          value={scopeId}
          onChange={(e) => setScopeId(e.target.value)}
          autoFocus
        />
        <span className="modal-hint">
          The {scopeIdNoun(level)} this rule targets.
        </span>
      </div>
      <div className="modal-field">
        <label className="modal-field-label">Metric</label>
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
      </div>
      <div className="modal-field">
        <label className="modal-field-label">Direction</label>
        <Select value={direction} onChange={(e) => setDirection(e.target.value as Direction)}>
          {DIRECTIONS.map((d) => (
            <option key={d} value={d}>
              {d}
            </option>
          ))}
        </Select>
      </div>
      <div className="modal-field">
        <label className="modal-field-label">Bounds &amp; dwell</label>
        <div className="thresholds-bounds">
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
            title="Dwell: consecutive readings that must breach before the alert fires"
          />
        </div>
        <span className="modal-hint">
          Warn / crit / dwell are optional. Dwell is how many consecutive readings must cross the
          bound before the alert fires — higher values suppress flapping (rapid on/off alerts).
        </span>
      </div>
      {error && <p className="form-error">{error}</p>}
    </Modal>
  );
}

/** Confirm + delete a threshold rule (destructive-consent modal). */
function DeleteThresholdModal({
  rule,
  onClose,
  onDone,
}: {
  rule: StoredThreshold;
  onClose: () => void;
  onDone: () => void;
}) {
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const submit = () => {
    setBusy(true);
    setError(null);
    api
      .deleteThreshold(rule.id)
      .then(onDone)
      .catch((e: unknown) => {
        setError(errMsg(e, 'failed to delete'));
        setBusy(false);
      });
  };

  return (
    <Modal
      title="Delete threshold rule"
      onClose={onClose}
      footer={
        <>
          <Button variant="outline" onClick={onClose} disabled={busy}>
            Cancel
          </Button>
          <Button variant="danger" onClick={submit} disabled={busy}>
            Delete
          </Button>
        </>
      }
    >
      <p className="modal-confirm-text">
        Delete the <strong>{rule.scope_level}</strong> rule on{' '}
        <strong className="mono">{rule.metric}</strong> for{' '}
        <strong className="mono">{rule.scope_id}</strong>? This cannot be undone.
      </p>
      {error && <p className="form-error">{error}</p>}
    </Modal>
  );
}

export function ThresholdsPage() {
  const authed = useAuthStore((s) => s.authed);
  const [rows, setRows] = useState<StoredThreshold[]>([]);
  const [unavailable, setUnavailable] = useState(false);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [adding, setAdding] = useState(false);
  const [deleting, setDeleting] = useState<StoredThreshold | null>(null);
  const { scopeName } = useEntityNames();

  const load = useCallback(() => {
    api
      .listThresholds()
      .then((list) => {
        setRows(list);
        setUnavailable(false);
      })
      .catch((e: unknown) => {
        if (e instanceof ApiError && e.code === 'admin_unavailable') setUnavailable(true);
      })
      .finally(() => setLoading(false));
  }, []);

  useEffect(() => {
    load();
  }, [load]);

  return (
    <div>
      <PageHeader
        title="Alert rules"
        trail={[{ label: 'Alerts' }, { label: 'Alert rules' }]}
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
        <>
          <TableToolbar>
            <TableSpacer />
            <ResultCount shown={rows.length} noun="rules" />
            {authed && (
              <Button variant="primary" onClick={() => setAdding(true)}>
                + Add rule
              </Button>
            )}
          </TableToolbar>

          {error && <p className="form-error">{error}</p>}

          <div className="ytable">
            <div className="ytable-head" style={{ gridTemplateColumns: COLS }}>
              <div className="ytable-h">Scope</div>
              <div className="ytable-h">Metric</div>
              <div className="ytable-h">Direction</div>
              <div className="ytable-h">Bounds</div>
              <div className="ytable-h">Dwell</div>
              <div className="ytable-h right">Actions</div>
            </div>

            {rows.length === 0 ? (
              <div className="yt-empty">
                <p className="yt-empty-title">{loading ? 'Loading…' : 'No threshold rules yet'}</p>
                {!loading && (
                  <p className="yt-empty-sub">
                    Add a metric threshold scoped to a profile, group, or node.
                  </p>
                )}
              </div>
            ) : (
              rows.map((t) => (
                <div className="ytable-row" style={{ gridTemplateColumns: COLS }} key={t.id}>
                  <div className="ytable-cell">
                    <Badge tone="neutral">{t.scope_level}</Badge>
                    <EntityName name={scopeName(t.scope_level, t.scope_id)} id={t.scope_id} />
                  </div>
                  <div className="ytable-cell mono">{t.metric}</div>
                  <div className="ytable-cell muted">{t.direction}</div>
                  <div className="ytable-cell">
                    <span className="thresholds-bounds">
                      {t.warning != null && <Badge tone="warning">W {t.warning}</Badge>}
                      {t.critical != null && <Badge tone="critical">C {t.critical}</Badge>}
                    </span>
                  </div>
                  <div className="ytable-cell muted">dwell {t.dwell_samples}</div>
                  <div className="ytable-cell right">
                    {authed && (
                      <span className="ytable-actions">
                        <IconButton title="Delete" danger onClick={() => setDeleting(t)}>
                          <TrashIcon />
                        </IconButton>
                      </span>
                    )}
                  </div>
                </div>
              ))
            )}
          </div>
        </>
      )}

      {adding && <AddThresholdModal onClose={() => setAdding(false)} onSaved={load} />}
      {deleting && (
        <DeleteThresholdModal
          rule={deleting}
          onClose={() => setDeleting(null)}
          onDone={() => {
            setDeleting(null);
            setError(null);
            load();
          }}
        />
      )}
    </div>
  );
}
