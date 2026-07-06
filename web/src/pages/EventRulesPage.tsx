import { useCallback, useEffect, useMemo, useState } from 'react';
import { api, ApiError } from '../services/api';
import { useAuthStore } from '../store';
import type { EventRule, EventRuleInput, EventSource, Severity } from '../types/api';
import { PageHeader } from '../components/ui/PageHeader';
import { Card } from '../components/ui/Card';
import { Button } from '../components/ui/Button';
import { Modal } from '../components/ui/Modal';
import { TextInput, Select, RequiredMark, FieldHint } from '../components/ui/Field';
import { Badge } from '../components/ui/Badge';
import { IconButton } from '../components/ui/IconButton';
import { TableToolbar, SearchInput, TableSpacer, ResultCount } from '../components/ui/TableToolbar';
import { EditIcon, TrashIcon, PowerIcon } from '../components/ui/icons';
import './EventRulesPage.css';

const COLS = '1.4fr 130px 1fr 120px 110px 110px';
const errMsg = (e: unknown, fallback: string) => (e instanceof ApiError ? e.message : fallback);

const SEVERITY_TONE: Record<Severity, 'critical' | 'warning' | 'info'> = {
  critical: 'critical',
  warning: 'warning',
  info: 'info',
};

function ruleToInput(r: EventRule): EventRuleInput {
  return {
    name: r.name,
    enabled: r.enabled,
    source_kind: r.source_kind,
    source_id: r.source_id,
    node_id: r.node_id,
    match_kind: r.match_kind,
    pattern: r.pattern,
    clear_pattern: r.clear_pattern,
    severity: r.severity,
    ttl_secs: r.ttl_secs,
    min_count: r.min_count,
    window_secs: r.window_secs,
  };
}

