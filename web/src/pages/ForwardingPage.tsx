// SPDX-License-Identifier: AGPL-3.0-only
// Forwarding (Settings ▸ Forwarding): relay received syslog / SNMP traps / flow exports on to
// external collectors — a SIEM, an existing syslog estate, an analytics pipeline (ADR-034).
// ManageConfig-gated and audited: a destination sends log bodies, which routinely carry
// credentials, off-box.
//
// Data-table standard v2: a toolbar (New + count) over the shared `DataTable`, edit/test/delete as
// per-row OverflowMenu actions with modals. The form mirrors core's validation rules through the
// pure helpers in `forwardingOptions.ts`, so an impossible combination is never offered.

import { useCallback, useEffect, useMemo, useState } from 'react';
import { useTranslation } from 'react-i18next';
import type { TFunction } from 'i18next';
import { api, errMsg, ApiError } from '../services/api';
import { useAuthStore } from '../store';
import type {
  ForwardDestKind,
  ForwardDestStatus,
  ForwardDestination,
  ForwardFilterField,
  ForwardFilterOp,
  ForwardSourceKind,
  ForwardStatus,
} from '../types/api';
import { FORWARD_SOURCE_KINDS } from '../types/api';
import {
  destKindsForSource,
  fieldsForSource,
  opsForField,
  reconcileDraft,
  supportsRendered,
  supportsVerbatim,
  filtersWholeDatagram,
  usesCommunity,
  usesHostPort,
  usesServiceAccount,
  usesTls,
} from './forwardingOptions';
import { PageHeader } from '../components/ui/PageHeader';
import { Card } from '../components/ui/Card';
import { Button } from '../components/ui/Button';
import { ConfirmDeleteModal } from '../components/ui/ConfirmDeleteModal';
import { Badge } from '../components/ui/Badge';
import { Modal } from '../components/ui/Modal';
import { TextInput, TextArea, Select } from '../components/ui/Field';
import { DataTable, type Column } from '../components/ui/DataTable';
import { TableToolbar, TableSpacer, ResultCount } from '../components/ui/TableToolbar';
import { ClearFilters } from '../components/ui/ClearFilters';
import { MobileFilterButton, MobileFilterSheet } from '../components/ui/MobileFilterSheet';
import { useClientFilters } from '../lib/useClientFilters';
import { forwardingFilters } from './forwardingListFilters';
import { OverflowMenu } from '../components/ui/OverflowMenu';
import { EditIcon, PowerIcon, TrashIcon } from '../components/ui/icons';
import {
  draftFrom,
  emptyDraft,
  toInput,
  type Draft,
  type DraftCondition,
} from './forwardingDraft';
import './ForwardingPage.css';

const STATUS_POLL_MS = 10_000;

