// SPDX-License-Identifier: AGPL-3.0-only
// The port's alert rules: what governs this interface, and the form to add or change one
// (ADR-076 増分 5, 決定 8 and 決定 11).
//
// Two views in one dialog. The list answers "what is watching this port", and it lists **every**
// rule that reaches it — a port is governed from six scope levels and the interesting rule is
// usually not its own, so a list of only the port's rules would show nothing about a port that is
// alerting. The form asks what to watch and how to express the bound; it never asks for a metric
// name (see `lib/portRuleForm.ts` for why that question is wrong here).
//
// Every judgement lives in `lib/portRuleForm.ts`, because Vitest does not execute a `.tsx`
// (`testing.md`). This file owns the two layouts, the fetch, and nothing else.

import { useCallback, useEffect, useMemo, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { api, errMsg } from '../../services/api';
import { useCan } from '../../store';
import { formatBps } from '../../lib/format';
import {
  PORT_RULE_BASES,
  PORT_RULE_SUBJECTS,
  PORT_SUBJECT_SPECS,
  RATE_UNITS,
  bpsOfPercent,
  isPortRuleReady,
  needsPortSpeed,
  newPortRuleForm,
  portRuleFrom,
  portRuleToThreshold,
  type PortRuleBasis,
  type PortRuleForm,
  type PortRuleSubject,
  type RateUnit,
} from '../../lib/portRuleForm';
import type { MatchingThreshold, StoredThreshold } from '../../types/api';
import { boundSentence, isOwnRule } from './interfaceRuleText';
import { Modal } from '../ui/Modal';
import { Button } from '../ui/Button';
import { Select, TextInput } from '../ui/Field';
import { IconButton } from '../ui/IconButton';
import { EditIcon, TrashIcon } from '../ui/icons';
import { ConfirmDeleteModal } from '../ui/ConfirmDeleteModal';
import './InterfaceRulesModal.css';

interface Props {
  nodeId: string;
  ifindex: number;
  /** The port as the operator sees it in the dock header (`GE0/0/1`, or `if7`). */
  portLabel: string;
  /** The device's own declared speed, in bits/sec, or null when it reports none. */
  speedBps: number | null;
  onClose: () => void;
}

export function InterfaceRulesModal({ nodeId, ifindex, portLabel, speedBps, onClose }: Props) {
  const { t } = useTranslation('nodes');
  const { t: ta } = useTranslation('alertsConfig');
  const canConfig = useCan('manage_config');
  const [rows, setRows] = useState<MatchingThreshold[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  /** `null` = the list; otherwise the form, editing `rule` or adding when it is undefined. */
  const [editing, setEditing] = useState<{ rule?: StoredThreshold } | null>(null);
  const [deleting, setDeleting] = useState<StoredThreshold | null>(null);

  const load = useCallback(() => {
    setError(null);
    api
      .listInterfaceThresholds(nodeId, ifindex)
      .then(setRows)
      .catch((e: unknown) => {
        setRows([]);
        setError(errMsg(e, t('interfaces.rules.loadFailed')));
      });
  }, [nodeId, ifindex, t]);

  useEffect(load, [load]);

  const own = useMemo(
    () => (rows ?? []).filter((r) => isOwnRule(r, nodeId, ifindex)),
    [rows, nodeId, ifindex],
  );
  const inherited = useMemo(
    () => (rows ?? []).filter((r) => !isOwnRule(r, nodeId, ifindex)),
    [rows, nodeId, ifindex],
  );

  if (deleting) {
    return (
      <ConfirmDeleteModal
        title={t('interfaces.rules.deleteTitle')}
        onConfirm={() => api.deleteThreshold(deleting.id)}
        errorFallback={t('interfaces.rules.deleteFailed')}
        onClose={() => setDeleting(null)}
        onDone={() => {
          setDeleting(null);
          load();
        }}
      >
        {t('interfaces.rules.deleteBody', { port: portLabel })}
      </ConfirmDeleteModal>
    );
  }

  if (editing) {
    return (
      <PortRuleFormView
        nodeId={nodeId}
        ifindex={ifindex}
        portLabel={portLabel}
        speedBps={speedBps}
        rule={editing.rule}
        onCancel={() => setEditing(null)}
        onSaved={() => {
          setEditing(null);
          load();
        }}
      />
    );
  }

  return (
    <Modal
      title={t('interfaces.rules.title', { port: portLabel })}
      onClose={onClose}
      footer={
        <>
          {canConfig && (
            <Button variant="outline" onClick={() => setEditing({})}>
              {t('interfaces.rules.add')}
            </Button>
          )}
          <Button variant="primary" onClick={onClose}>
            {t('common:actions.close')}
          </Button>
        </>
      }
    >
      <p className="ifrules-sub">
        {speedBps != null && speedBps > 0
          ? t('interfaces.rules.subtitle', { speed: formatBps(speedBps) })
          : t('interfaces.rules.subtitleNoSpeed')}
      </p>

      {rows === null ? (
        <p className="modal-hint">{t('common:status.loading')}</p>
      ) : rows.length === 0 ? (
        <p className="ifrules-empty">{t('interfaces.rules.empty')}</p>
      ) : (
        <>
          {/* The sections carry `data-kind` so a test can address them structurally: the
              inherited section's own hint sentence contains the words "this port" too, so a text
              filter matches both and the assertions inspect the wrong rows while staying green.
              (It did, on the first run of `interfaceRules.spec.ts`.) */}
          {own.length > 0 && (
            <section className="ifrules-section" data-kind="own">
              <h4 className="ifrules-head">{t('interfaces.rules.thisPort')}</h4>
              {own.map((row) => (
                <RuleRow
                  key={row.rule.id}
                  row={row}
                  speedBps={speedBps}
                  editable={canConfig}
                  onEdit={() => setEditing({ rule: row.rule })}
                  onDelete={() => setDeleting(row.rule)}
                />
              ))}
            </section>
          )}
          {inherited.length > 0 && (
            <section className="ifrules-section" data-kind="inherited">
              <h4 className="ifrules-head">{t('interfaces.rules.inherited')}</h4>
              {inherited.map((row) => (
                <RuleRow key={row.rule.id} row={row} speedBps={speedBps} editable={false} />
              ))}
              {/* Not a footnote: without it the read-only rows read as broken rather than as
                  deliberately edited elsewhere (ADR-055 R6). */}
              <p className="modal-hint">
                {t('interfaces.rules.inheritedHint', {
                  screen: ta('thresholds.title', { defaultValue: 'Metric alert rules' }),
                })}
              </p>
            </section>
          )}
        </>
      )}
      {error && <p className="form-error">{error}</p>}
    </Modal>
  );
}

/** One rule, in the words of the port dialog where it can be — by its metric name where it cannot. */
function RuleRow({
  row,
  speedBps,
  editable,
  onEdit,
  onDelete,
}: {
  row: MatchingThreshold;
  speedBps: number | null;
  editable: boolean;
  onEdit?: () => void;
  onDelete?: () => void;
}) {
  const { t } = useTranslation('nodes');
  const { t: ta } = useTranslation('alertsConfig');
  const form = portRuleFrom(row.rule);

  return (
    <div className={`ifrules-row${row.in_force ? '' : ' overridden'}`}>
      <span
        className="ifrules-dot"
        title={
          row.in_force ? t('interfaces.rules.inForce') : t('interfaces.rules.overridden')
        }
        aria-label={
          row.in_force ? t('interfaces.rules.inForce') : t('interfaces.rules.overridden')
        }
      />
      <div className="ifrules-what">
        <span className="ifrules-subject">
          {form ? (
            t(`interfaces.rules.subjects.${form.subject}`)
          ) : (
            // No subject says this rule, so it is shown as what it is. Rounding it into the
            // nearest subject would retarget it the next time anyone pressed Save.
            <span className="mono">{row.rule.metric}</span>
          )}
        </span>
        <span className="ifrules-bound">{boundSentence(row.rule, form, speedBps, t)}</span>
      </div>
      <span className="ifrules-scope">{ta(`thresholds.scopeLevel.${row.rule.scope_level}`)}</span>
      {editable && onEdit && onDelete ? (
        <span className="ifrules-actions">
          <IconButton title={t('common:actions.edit')} onClick={onEdit}>
            <EditIcon />
          </IconButton>
          <IconButton title={t('common:actions.delete')} onClick={onDelete} danger>
            <TrashIcon />
          </IconButton>
        </span>
      ) : (
        <span className="ifrules-actions" />
      )}
    </div>
  );
}

/** The bound, said the way the subject means it. */

/** Add or edit — the port-shaped form. */
function PortRuleFormView({
  nodeId,
  ifindex,
  portLabel,
  speedBps,
  rule,
  onCancel,
  onSaved,
}: {
  nodeId: string;
  ifindex: number;
  portLabel: string;
  speedBps: number | null;
  rule?: StoredThreshold;
  onCancel: () => void;
  onSaved: () => void;
}) {
  const { t } = useTranslation('nodes');
  const [form, setForm] = useState<PortRuleForm>(
    () => (rule && portRuleFrom(rule)) || newPortRuleForm(),
  );
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const set = <K extends keyof PortRuleForm>(key: K, value: PortRuleForm[K]) =>
    setForm((f) => ({ ...f, [key]: value }));

  const spec = PORT_SUBJECT_SPECS[form.subject];
  const ready = isPortRuleReady(form);
  const speedMissing = needsPortSpeed(form) && !(speedBps != null && speedBps > 0);
  const body = portRuleToThreshold(form, nodeId, ifindex);

  const submit = () => {
    if (!ready) return;
    setBusy(true);
    setError(null);
    const call = rule
      ? api.updateThreshold(rule.id, body)
      : api.createThreshold(body).then(() => undefined);
    call.then(onSaved).catch((e: unknown) => {
      setError(errMsg(e, t('interfaces.rules.saveFailed')));
      setBusy(false);
    });
  };

  const unitLabel = spec.hasBasis && form.basis === 'absolute' ? null : (spec.unit ?? '');

  return (
    <Modal
      title={t(rule ? 'interfaces.rules.editTitle' : 'interfaces.rules.addTitle', {
        port: portLabel,
      })}
      onClose={onCancel}
      footer={
        <>
          <Button variant="outline" onClick={onCancel} disabled={busy}>
            {t('common:actions.cancel')}
          </Button>
          <Button variant="primary" onClick={submit} disabled={!ready || busy}>
            {rule ? t('common:actions.save') : t('interfaces.rules.add')}
          </Button>
        </>
      }
    >
      <div className="modal-field">
        <label className="modal-field-label">{t('interfaces.rules.subject')}</label>
        <Select
          value={form.subject}
          onChange={(e) => setForm(newPortRuleForm(e.target.value as PortRuleSubject))}
        >
          {PORT_RULE_SUBJECTS.map((s) => (
            <option key={s} value={s}>
              {t(`interfaces.rules.subjects.${s}`)}
            </option>
          ))}
        </Select>
      </div>

      {spec.hasBasis && (
        <div className="modal-field">
          <label className="modal-field-label">{t('interfaces.rules.basis')}</label>
          <div className="ifrules-basis">
            {PORT_RULE_BASES.map((b) => (
              <label key={b} className="ifrules-radio">
                <input
                  type="radio"
                  name="port-rule-basis"
                  checked={form.basis === b}
                  onChange={() => set('basis', b as PortRuleBasis)}
                />
                <span>{t(`interfaces.rules.basis_${b}`)}</span>
              </label>
            ))}
          </div>
          {speedMissing && <span className="modal-hint warn">{t('interfaces.rules.noSpeed')}</span>}
        </div>
      )}

      {spec.fixedBounds ? (
        <div className="modal-field">
          <label className="modal-field-label">{t('interfaces.rules.when')}</label>
          <p className="ifrules-fixed">{t('interfaces.rules.linkNotUp')}</p>
          <span className="modal-hint">{t('interfaces.rules.linkStateHint')}</span>
        </div>
      ) : (
        <div className="modal-field">
          <label className="modal-field-label">
            {t(spec.direction === 'above' ? 'interfaces.rules.above' : 'interfaces.rules.below')}
          </label>
          <div className="ifrules-bounds">
            <TextInput
              className="ifrules-num"
              placeholder={t('interfaces.rules.warnPlaceholder')}
              value={form.warning}
              onChange={(e) => set('warning', e.target.value)}
            />
            <TextInput
              className="ifrules-num"
              placeholder={t('interfaces.rules.critPlaceholder')}
              value={form.critical}
              onChange={(e) => set('critical', e.target.value)}
            />
            {unitLabel === null ? (
              <Select value={form.unit} onChange={(e) => set('unit', e.target.value as RateUnit)}>
                {RATE_UNITS.map((u) => (
                  <option key={u} value={u}>
                    {u}
                  </option>
                ))}
              </Select>
            ) : (
              <span className="ifrules-unit">{unitLabel}</span>
            )}
          </div>
          {/* What the percentage actually means on this port. Without it, "90%" is a number the
              operator has to do arithmetic on against a speed shown on another surface. */}
          {unitLabel === '%' && (
            <span className="modal-hint">
              {(() => {
                const crit = Number(form.critical);
                const bps = Number.isFinite(crit) ? bpsOfPercent(crit, speedBps) : null;
                return bps == null
                  ? t('interfaces.rules.percentOfUnknown')
                  : t('interfaces.rules.percentOf', {
                      pct: crit,
                      speed: formatBps(speedBps),
                      value: formatBps(bps),
                    });
              })()}
            </span>
          )}
        </div>
      )}

      <div className="modal-field">
        <label className="modal-field-label">{t('interfaces.rules.dwell')}</label>
        <div className="ifrules-bounds">
          <TextInput
            className="ifrules-num"
            value={form.dwell}
            onChange={(e) => set('dwell', e.target.value)}
          />
          <span className="ifrules-unit">
            {/* Deliberately different wording per subject: the derived metrics are evaluated once
                a minute by a leader loop, the polled ones once per poll. Calling both "samples"
                is what made the delay unguessable. */}
            {t(
              spec.cadence === 'minutes'
                ? 'interfaces.rules.dwellMinutes'
                : 'interfaces.rules.dwellPolls',
            )}
          </span>
        </div>
      </div>

      {/* The rule this becomes, in the vocabulary the rules screen and the API use. Small, but it
          is the only thing tying this form to what an operator will later see listed there. */}
      <p className="ifrules-stored mono">
        {t('interfaces.rules.storedAs', {
          metric: body.metric,
          direction: body.direction,
          value: body.critical ?? body.warning ?? '—',
        })}
      </p>
      {error && <p className="form-error">{error}</p>}
    </Modal>
  );
}