export function EventRulesPage() {
  const authed = useAuthStore((s) => s.authed);
  const [rows, setRows] = useState<EventRule[]>([]);
  const [sources, setSources] = useState<EventSource[]>([]);
  const [query, setQuery] = useState('');
  const [unavailable, setUnavailable] = useState(false);
  const [loading, setLoading] = useState(true);
  const [adding, setAdding] = useState(false);
  const [editing, setEditing] = useState<EventRule | null>(null);
  const [deleting, setDeleting] = useState<EventRule | null>(null);
  const [error, setError] = useState<string | null>(null);

  const load = useCallback(() => {
    api
      .listEventRules()
      .then((list) => {
        setRows(list);
        setUnavailable(false);
      })
      .catch((e: unknown) => {
        if (e instanceof ApiError && e.code === 'admin_unavailable') setUnavailable(true);
      })
      .finally(() => setLoading(false));
    api
      .listEventSources()
      .then(setSources)
      .catch(() => setSources([]));
  }, []);
  useEffect(() => {
    load();
  }, [load]);

  const filtered = useMemo(() => {
    const q = query.trim().toLowerCase();
    if (!q) return rows;
    return rows.filter((r) =>
      [r.name, r.pattern, r.clear_pattern ?? '', r.severity, r.source_kind ?? ''].some((f) =>
        f.toLowerCase().includes(q),
      ),
    );
  }, [rows, query]);

  const toggleEnabled = (r: EventRule) => {
    setError(null);
    api
      .updateEventRule(r.id, { ...ruleToInput(r), enabled: !r.enabled })
      .then(load)
      .catch((e: unknown) => setError(errMsg(e, 'failed to update rule')));
  };

  return (
    <div>
      <PageHeader
        title="Event rules"
        note="Match received syslog / SNMP-trap / webhook events (substring or regex) and raise alerts. Info-severity rules record the match without alerting."
      />
      {unavailable ? (
        <Card>Event rules are unavailable in this mode (no metadata store).</Card>
      ) : (
        <>
          <TableToolbar>
            <SearchInput
              value={query}
              onChange={setQuery}
              placeholder="Search name, pattern, severity…"
              ariaLabel="Search event rules"
            />
            <TableSpacer />
            <ResultCount shown={filtered.length} total={rows.length} noun="rules" />
            {authed && (
              <Button variant="primary" onClick={() => setAdding(true)}>
                + Add rule
              </Button>
            )}
          </TableToolbar>
          {error && <p className="form-error">{error}</p>}
          <div className="ytable eventrules-table">
            <div className="ytable-head" style={{ gridTemplateColumns: COLS }}>
              <div className="ytable-h">Name</div>
              <div className="ytable-h">Severity</div>
              <div className="ytable-h">Pattern</div>
              <div className="ytable-h">Scope</div>
              <div className="ytable-h">Status</div>
              <div className="ytable-h right">Actions</div>
            </div>
            {filtered.length === 0 ? (
              <div className="yt-empty">
                <p className="yt-empty-title">
                  {loading ? 'Loading…' : rows.length === 0 ? 'No event rules' : 'No rules match'}
                </p>
                {!loading && (
                  <p className="yt-empty-sub">
                    {rows.length === 0
                      ? 'Add a rule to turn matching events into alerts.'
                      : 'Try a different search.'}
                  </p>
                )}
              </div>
            ) : (
              filtered.map((r) => (
                <div className="ytable-row" key={r.id} style={{ gridTemplateColumns: COLS }}>
                  <div className="ytable-cell">{r.name}</div>
                  <div className="ytable-cell">
                    <Badge tone={SEVERITY_TONE[r.severity]}>{r.severity}</Badge>
                  </div>
                  <div className="ytable-cell eventrules-match">
                    <div className="eventrules-sig">
                      <span className="eventrules-sig-kind">{r.match_kind}</span>
                      <span className="eventrules-sig-val mono" title={r.pattern}>
                        {r.pattern}
                      </span>
                    </div>
                    {r.clear_pattern && (
                      <div className="eventrules-sig">
                        <span className="eventrules-sig-kind">clear</span>
                        <span className="eventrules-sig-val mono" title={r.clear_pattern}>
                          {r.clear_pattern}
                        </span>
                      </div>
                    )}
                  </div>
                  <div className="ytable-cell mono">{r.source_kind ?? 'any'}</div>
                  <div className="ytable-cell">
                    <Badge tone={r.enabled ? 'up' : 'neutral'}>
                      {r.enabled ? 'enabled' : 'disabled'}
                    </Badge>
                  </div>
                  <div className="ytable-cell right">
                    {authed && (
                      <span className="ytable-actions">
                        <IconButton
                          title={r.enabled ? 'Disable rule' : 'Enable rule'}
                          onClick={() => toggleEnabled(r)}
                        >
                          <PowerIcon />
                        </IconButton>
                        <IconButton title="Edit rule" onClick={() => setEditing(r)}>
                          <EditIcon />
                        </IconButton>
                        <IconButton title="Delete rule" danger onClick={() => setDeleting(r)}>
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
      {adding && (
        <RuleModal
          mode="add"
          sources={sources}
          onClose={() => setAdding(false)}
          onDone={() => {
            setAdding(false);
            load();
          }}
        />
      )}
      {editing && (
        <RuleModal
          mode="edit"
          rule={editing}
          sources={sources}
          onClose={() => setEditing(null)}
          onDone={() => {
            setEditing(null);
            load();
          }}
        />
      )}
      {deleting && (
        <DeleteRuleModal
          rule={deleting}
          onClose={() => setDeleting(null)}
          onDone={() => {
            setDeleting(null);
            load();
          }}
        />
      )}
    </div>
  );
}

function RuleModal({
  mode,
  rule,
  sources,
  onClose,
  onDone,
}: {
  mode: 'add' | 'edit';
  rule?: EventRule;
  sources: EventSource[];
  onClose: () => void;
  onDone: () => void;
}) {
  const [name, setName] = useState(rule?.name ?? '');
  const [matchKind, setMatchKind] = useState<'substring' | 'regex'>(rule?.match_kind ?? 'substring');
  const [pattern, setPattern] = useState(rule?.pattern ?? '');
  const [clearPattern, setClearPattern] = useState(rule?.clear_pattern ?? '');
  const [severity, setSeverity] = useState<Severity>(rule?.severity ?? 'warning');
  const [sourceKind, setSourceKind] = useState<string>(rule?.source_kind ?? '');
  const [sourceId, setSourceId] = useState<string>(rule?.source_id ?? '');
  const [ttl, setTtl] = useState(String(rule?.ttl_secs ?? 1800));
  const [minCount, setMinCount] = useState(String(rule?.min_count ?? 1));
  const [windowSecs, setWindowSecs] = useState(String(rule?.window_secs ?? 60));
  const [enabled, setEnabled] = useState(rule?.enabled ?? true);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  // Interactive tester.
  const [sample, setSample] = useState('');
  const [testResult, setTestResult] = useState<string | null>(null);

  const valid = name.trim() !== '' && pattern.trim() !== '';

  const runTest = () => {
    setTestResult(null);
    api
      .testEventRule({
        match_kind: matchKind,
        pattern,
        clear_pattern: clearPattern.trim() || null,
        sample,
      })
      .then((r) => {
        if (r.error) setTestResult(`error: ${r.error}`);
        else if (r.clear_matched) setTestResult('clear-pattern matched → would resolve');
        else if (r.matched) setTestResult('matched → would fire');
        else setTestResult('no match');
      })
      .catch((e: unknown) => setTestResult(errMsg(e, 'test failed')));
  };

  const submit = () => {
    if (!valid) return;
    const body: EventRuleInput = {
      name: name.trim(),
      enabled,
      source_kind: sourceKind ? (sourceKind as EventRuleInput['source_kind']) : null,
      source_id: sourceId || null,
      node_id: null,
      match_kind: matchKind,
      pattern,
      clear_pattern: clearPattern.trim() || null,
      severity,
      ttl_secs: Number(ttl),
      min_count: Number(minCount),
      window_secs: Number(windowSecs),
    };
    setBusy(true);
    setError(null);
    const call =
      mode === 'edit' && rule
        ? api.updateEventRule(rule.id, body)
        : api.createEventRule(body).then(() => undefined);
    call.then(onDone).catch((e: unknown) => {
      setError(errMsg(e, 'failed to save rule'));
      setBusy(false);
    });
  };

  return (
    <Modal
      title={mode === 'edit' ? 'Edit event rule' : 'Add event rule'}
      onClose={onClose}
      footer={
        <>
          <Button variant="outline" onClick={onClose} disabled={busy}>
            Cancel
          </Button>
          <Button variant="primary" onClick={submit} disabled={!valid || busy}>
            {mode === 'edit' ? 'Save' : 'Add rule'}
          </Button>
        </>
      }
    >
      <div className="modal-field">
        <label className="modal-field-label">
          Name <RequiredMark />
        </label>
        <TextInput value={name} onChange={(e) => setName(e.target.value)} autoFocus />
      </div>
      <div className="modal-field-row">
        <div className="modal-field">
          <label className="modal-field-label">Match kind</label>
          <Select
            value={matchKind}
            onChange={(e) => setMatchKind(e.target.value as 'substring' | 'regex')}
          >
            <option value="substring">substring</option>
            <option value="regex">regex</option>
          </Select>
        </div>
        <div className="modal-field">
          <label className="modal-field-label">Severity</label>
          <Select value={severity} onChange={(e) => setSeverity(e.target.value as Severity)}>
            <option value="critical">critical</option>
            <option value="warning">warning</option>
            <option value="info">info — record only</option>
          </Select>
        </div>
      </div>
      <div className="modal-field">
        <label className="modal-field-label">
          Pattern <RequiredMark />
        </label>
        <TextInput
          className="mono"
          placeholder={matchKind === 'regex' ? '(?i)link down|%LINK-3' : 'link down'}
          value={pattern}
          onChange={(e) => setPattern(e.target.value)}
        />
        <FieldHint>Matching is case-sensitive; use {'(?i)'} in a regex for case-insensitive.</FieldHint>
      </div>
      <div className="modal-field">
        <label className="modal-field-label">Clear pattern</label>
        <TextInput
          className="mono"
          placeholder="link up"
          value={clearPattern}
          onChange={(e) => setClearPattern(e.target.value)}
        />
        <FieldHint>Optional — a message matching this resolves the alert (e.g. link-up clears link-down).</FieldHint>
      </div>
      <div className="modal-field-row">
        <div className="modal-field">
          <label className="modal-field-label">Source kind</label>
          <Select value={sourceKind} onChange={(e) => setSourceKind(e.target.value)}>
            <option value="">any</option>
            <option value="syslog">syslog</option>
            <option value="trap">trap</option>
            <option value="webhook">webhook</option>
          </Select>
        </div>
        <div className="modal-field">
          <label className="modal-field-label">Webhook source</label>
          <Select value={sourceId} onChange={(e) => setSourceId(e.target.value)}>
            <option value="">any</option>
            {sources.map((s) => (
              <option key={s.id} value={s.id}>
                {s.name}
              </option>
            ))}
          </Select>
        </div>
      </div>
      <div className="modal-field-row">
        <div className="modal-field">
          <label className="modal-field-label">Auto-close (s)</label>
          <TextInput
            type="number"
            value={ttl}
            onChange={(e) => setTtl(e.target.value)}
          />
        </div>
        <div className="modal-field">
          <label className="modal-field-label">Fire after N</label>
          <TextInput
            type="number"
            value={minCount}
            onChange={(e) => setMinCount(e.target.value)}
          />
        </div>
        <div className="modal-field">
          <label className="modal-field-label">within (s)</label>
          <TextInput
            type="number"
            value={windowSecs}
            onChange={(e) => setWindowSecs(e.target.value)}
          />
        </div>
      </div>
      <label className="eventrules-enabled">
        <input type="checkbox" checked={enabled} onChange={(e) => setEnabled(e.target.checked)} />
        <span>Enabled</span>
      </label>

      <div className="eventrules-tester">
        <label className="modal-field-label">Test against a sample</label>
        <div className="eventrules-tester-row">
          <TextInput
            className="mono"
            placeholder="paste a sample event message…"
            value={sample}
            onChange={(e) => setSample(e.target.value)}
          />
          <Button variant="outline" onClick={runTest} disabled={pattern.trim() === ''}>
            Test
          </Button>
        </div>
        {testResult && <p className="eventrules-tester-result">{testResult}</p>}
      </div>

      {error && <p className="form-error">{error}</p>}
    </Modal>
  );
}

function DeleteRuleModal({
  rule,
  onClose,
  onDone,
}: {
  rule: EventRule;
  onClose: () => void;
  onDone: () => void;
}) {
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const submit = () => {
    setBusy(true);
    setError(null);
    api
      .deleteEventRule(rule.id)
      .then(onDone)
      .catch((e: unknown) => {
        setError(errMsg(e, 'failed to delete rule'));
        setBusy(false);
      });
  };
  return (
    <Modal
      title="Delete event rule"
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
        Delete rule <strong>{rule.name}</strong>? Active alerts it raised will auto-close on their
        TTL; this only stops future matches.
      </p>
      {error && <p className="form-error">{error}</p>}
    </Modal>
  );
}