/** Table columns. Renderers close over `t`, so the caller rebuilds them on a language switch. */
function destinationColumns(
  t: TFunction,
  status: Map<string, ForwardDestStatus>,
  rows: readonly ForwardDestination[],
  onEdit: (row: ForwardDestination) => void,
  onTest: (row: ForwardDestination) => void,
  onDelete: (row: ForwardDestination) => void,
): Column<ForwardDestination>[] {
  const specs = forwardingFilters(t, rows);
  const cols: Column<ForwardDestination>[] = [
    {
      key: 'name',
      header: t('cols.name'),
      width: '1fr',
      render: (r) => <span className="fwd-name">{r.name}</span>,
    },
    {
      key: 'source',
      header: t('cols.source'),
      width: '110px',
      render: (r) => <Badge tone="info">{t(`source.${r.source_kind}`)}</Badge>,
    },
    {
      key: 'target',
      header: t('cols.target'),
      width: '1fr',
      render: (r) => <span className="mono">{r.target}</span>,
    },
    {
      // Split out of Target by ADR-053 Inc.3: the cell rendered the address *and* the protocol, and
      // the destination-kind dropdown the toolbar used to carry had nowhere to land while the two
      // shared a column. One column, one filter.
      key: 'dest',
      header: t('cols.dest'),
      width: '120px',
      render: (r) => <span className="fwd-sub">{t(`dest.${r.dest_kind}`)}</span>,
    },
    {
      // Likewise the enabled state, which was a badge tucked beside the name.
      key: 'status',
      header: t('cols.status'),
      width: '110px',
      render: (r) =>
        r.enabled ? (
          <Badge tone="up">{t('common:filter.enabled')}</Badge>
        ) : (
          <Badge tone="neutral">{t('status.disabled')}</Badge>
        ),
    },
    {
      key: 'scope',
      header: t('cols.scope'),
      width: '120px',
      render: (r) => r.pool ?? <span className="muted">{t('scope.allPools')}</span>,
    },
    {
      key: 'fidelity',
      header: t('cols.fidelity'),
      width: '120px',
      render: (r) => (
        <Badge tone={r.verbatim ? 'up' : 'neutral'}>
          {t(r.verbatim ? 'fidelity.verbatim' : 'fidelity.rendered')}
        </Badge>
      ),
    },
    {
      key: 'filter',
      header: t('cols.filter'),
      width: '130px',
      render: (r) => {
        const n = r.filter?.conditions?.length ?? 0;
        return n === 0 ? (
          <span className="muted">{t('filter.none')}</span>
        ) : (
          <span>{t('filter.count', { count: n, mode: t(`filter.mode.${r.filter.mode}`) })}</span>
        );
      },
    },
    {
      key: 'health',
      header: t('cols.health'),
      width: '210px',
      render: (r) => {
        const s = status.get(r.id);
        if (!s) return <span className="muted">—</span>;
        return (
          <span className="fwd-health">
            {s.circuit_open && <Badge tone="critical">{t('health.paused')}</Badge>}
            <span className="mono" title={s.last_error ?? undefined}>
              {t('health.counts', { sent: s.sent, dropped: s.dropped + s.errors })}
            </span>
            {s.rendered > 0 && r.verbatim && (
              <Badge tone="warning" title={t('health.degradedHint')}>
                {t('health.degraded')}
              </Badge>
            )}
          </span>
        );
      },
    },
    {
      key: 'actions',
      header: t('cols.actions'),
      width: '96px',
      align: 'right',
      render: (r) => (
        <OverflowMenu
          actions={[
            { label: t('actions.edit'), icon: <EditIcon />, onClick: () => onEdit(r) },
            { label: t('actions.test'), icon: <PowerIcon />, onClick: () => onTest(r) },
            {
              label: t('actions.delete'),
              icon: <TrashIcon />,
              danger: true,
              onClick: () => onDelete(r),
            },
          ]}
        />
      ),
    },
  ];
  // Attached by key, so a column with no spec has no control and a spec with no column is visible
  // here rather than a silent no-op.
  for (const c of cols) c.filter = specs[c.key];
  return cols;
}

/** One `field op value` row of the filter builder. */
function ConditionRow({
  t,
  source,
  condition,
  onChange,
  onRemove,
}: {
  t: TFunction;
  source: ForwardSourceKind;
  condition: DraftCondition;
  onChange: (next: DraftCondition) => void;
  onRemove: () => void;
}) {
  const ops = opsForField(condition.field);
  return (
    <div className="fwd-cond">
      <Select
        value={condition.field}
        onChange={(e) => {
          const field = e.target.value as ForwardFilterField;
          // Keep the operator only if the new field's type still accepts it.
          const op = opsForField(field).includes(condition.op) ? condition.op : opsForField(field)[0];
          onChange({ ...condition, field, op });
        }}
      >
        {fieldsForSource(source).map((f) => (
          <option key={f} value={f}>
            {t(`field.${f}`)}
          </option>
        ))}
      </Select>
      <Select
        value={condition.op}
        onChange={(e) => onChange({ ...condition, op: e.target.value as ForwardFilterOp })}
      >
        {ops.map((op) => (
          <option key={op} value={op}>
            {t(`op.${op}`)}
          </option>
        ))}
      </Select>
      <TextInput
        value={condition.value}
        placeholder={t(`valuePlaceholder.${condition.field}`, { defaultValue: '' })}
        onChange={(e) => onChange({ ...condition, value: e.target.value })}
      />
      <Button variant="outline" onClick={onRemove} aria-label={t('filter.remove')}>
        ×
      </Button>
    </div>
  );
}

