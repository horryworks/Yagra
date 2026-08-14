// SPDX-License-Identifier: AGPL-3.0-only
import { useCallback, useEffect, useMemo, useState } from 'react';
import { Trans, useTranslation } from 'react-i18next';
import { api, errMsg, ApiError } from '../services/api';
import { useAuthStore } from '../store';
import {
  SEVERITIES,
  type EventRule,
  type EventRuleInput,
  type EventSource,
  type Severity,
} from '../types/api';
import { PageHeader } from '../components/ui/PageHeader';
import { Card } from '../components/ui/Card';
import { Button } from '../components/ui/Button';
import { ConfirmDeleteModal } from '../components/ui/ConfirmDeleteModal';
import { Modal } from '../components/ui/Modal';
import { TextInput, Select, RequiredMark, FieldHint } from '../components/ui/Field';
import { Badge } from '../components/ui/Badge';
import { OverflowMenu } from '../components/ui/OverflowMenu';
import { TableToolbar, TableSpacer, ResultCount } from '../components/ui/TableToolbar';
import { DataTable, type Column } from '../components/ui/DataTable';
import { ClearFilters } from '../components/ui/ClearFilters';
import { FilterButton, MobileFilterSheet } from '../components/ui/MobileFilterSheet';
import { useClientFilters } from '../lib/useClientFilters';
import { eventRuleFilters } from './eventConfigFilters';
import { EditIcon, TrashIcon, PowerIcon } from '../components/ui/icons';
import { severityLabel } from '../lib/format';
import './EventRulesPage.css';

const SEVERITY_TONE: Record<Severity, 'critical' | 'warning' | 'info'> = {
  critical: 'critical',
  warning: 'warning',
  info: 'info',
};

function SeverityBadge({ value }: { value: Severity }) {
  return <Badge tone={SEVERITY_TONE[value]}>{severityLabel(value)}</Badge>;
}

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
  const [sheet, setSheet] = useState(false);
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

  const toggleEnabled = (r: EventRule) => {
    setError(null);
    api
      .updateEventRule(r.id, { ...ruleToInput(r), enabled: !r.enabled })
      .then(load)
      .catch((e: unknown) => setError(errMsg(e, t('eventRules.err.update'))));
  };

  const columns = useMemo<Column<EventRule>[]>(() => {
    const scopeLabel = (r: EventRule) => r.source_kind ?? t('eventRules.any');
    const specs = eventRuleFilters(t, SEVERITIES, scopeLabel);
    const cols: Column<EventRule>[] = [
      { key: 'name', header: t('eventRules.cols.name'), width: '1.4fr', render: (r) => r.name },
      {
        key: 'severity',
        header: t('eventRules.cols.severity'),
        width: '130px',
        render: (r) => <SeverityBadge value={r.severity} />,
      },
      {
        key: 'pattern',
        header: t('eventRules.cols.pattern'),
        width: '1fr',
        render: (r) => (
          <span className="eventrules-match">
            <span className="eventrules-sig">
              <span className="eventrules-sig-kind">
                {t(`eventRules.matchKind.${r.match_kind}`)}
              </span>
              <span className="eventrules-sig-val mono" title={r.pattern}>
                {r.pattern}
              </span>
            </span>
            {r.clear_pattern && (
              <span className="eventrules-sig">
                <span className="eventrules-sig-kind">{t('eventRules.clear')}</span>
                <span className="eventrules-sig-val mono" title={r.clear_pattern}>
                  {r.clear_pattern}
                </span>
              </span>
            )}
          </span>
        ),
      },
      {
        key: 'scope',
        header: t('eventRules.cols.scope'),
        width: '120px',
        render: (r) => <span className="mono">{scopeLabel(r)}</span>,
      },
      {
        key: 'status',
        header: t('eventRules.cols.status'),
        width: '110px',
        render: (r) => (
          <Badge tone={r.enabled ? 'up' : 'neutral'}>
            {r.enabled ? t('status.enabled') : t('status.disabled')}
          </Badge>
        ),
      },
      {
        key: 'actions',
        header: t('eventRules.cols.actions'),
        width: '110px',
        align: 'right',
        render: (r) =>
          authed ? (
            <span className="ytable-actions">
              <OverflowMenu
                actions={[
                  {
                    label: r.enabled ? t('eventRules.disable') : t('eventRules.enable'),
                    icon: <PowerIcon />,
                    onClick: () => toggleEnabled(r),
                  },
                  { label: t('eventRules.edit'), icon: <EditIcon />, onClick: () => setEditing(r) },
                  {
                    label: t('eventRules.delete'),
                    icon: <TrashIcon />,
                    danger: true,
                    onClick: () => setDeleting(r),
                  },
                ]}
              />
            </span>
          ) : null,
      },
    ];
    for (const c of cols) c.filter = specs[c.key];
    return cols;
    // `toggleEnabled` is rebuilt every render; listing it would rebuild the columns on every
    // keystroke elsewhere and re-run the predicate for nothing.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [t, authed]);

  const { filterCols, filters, setFilters, clear, shown, counts, anyFiltered } = useClientFilters(
    columns,
    rows,
    { url: true },
  );

  return (
    <div>
      <PageHeader title={t('nav:alerts.eventRules')} note={t('eventRules.note')} />
      {unavailable ? (
        <Card>{t('eventRules.unavailable')}</Card>
      ) : (
        <>
          <TableToolbar>
            <FilterButton
              columns={filterCols}
              filters={filters}
              onOpen={() => setSheet(true)}
            />
            <ClearFilters columns={filterCols} filters={filters} onClear={clear} />
            <TableSpacer />
            <ResultCount
              shown={shown.length}
              total={anyFiltered ? rows.length : undefined}
              noun={t('common:noun.rule', { count: rows.length })}
            />
            {authed && (
              <Button variant="primary" onClick={() => setAdding(true)}>
                {t('eventRules.add')}
              </Button>
            )}
          </TableToolbar>
          {error && <p className="form-error">{error}</p>}
          <DataTable
            rows={shown}
            columns={columns}
            rowKey={(r) => r.id}
            filters={filters}
            onFiltersChange={setFilters}
            filterCounts={counts}
            loading={loading}
            empty={anyFiltered ? t('eventRules.emptyMatch') : t('eventRules.empty')}
          />
          {sheet && (
            <MobileFilterSheet
              columns={filterCols}
              filters={filters}
              onChange={setFilters}
              counts={counts}
              labels={Object.fromEntries(columns.map((c) => [c.key, String(c.header)]))}
              onClose={() => setSheet(false)}
            />
          )}
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
  // `match_kind` is a `String` on the wire too; the DB CHECK admits only these two.
  const [matchKind, setMatchKind] = useState<'substring' | 'regex'>(
    rule?.match_kind === 'regex' ? 'regex' : 'substring',
  );
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
