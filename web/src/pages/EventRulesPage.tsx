// SPDX-License-Identifier: AGPL-3.0-only
import { useCallback, useEffect, useMemo, useState } from 'react';
import { Trans, useTranslation } from 'react-i18next';
import { api, errMsg, ApiError } from '../services/api';
import { useAuthStore } from '../store';
import type { EventRule, EventRuleInput, EventSource, Severity } from '../types/api';
import { PageHeader } from '../components/ui/PageHeader';
import { Card } from '../components/ui/Card';
import { Button } from '../components/ui/Button';
import { ConfirmDeleteModal } from '../components/ui/ConfirmDeleteModal';
import { Modal } from '../components/ui/Modal';
import { TextInput, Select, RequiredMark, FieldHint } from '../components/ui/Field';
import { Badge } from '../components/ui/Badge';
import { OverflowMenu } from '../components/ui/OverflowMenu';
import { TableToolbar, SearchInput, TableSpacer, ResultCount } from '../components/ui/TableToolbar';
import { EditIcon, TrashIcon, PowerIcon } from '../components/ui/icons';
import { severityLabel } from '../lib/format';
import './EventRulesPage.css';

const COLS = '1.4fr 130px 1fr 120px 110px 110px';
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
  const { t } = useTranslation('alertsConfig');
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
      .catch((e: unknown) => setError(errMsg(e, t('eventRules.err.update'))));
  };

  return (
    <div>
      <PageHeader title={t('nav:alerts.eventRules')} note={t('eventRules.note')} />
      {unavailable ? (
        <Card>{t('eventRules.unavailable')}</Card>
      ) : (
        <>
          <TableToolbar>
            <SearchInput
              value={query}
              onChange={setQuery}
              placeholder={t('eventRules.searchPlaceholder')}
              ariaLabel={t('eventRules.searchAria')}
            />
            <TableSpacer />
            <ResultCount
              shown={filtered.length}
              total={rows.length}
              noun={t('common:noun.rule', { count: rows.length })}
            />
            {authed && (
              <Button variant="primary" onClick={() => setAdding(true)}>
                {t('eventRules.add')}
              </Button>
            )}
          </TableToolbar>
          {error && <p className="form-error">{error}</p>}
          <div className="ytable eventrules-table">
            <div className="ytable-head" style={{ gridTemplateColumns: COLS }}>
              <div className="ytable-h">{t('eventRules.cols.name')}</div>
              <div className="ytable-h">{t('eventRules.cols.severity')}</div>
              <div className="ytable-h">{t('eventRules.cols.pattern')}</div>
              <div className="ytable-h">{t('eventRules.cols.scope')}</div>
              <div className="ytable-h">{t('eventRules.cols.status')}</div>
              <div className="ytable-h right">{t('eventRules.cols.actions')}</div>
            </div>
            {filtered.length === 0 ? (
              <div className="yt-empty">
                <p className="yt-empty-title">
                  {loading
                    ? t('common:loading')
                    : rows.length === 0
                      ? t('eventRules.empty')
                      : t('eventRules.emptyMatch')}
                </p>
                {!loading && (
                  <p className="yt-empty-sub">
                    {rows.length === 0
                      ? t('eventRules.emptySub')
                      : t('eventRules.emptyMatchSub')}
                  </p>
                )}
              </div>
            ) : (
              filtered.map((r) => (
                <div className="ytable-row" key={r.id} style={{ gridTemplateColumns: COLS }}>
                  <div className="ytable-cell">{r.name}</div>
                  <div className="ytable-cell">
                    <Badge tone={SEVERITY_TONE[r.severity]}>{severityLabel(r.severity)}</Badge>
                  </div>
                  <div className="ytable-cell eventrules-match">
                    <div className="eventrules-sig">
                      <span className="eventrules-sig-kind">
                        {t(`eventRules.matchKind.${r.match_kind}`)}
                      </span>
                      <span className="eventrules-sig-val mono" title={r.pattern}>
                        {r.pattern}
                      </span>
                    </div>
                    {r.clear_pattern && (
                      <div className="eventrules-sig">
                        <span className="eventrules-sig-kind">{t('eventRules.clear')}</span>
                        <span className="eventrules-sig-val mono" title={r.clear_pattern}>
                          {r.clear_pattern}
                        </span>
                      </div>
                    )}
                  </div>
                  <div className="ytable-cell mono">{r.source_kind ?? t('eventRules.any')}</div>
                  <div className="ytable-cell">
                    <Badge tone={r.enabled ? 'up' : 'neutral'}>
                      {r.enabled ? t('status.enabled') : t('status.disabled')}
                    </Badge>
                  </div>
                  <div className="ytable-cell right">
                    {authed && (
                      <span className="ytable-actions">
                        <OverflowMenu
                          actions={[
                            {
                              label: r.enabled ? t('eventRules.disable') : t('eventRules.enable'),
                              icon: <PowerIcon />,
                              onClick: () => toggleEnabled(r),
                            },
                            {
                              label: t('eventRules.edit'),
                              icon: <EditIcon />,
                              onClick: () => setEditing(r),
                            },
                            {
                              label: t('eventRules.delete'),
                              icon: <TrashIcon />,
                              danger: true,
                              onClick: () => setDeleting(r),
                            },
                          ]}
                        />
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
  const { t } = useTranslation('alertsConfig');
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
        if (r.error) setTestResult(t('eventRules.test.error', { msg: r.error }));
        else if (r.clear_matched) setTestResult(t('eventRules.test.clearMatched'));
        else if (r.matched) setTestResult(t('eventRules.test.matched'));
        else setTestResult(t('eventRules.test.noMatch'));
      })
      .catch((e: unknown) => setTestResult(errMsg(e, t('eventRules.test.failed'))));
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
      setError(errMsg(e, t('eventRules.err.save')));
      setBusy(false);
    });
  };

  return (
    <Modal
      title={mode === 'edit' ? t('eventRules.modal.editTitle') : t('eventRules.modal.addTitle')}
      onClose={onClose}
      footer={
        <>
          <Button variant="outline" onClick={onClose} disabled={busy}>
            {t('common:actions.cancel')}
          </Button>
          <Button variant="primary" onClick={submit} disabled={!valid || busy}>
            {mode === 'edit' ? t('common:actions.save') : t('eventRules.modal.add')}
          </Button>
        </>
      }
    >
      <div className="modal-field">
        <label className="modal-field-label">
          {t('eventRules.modal.name')} <RequiredMark />
        </label>
        <TextInput value={name} onChange={(e) => setName(e.target.value)} autoFocus />
      </div>
      <div className="modal-field-row">
        <div className="modal-field">
          <label className="modal-field-label">{t('eventRules.modal.matchKind')}</label>
          <Select
            value={matchKind}
            onChange={(e) => setMatchKind(e.target.value as 'substring' | 'regex')}
          >
            <option value="substring">{t('eventRules.matchKind.substring')}</option>
            <option value="regex">{t('eventRules.matchKind.regex')}</option>
          </Select>
        </div>
        <div className="modal-field">
          <label className="modal-field-label">{t('eventRules.modal.severity')}</label>
          <Select value={severity} onChange={(e) => setSeverity(e.target.value as Severity)}>
            <option value="critical">{severityLabel('critical')}</option>
            <option value="warning">{severityLabel('warning')}</option>
            <option value="info">
              {t('eventRules.modal.severityInfo', { label: severityLabel('info') })}
            </option>
          </Select>
        </div>
      </div>
      <div className="modal-field">
        <label className="modal-field-label">
          {t('eventRules.modal.pattern')} <RequiredMark />
        </label>
        <TextInput
          className="mono"
          placeholder={matchKind === 'regex' ? '(?i)link down|%LINK-3' : 'link down'}
          value={pattern}
          onChange={(e) => setPattern(e.target.value)}
        />
        <FieldHint>{t('eventRules.modal.patternHint')}</FieldHint>
      </div>
      <div className="modal-field">
        <label className="modal-field-label">{t('eventRules.modal.clearPattern')}</label>
        <TextInput
          className="mono"
          placeholder="link up"
          value={clearPattern}
          onChange={(e) => setClearPattern(e.target.value)}
        />
        <FieldHint>{t('eventRules.modal.clearPatternHint')}</FieldHint>
      </div>
      <div className="modal-field-row">
        <div className="modal-field">
          <label className="modal-field-label">{t('eventRules.modal.sourceKind')}</label>
          <Select value={sourceKind} onChange={(e) => setSourceKind(e.target.value)}>
            <option value="">{t('eventRules.any')}</option>
            <option value="syslog">syslog</option>
            <option value="trap">trap</option>
            <option value="webhook">webhook</option>
          </Select>
        </div>
        <div className="modal-field">
          <label className="modal-field-label">{t('eventRules.modal.webhookSource')}</label>
          <Select value={sourceId} onChange={(e) => setSourceId(e.target.value)}>
            <option value="">{t('eventRules.any')}</option>
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
          <label className="modal-field-label">{t('eventRules.modal.autoClose')}</label>
          <TextInput
            type="number"
            value={ttl}
            onChange={(e) => setTtl(e.target.value)}
          />
        </div>
        <div className="modal-field">
          <label className="modal-field-label">{t('eventRules.modal.fireAfter')}</label>
          <TextInput
            type="number"
            value={minCount}
            onChange={(e) => setMinCount(e.target.value)}
          />
        </div>
        <div className="modal-field">
          <label className="modal-field-label">{t('eventRules.modal.within')}</label>
          <TextInput
            type="number"
            value={windowSecs}
            onChange={(e) => setWindowSecs(e.target.value)}
          />
        </div>
      </div>
      <label className="eventrules-enabled">
        <input type="checkbox" checked={enabled} onChange={(e) => setEnabled(e.target.checked)} />
        <span>{t('eventRules.modal.enabled')}</span>
      </label>

      <div className="eventrules-tester">
        <label className="modal-field-label">{t('eventRules.modal.test')}</label>
        <div className="eventrules-tester-row">
          <TextInput
            className="mono"
            placeholder={t('eventRules.modal.samplePlaceholder')}
            value={sample}
            onChange={(e) => setSample(e.target.value)}
          />
          <Button variant="outline" onClick={runTest} disabled={pattern.trim() === ''}>
            {t('eventRules.modal.testBtn')}
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
  const { t } = useTranslation('alertsConfig');
  return (
    <ConfirmDeleteModal
      title={t('eventRules.deleteModal.title')}
      onConfirm={() => api.deleteEventRule(rule.id)}
      errorFallback={t('eventRules.err.delete')}
      onClose={onClose}
      onDone={onDone}
    >
      <Trans
        t={t}
        i18nKey="eventRules.deleteModal.body"
        values={{ name: rule.name }}
        components={{ strong: <strong /> }}
      />
    </ConfirmDeleteModal>
  );
}