/** Create/edit a destination. `existing` null = create. */
function DestinationModal({
  existing,
  onClose,
  onSaved,
}: {
  existing: ForwardDestination | null;
  onClose: () => void;
  onSaved: () => void;
}) {
  const { t } = useTranslation('settings-forwarding');
  const [draft, setDraft] = useState<Draft>(() => (existing ? draftFrom(existing) : emptyDraft()));
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  // Every mutation goes through this so the draft can never hold a combination core would reject.
  const update = (patch: Partial<Draft>) => setDraft((d) => reconcileDraft({ ...d, ...patch }));

  const verbatimPossible = supportsVerbatim(draft.source_kind, draft.dest_kind);
  const renderedPossible = supportsRendered(draft.source_kind, draft.dest_kind);
  const hostPort = usesHostPort(draft.dest_kind);
  const rowsOnly = usesServiceAccount(draft.dest_kind);
  const ready = draft.name.trim() !== '' && draft.target.trim() !== '';

  const submit = () => {
    if (!ready) return;
    setBusy(true);
    setError(null);
    const body = toInput(draft);
    const call = existing
      ? api.updateForwardDestination(existing.id, body)
      : api.createForwardDestination(body).then(() => undefined);
    call.then(onSaved).catch((e: unknown) => {
      setError(
        e instanceof ApiError && e.code === 'duplicate_name'
          ? t('err.duplicate')
          : errMsg(e, t('err.save')),
      );
      setBusy(false);
    });
  };

  return (
    <Modal
      title={t(existing ? 'edit.title' : 'add.title')}
      size="wide"
      onClose={onClose}
      footer={
        <>
          <Button variant="outline" onClick={onClose} disabled={busy}>
            {t('common:actions.cancel')}
          </Button>
          <Button variant="primary" onClick={submit} disabled={!ready || busy}>
            {t('common:actions.save')}
          </Button>
        </>
      }
    >
      <div className="modal-field">
        <label className="modal-field-label">{t('field.name')}</label>
        <TextInput
          value={draft.name}
          placeholder={t('field.namePlaceholder')}
          onChange={(e) => update({ name: e.target.value })}
          autoFocus
        />
      </div>

      <div className="fwd-row">
        <div className="modal-field">
          <label className="modal-field-label">{t('field.source')}</label>
          <Select
            value={draft.source_kind}
            onChange={(e) => update({ source_kind: e.target.value as ForwardSourceKind })}
          >
            {FORWARD_SOURCE_KINDS.map((k) => (
              <option key={k} value={k}>
                {t(`source.${k}`)}
              </option>
            ))}
          </Select>
        </div>
        <div className="modal-field">
          <label className="modal-field-label">{t('field.dest')}</label>
          <Select
            value={draft.dest_kind}
            onChange={(e) => update({ dest_kind: e.target.value as ForwardDestKind })}
          >
            {destKindsForSource(draft.source_kind).map((k) => (
              <option key={k} value={k}>
                {t(`dest.${k}`)}
              </option>
            ))}
          </Select>
        </div>
      </div>

      <div className="fwd-row">
        {/* Two target shapes, and the difference is not cosmetic: a relay is addressed by
            `host:port`, BigQuery by `project.dataset.table`. Relabelling rather than sharing one
            vague placeholder is what stops an admin typing a host into a table field. */}
        <div className="modal-field">
          <label className="modal-field-label">
            {hostPort ? t('field.target') : t('field.targetTable')}
          </label>
          <TextInput
            value={draft.target}
            placeholder={hostPort ? t('field.targetPlaceholder') : t('field.targetTablePlaceholder')}
            onChange={(e) => update({ target: e.target.value })}
          />
          <span className="modal-hint">
            {hostPort ? t('field.targetHint') : t('field.targetTableHint')}
          </span>
        </div>
        <div className="modal-field">
          <label className="modal-field-label">{t('field.pool')}</label>
          <TextInput
            value={draft.pool}
            placeholder={t('field.poolPlaceholder')}
            onChange={(e) => update({ pool: e.target.value })}
          />
        </div>
      </div>

      <div className="modal-field">
        <label className="modal-field-label">{t('field.fidelity')}</label>
        <Select
          value={rowsOnly ? 'rows' : draft.verbatim ? 'verbatim' : 'rendered'}
          disabled={rowsOnly || !verbatimPossible || !renderedPossible}
          onChange={(e) => update({ verbatim: e.target.value === 'verbatim' })}
        >
          {/* BigQuery is neither fidelity: it produces normalized rows. Showing a disabled third
              value is more honest than leaving "rendered" selected and hoping the hint is read. */}
          {rowsOnly ? (
            <option value="rows">{t('fidelity.rows')}</option>
          ) : (
            <>
              <option value="verbatim">{t('fidelity.verbatim')}</option>
              <option value="rendered">{t('fidelity.rendered')}</option>
            </>
          )}
        </Select>
        <span className="modal-hint">
          {rowsOnly
            ? t('field.fidelityRowsOnly')
            : !renderedPossible
              ? t('field.fidelityVerbatimOnly')
              : verbatimPossible
                ? t('field.fidelityHint')
                : t('field.fidelityImpossible')}
        </span>
      </div>

      {usesServiceAccount(draft.dest_kind) && (
        <div className="modal-field">
          <label className="modal-field-label">{t('field.serviceAccount')}</label>
          <TextArea
            value={draft.service_account_json}
            placeholder={
              existing?.has_secret
                ? t('field.serviceAccountKept')
                : t('field.serviceAccountPlaceholder')
            }
            spellCheck={false}
            onChange={(e) => update({ service_account_json: e.target.value })}
          />
          <span className="modal-hint">{t('field.serviceAccountHint')}</span>
        </div>
      )}

      {usesTls(draft.dest_kind) && (
        <div className="modal-field">
          <label className="modal-field-label">{t('field.caCert')}</label>
          <TextArea
            value={draft.ca_cert}
            placeholder={t('field.caCertPlaceholder')}
            spellCheck={false}
            onChange={(e) => update({ ca_cert: e.target.value })}
          />
          <span className="modal-hint">{t('field.caCertHint')}</span>
        </div>
      )}

      {usesCommunity(draft.dest_kind) && (
        <div className="modal-field">
          <label className="modal-field-label">{t('field.community')}</label>
          <TextInput
            type="password"
            value={draft.community}
            placeholder={existing?.has_secret ? t('field.communityKept') : 'public'}
            onChange={(e) => update({ community: e.target.value })}
          />
          <span className="modal-hint">{t('field.communityHint')}</span>
        </div>
      )}

      <div className="modal-field">
        <label className="modal-field-label">{t('field.rateLimit')}</label>
        <TextInput
          value={draft.rate_limit}
          inputMode="numeric"
          placeholder={t('field.rateLimitPlaceholder')}
          onChange={(e) => update({ rate_limit: e.target.value.replace(/[^0-9]/g, '') })}
        />
        <span className="modal-hint">{t('field.rateLimitHint')}</span>
      </div>

      <div className="modal-field">
        <label className="modal-field-label">{t('filter.label')}</label>
        <div className="fwd-mode">
          <Select
            value={draft.mode}
            onChange={(e) => update({ mode: e.target.value as 'all' | 'any' })}
          >
            <option value="all">{t('filter.mode.all')}</option>
            <option value="any">{t('filter.mode.any')}</option>
          </Select>
          <Button
            variant="outline"
            onClick={() => {
              const field = fieldsForSource(draft.source_kind)[0];
              update({
                conditions: [
                  ...draft.conditions,
                  { field, op: opsForField(field)[0], value: '' },
                ],
              });
            }}
          >
            + {t('filter.add')}
          </Button>
        </div>
        {/* A flow export is a template + many records, and records cannot be removed from one
            without re-encoding it. Saying so here is the difference between a filter that behaves
            surprisingly and one that behaves as documented. */}
        {filtersWholeDatagram(draft.source_kind, draft.dest_kind) &&
          draft.conditions.length > 0 && (
            <span className="modal-hint fwd-warn">{t('filter.flowAnyRecord')}</span>
          )}
        {/* ...and the converse, because it is the reason to pick BigQuery for a filtered flow
            feed: rows are independent, so a non-matching record is simply not written. */}
        {draft.source_kind === 'flow' && rowsOnly && draft.conditions.length > 0 && (
          <span className="modal-hint">{t('filter.flowPerRecord')}</span>
        )}
        {draft.conditions.length === 0 ? (
          <span className="modal-hint">{t('filter.emptyHint')}</span>
        ) : (
          draft.conditions.map((c, i) => (
            <ConditionRow
              // Conditions are an ordered list with no stable id; the index is the identity here.
              key={i}
              t={t}
              source={draft.source_kind}
              condition={c}
              onChange={(next) =>
                update({ conditions: draft.conditions.map((old, j) => (j === i ? next : old)) })
              }
              onRemove={() =>
                update({ conditions: draft.conditions.filter((_, j) => j !== i) })
              }
            />
          ))
        )}
      </div>

      <div className="modal-field">
        <label className="modal-field-label">{t('field.enabled')}</label>
        <Select
          value={draft.enabled ? 'on' : 'off'}
          onChange={(e) => update({ enabled: e.target.value === 'on' })}
        >
          <option value="on">{t('status.enabled')}</option>
          <option value="off">{t('status.disabled')}</option>
        </Select>
      </div>

      <p className="modal-hint fwd-warn">{t('securityNote')}</p>
      {error && <p className="form-error">{error}</p>}
    </Modal>
  );
}

/** Confirm + delete a destination. */
function DeleteModal({
  row,
  onClose,
  onDone,
}: {
  row: ForwardDestination;
  onClose: () => void;
  onDone: () => void;
}) {
  const { t } = useTranslation('settings-forwarding');
  return (
    <ConfirmDeleteModal
      title={t('delete.title')}
      confirmLabel={t('actions.delete')}
      onConfirm={() => api.deleteForwardDestination(row.id)}
      errorFallback={t('err.delete')}
      onClose={onClose}
      onDone={onDone}
    >
      {t('delete.confirm', { name: row.name })}
    </ConfirmDeleteModal>
  );
}

export function ForwardingPage() {
  const { t } = useTranslation('settings-forwarding');
  const authed = useAuthStore((s) => s.authed);
  const [rows, setRows] = useState<ForwardDestination[]>([]);
  const [status, setStatus] = useState<ForwardStatus | null>(null);
  const [unavailable, setUnavailable] = useState(false);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [editing, setEditing] = useState<ForwardDestination | null>(null);
  const [adding, setAdding] = useState(false);
  const [deleting, setDeleting] = useState<ForwardDestination | null>(null);
  const [testResult, setTestResult] = useState<{ name: string; text: string } | null>(null);
  const [sheet, setSheet] = useState(false);

  const load = useCallback(() => {
    setError(null);
    api
      .listForwardDestinations()
      .then((list) => {
        setRows(list);
        setUnavailable(false);
      })
      .catch((e: unknown) => {
        if (e instanceof ApiError && (e.status === 401 || e.status === 403)) setUnavailable(true);
        else setError(errMsg(e, t('err.load')));
      })
      .finally(() => setLoading(false));
  }, [t]);

  useEffect(() => {
    if (authed) load();
    else setLoading(false);
  }, [authed, load]);

  // Counters are live, so poll them separately from the (edit-driven) destination list.
  useEffect(() => {
    if (!authed || unavailable) return undefined;
    let alive = true;
    const tick = () => {
      api
        .forwardingStatus()
        .then((s) => {
          if (alive) setStatus(s);
        })
        .catch(() => {
          /* transient: the table simply shows no counters this cycle */
        });
    };
    tick();
    const id = window.setInterval(tick, STATUS_POLL_MS);
    return () => {
      alive = false;
      window.clearInterval(id);
    };
  }, [authed, unavailable]);

  const statusById = useMemo(
    () => new Map((status?.destinations ?? []).map((s) => [s.id, s])),
    [status],
  );

  const runTest = useCallback(
    (row: ForwardDestination) => {
      api
        .testForwardDestination(row.id)
        .then((r) =>
          setTestResult({
            name: row.name,
            // "Handed to the collector" is the honest wording for a fire-and-forget datagram, but a
            // BigQuery test actually round-trips — say what really happened rather than under-claim.
            text: r.delivered
              ? t(row.dest_kind === 'bigquery' ? 'test.okBigQuery' : 'test.ok')
              : t('test.failed', { error: r.error ?? '' }),
          }),
        )
        .catch((e: unknown) => setTestResult({ name: row.name, text: errMsg(e, t('err.test')) }));
    },
    [t],
  );

  const columns = useMemo(
    () => destinationColumns(t, statusById, rows, setEditing, runTest, setDeleting),
    [t, statusById, rows, runTest],
  );
  // Client-side: the destination list is bounded by what an operator configured, not by fleet size
  // (ui-conventions). URL-backed — one table on this route, so a filtered view is linkable.
  const { filterCols, filters, setFilters, clear, shown, counts, anyFiltered } = useClientFilters(
    columns,
    rows,
    { url: true },
  );

  // A byte-exact destination cannot be honoured for traffic from a poller that predates raw
  // capture — say so rather than silently shipping re-rendered output.
  const staleP = status?.pollers_without_raw_capture ?? [];
  const wantsVerbatim = rows.some((r) => r.enabled && r.verbatim && r.source_kind !== 'flow');
  // A poller that predates the flow relay sends no datagrams at all, so a flow destination fed only
  // by such pollers receives nothing — a harder failure than degraded fidelity, and worth its own
  // wording rather than being folded into the banner above.
  const noFlowRelay = status?.pollers_without_flow_relay ?? [];
  const wantsFlow = rows.some((r) => r.enabled && r.source_kind === 'flow');

  return (
    <div className="page-fill">
      <PageHeader
        title={t('nav:settings.forwarding')}
        trail={[{ label: t('nav:sections.settings') }, { label: t('nav:settings.forwarding') }]}
        note={t('note')}
      />

      {!authed ? (
        <Card>
          <p className="muted">{t('signInPrompt')}</p>
        </Card>
      ) : unavailable ? (
        <Card>
          <p className="muted">{t('unavailable')}</p>
        </Card>
      ) : (
        <>
          {wantsVerbatim && staleP.length > 0 && (
            <Card>
              <p className="fwd-warn">{t('warn.noRawCapture', { pollers: staleP.join(', ') })}</p>
            </Card>
          )}
          {wantsFlow && noFlowRelay.length > 0 && (
            <Card>
              <p className="fwd-warn">
                {t('warn.noFlowRelay', { pollers: noFlowRelay.join(', ') })}
              </p>
            </Card>
          )}
          {status && !status.sending && (
            <Card>
              <p className="muted">{t('warn.standby')}</p>
            </Card>
          )}

          <TableToolbar>
            <MobileFilterButton
              columns={filterCols}
              filters={filters}
              onOpen={() => setSheet(true)}
            />
            <ClearFilters columns={filterCols} filters={filters} onClear={clear} />
            <TableSpacer />
            <ResultCount
              shown={shown.length}
              total={anyFiltered ? rows.length : undefined}
              noun={t('count', { count: shown.length })}
            />
            <Button variant="primary" onClick={() => setAdding(true)}>
              + {t('add.button')}
            </Button>
          </TableToolbar>

          {error && <p className="form-error">{error}</p>}

          <DataTable
            rows={shown}
            columns={columns}
            filters={filters}
            onFiltersChange={setFilters}
            filterCounts={counts}
            rowKey={(r) => r.id}
            loading={loading}
            empty={anyFiltered ? t('common:filter.noMatch') : t('empty')}
          />
          {sheet && (
            <MobileFilterSheet
              columns={filterCols}
              filters={filters}
              onChange={setFilters}
              counts={counts}
              labels={Object.fromEntries(columns.map((c) => [c.key, t(`cols.${c.key}`)]))}
              onClose={() => setSheet(false)}
            />
          )}
        </>
      )}

      {(adding || editing) && (
        <DestinationModal
          existing={editing}
          onClose={() => {
            setAdding(false);
            setEditing(null);
          }}
          onSaved={() => {
            setAdding(false);
            setEditing(null);
            load();
          }}
        />
      )}
      {deleting && (
        <DeleteModal
          row={deleting}
          onClose={() => setDeleting(null)}
          onDone={() => {
            setDeleting(null);
            load();
          }}
        />
      )}
      {testResult && (
        <Modal
          title={t('test.title', { name: testResult.name })}
          onClose={() => setTestResult(null)}
          footer={
            <Button variant="primary" onClick={() => setTestResult(null)}>
              {t('common:actions.close')}
            </Button>
          }
        >
          <p className="modal-confirm-text">{testResult.text}</p>
        </Modal>
      )}
    </div>
  );
}
