// SPDX-License-Identifier: AGPL-3.0-only
// The port's thresholds: what watches this interface, and the form to set or change it
// (ADR-076 Inc.5, decisions 8 and 11; redrawn by ADR-230 decision 7).
//
// Two views in one dialog. The list is one row per thing a port rule can watch — the value in
// force, where it was decided (this port, every node, a profile, a folder, the node), and the one
// action that fits. A rule on a node-wide metric is not listed: it cannot be changed for one port,
// and the node's rules are on Alerts ▸ Threshold rules. The form asks what to watch and how to
// express the bound; it never asks for a metric name (see `lib/portRuleForm.ts` for why).
//
// Every judgement lives in `lib/portRuleForm.ts` and `portSubjectRows.ts`, because Vitest does not
// execute a `.tsx` (`testing.md`). This file owns the two layouts, the fetch, and nothing else.

import { useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { Link } from 'react-router-dom';
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
  storedBound,
  type PortRuleBasis,
  type PortRuleForm,
  type PortRuleSubject,
  type RateUnit,
} from '../../lib/portRuleForm';
import type { MatchingThreshold, StoredThreshold } from '../../types/api';
import { boundSentence, isOwnRule } from './interfaceRuleText';
import { portSubjectRows, type PortSubjectRow } from './portSubjectRows';
import { thresholdRulesHref } from '../../pages/thresholdQuery';
import { useEntityNames } from '../ui/entityNames';
import { done } from '../../lib/submitState';
import { useSubmit } from '../../lib/useSubmit';
import { Modal } from '../ui/Modal';
import { Button } from '../ui/Button';
import { FormError, FormFooter } from '../ui/FormFooter';
import { Field, Select, TextInput } from '../ui/Field';
import { FieldGroup, Fields } from '../ui/FormLayout';
import { Callout } from '../ui/Section';
import { ConfirmDeleteModal } from '../ui/ConfirmDeleteModal';
import { InfoTip } from '../ui/InfoTip';
import './InterfaceRulesModal.css';

interface Props {
  nodeId: string;
  ifindex: number;
  /** The port as the operator sees it in the dock header (`GE0/0/1`, or `if7`). */
  portLabel: string;
  /** The device's own declared speed, in bits/sec, or null when it reports none. */
  speedBps: number | null;
  /** Whether the port carries a transceiver (`portSubjectRows.ts::isOpticalPort`). Left out it is
   *  `true`: a caller that cannot tell (the threshold-rule dialog's port picker) shows all five. */
  optical?: boolean;
  /** A rule of this port was saved or deleted. The dialog itself re-reads its list either way. */
  onChanged?: () => void;
  onClose: () => void;
}

/** What the form is for: an existing port rule, or a new one starting from `start`. */
type Editing = { rule: StoredThreshold; start?: undefined } | { rule?: undefined; start: PortRuleForm };

export function InterfaceRulesModal({
  nodeId,
  ifindex,
  portLabel,
  speedBps,
  optical = true,
  onChanged,
  onClose,
}: Props) {
  const { t } = useTranslation('nodes');
  const { t: ta } = useTranslation('alertsConfig');
  const canConfig = useCan('manage_config');
  const { scopeName } = useEntityNames();
  const [rows, setRows] = useState<MatchingThreshold[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  /** `null` = the list; otherwise the form. */
  const [editing, setEditing] = useState<Editing | null>(null);
  const [deleting, setDeleting] = useState<StoredThreshold | null>(null);
  /** Where the list was scrolled when it gave way to the confirm step or the form. The dialog swaps
   *  its body for either, so coming back mounts a fresh list at the top; this puts it back (ADR-203).
   *  Not two dialogs stacked: Escape is heard by every mounted dialog, and the list's would close
   *  the whole thing under the confirm step. */
  const listEl = useRef<HTMLDivElement>(null);
  const parkedAt = useRef<number | null>(null);
  const scroller = () => listEl.current?.closest<HTMLElement>('.modal-body') ?? null;
  const leaveList = () => {
    parkedAt.current = scroller()?.scrollTop ?? null;
  };
  const showingList = !deleting && !editing;
  useLayoutEffect(() => {
    const at = parkedAt.current;
    const el = scroller();
    if (!showingList || at === null || !el) return;
    parkedAt.current = null;
    el.scrollTop = at;
  }, [showingList]);

  const load = useCallback(() => {
    setError(null);
    api
      .listInterfaceThresholds(nodeId, ifindex)
      .then(setRows)
      .catch((e: unknown) => {
        // A failed re-read keeps the rules it had, and says so; only a first read goes empty.
        setRows((prev) => prev ?? []);
        setError(errMsg(e, t('interfaces.rules.loadFailed')));
      });
  }, [nodeId, ifindex, t]);

  useEffect(load, [load]);

  const table = useMemo(
    () => (rows === null ? null : portSubjectRows(rows, nodeId, ifindex, optical)),
    [rows, nodeId, ifindex, optical],
  );

  /** Where one rule came from: this port, or its level and what it names. */
  const source = (m: MatchingThreshold) => {
    if (isOwnRule(m, nodeId, ifindex)) return t('interfaces.rules.thisPort');
    const level = ta(`thresholds.tabs.${m.rule.scope_level}`);
    const names = m.rule.scope_ids.map((id) => scopeName(m.rule.scope_level, id)).join(', ');
    return names ? `${level}: ${names}` : level;
  };

  if (deleting) {
    return (
      <ConfirmDeleteModal
        title={t('interfaces.rules.deleteTitle')}
        confirmLabel={t('interfaces.rules.revert')}
        onConfirm={() => api.deleteThreshold(deleting.id)}
        errorFallback={t('interfaces.rules.deleteFailed')}
        onClose={() => setDeleting(null)}
        onDone={() => {
          setDeleting(null);
          load();
          onChanged?.();
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
        start={editing.start}
        onCancel={() => setEditing(null)}
        onSaved={() => {
          setEditing(null);
          load();
          onChanged?.();
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
          {/* The node-wide rules (CPU, reachability) are not this port's, and are read where the
              node's rules are (ADR-230 decision 7). */}
          <Link className="ifrules-nodewide" to={thresholdRulesHref({ check: nodeId })}>
            {t('interfaces.rules.nodeWide')}
          </Link>
          <Button variant="primary" onClick={onClose}>
            {t('common:actions.close')}
          </Button>
        </>
      }
    >
      {/* Only the state that limits what can be set. With a speed, each percentage is spelled
          out against it in the form itself (`percentOf`), which is where the number is typed. */}
      {!(speedBps != null && speedBps > 0) && (
        <p className="ifrules-sub">{t('interfaces.rules.subtitleNoSpeed')}</p>
      )}

      <div ref={listEl}>
        {table === null ? (
          <p className="form-status">{t('common:loading')}</p>
        ) : (
          <>
            <div className="ifrules-grid ifrules-headrow">
              <span>{t('interfaces.rules.colWatch')}</span>
              <span>{t('interfaces.rules.colInForce')}</span>
              <span>{t('interfaces.rules.colFrom')}</span>
              <span />
            </div>
            {table.subjects.map((r) => (
              <div className="ifrules-grid ifrules-row" key={r.subject} data-subject={r.subject}>
                <span className="ifrules-subject">{t(`interfaces.rules.subjects.${r.subject}`)}</span>
                <span className="ifrules-bound">
                  {r.inForce.length === 0 ? (
                    <span className="muted">{t('interfaces.rules.notWatched')}</span>
                  ) : (
                    r.inForce.map((m) => (
                      <span key={m.rule.id}>
                        {boundSentence(m.rule, portRuleFrom(m.rule), speedBps, t)}
                      </span>
                    ))
                  )}
                </span>
                <span className="ifrules-scope">
                  {r.inForce.map((m) => (
                    <span key={m.rule.id} className={isOwnRule(m, nodeId, ifindex) ? 'own' : 'muted'}>
                      {source(m)}
                    </span>
                  ))}
                </span>
                <span className="ifrules-actions">
                  {canConfig && (
                    <RowActions
                      row={r}
                      onEdit={(rule) => {
                        leaveList();
                        setEditing({ rule });
                      }}
                      onStart={(start) => {
                        leaveList();
                        setEditing({ start });
                      }}
                      onRevert={(rule) => {
                        leaveList();
                        setDeleting(rule);
                      }}
                      elsewhere={thresholdRulesHref({ tab: 'interface' })}
                    />
                  )}
                </span>
              </div>
            ))}
            {/* Rules on this port's other per-interface metrics (errors, discards): the port form
                has no subject for them, and a rule that fires here must still be findable here. */}
            {table.other.length > 0 && (
              <section className="ifrules-section" data-kind="other">
                <h4 className="ifrules-head">{t('interfaces.rules.other')}</h4>
                {table.other.map((m) => (
                  <div className="ifrules-grid ifrules-row" key={m.rule.id}>
                    <span className="mono ifrules-subject">{m.rule.metric}</span>
                    <span className="ifrules-bound">{boundSentence(m.rule, null, speedBps, t)}</span>
                    <span className="ifrules-scope muted">{source(m)}</span>
                    <span />
                  </div>
                ))}
              </section>
            )}
          </>
        )}
      </div>
      <FormError form={{ error }} />
    </Modal>
  );
}

/** The one action a row offers, by what is in force there. */
function RowActions({
  row,
  onEdit,
  onStart,
  onRevert,
  elsewhere,
}: {
  row: PortSubjectRow;
  onEdit: (rule: StoredThreshold) => void;
  onStart: (start: PortRuleForm) => void;
  onRevert: (rule: StoredThreshold) => void;
  elsewhere: string;
}) {
  const { t } = useTranslation('nodes');
  const mine = row.own[0]?.rule;
  switch (row.action.kind) {
    case 'edit':
      return (
        <>
          {mine && (
            <Button variant="outline" onClick={() => onEdit(mine)}>
              {t('common:actions.edit')}
            </Button>
          )}
          {mine && (
            <Button variant="outline" onClick={() => onRevert(mine)}>
              {t('interfaces.rules.revert')}
            </Button>
          )}
        </>
      );
    case 'override': {
      const start = row.action.form;
      return (
        <Button variant="outline" onClick={() => onStart(start)}>
          {t('interfaces.rules.override')}
        </Button>
      );
    }
    case 'set': {
      const start = row.action.form;
      return (
        <Button variant="outline" onClick={() => onStart(start)}>
          {t('interfaces.rules.set')}
        </Button>
      );
    }
    case 'elsewhere':
      return <Link to={elsewhere}>{t('interfaces.rules.editElsewhere')}</Link>;
  }
}

/** The bound, said the way the subject means it. */

/** Add or edit — the port-shaped form. */
function PortRuleFormView({
  nodeId,
  ifindex,
  portLabel,
  speedBps,
  rule,
  start,
  onCancel,
  onSaved,
}: {
  nodeId: string;
  ifindex: number;
  portLabel: string;
  speedBps: number | null;
  rule?: StoredThreshold;
  /** A new rule's opening values — an inherited rule's bounds for an override (ADR-230). */
  start?: PortRuleForm;
  onCancel: () => void;
  onSaved: () => void;
}) {
  const { t } = useTranslation('nodes');
  const [form, setForm] = useState<PortRuleForm>(
    () => start ?? ((rule && portRuleFrom(rule)) || newPortRuleForm()),
  );
  const save = useSubmit({ errorFallback: t('interfaces.rules.saveFailed'), onDone: onSaved });
  const set = <K extends keyof PortRuleForm>(key: K, value: PortRuleForm[K]) =>
    setForm((f) => ({ ...f, [key]: value }));

  const spec = PORT_SUBJECT_SPECS[form.subject];
  const ready = isPortRuleReady(form);
  const speedMissing = needsPortSpeed(form) && !(speedBps != null && speedBps > 0);
  const body = portRuleToThreshold(form, nodeId, ifindex);

  const submit = () => {
    if (!ready) return;
    save.submit(() => {
      const call = rule ? api.updateThreshold(rule.id, body) : api.createThreshold(body);
      return call.then(() => done());
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
        <FormFooter
          form={save}
          onClose={onCancel}
          onSubmit={submit}
          submitLabel={rule ? t('common:actions.save') : t('interfaces.rules.add')}
          canSubmit={ready}
        />
      }
    >
      <Fields>
        <Field label={t('interfaces.rules.subject')}>
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
        </Field>

        {spec.hasBasis && (
          <FieldGroup label={t('interfaces.rules.basis')}>
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
            {speedMissing && <Callout tone="warning">{t('interfaces.rules.noSpeed')}</Callout>}
          </FieldGroup>
        )}

        {spec.fixedBounds ? (
          <Field label={t('interfaces.rules.when')}>
            <p className="ifrules-fixed">
              {t('interfaces.rules.linkNotUp')}{' '}
              <InfoTip
                infoKey="nodes:interfaces.rules.linkState.info"
                label={t('interfaces.rules.linkNotUp')}
              />
            </p>
          </Field>
        ) : (
          <FieldGroup
            label={t(spec.direction === 'above' ? 'interfaces.rules.above' : 'interfaces.rules.below')}
          >
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
                operator has to do arithmetic on against a speed shown on another surface. A port
                that reports no speed has nothing to convert, and the warning above says so. */}
            {unitLabel === '%' &&
              (() => {
                const crit = Number(form.critical);
                const bps = Number.isFinite(crit) ? bpsOfPercent(crit, speedBps) : null;
                return bps == null ? null : (
                  <p className="form-status">
                    {t('interfaces.rules.percentOf', {
                      pct: crit,
                      speed: formatBps(speedBps),
                      value: formatBps(bps),
                    })}
                  </p>
                );
              })()}
          </FieldGroup>
        )}

        <Field label={t('interfaces.rules.dwell')} htmlFor="port-rule-dwell">
          <div className="ifrules-bounds">
            <TextInput
              id="port-rule-dwell"
              className="ifrules-num"
              value={form.dwell}
              onChange={(e) => set('dwell', e.target.value)}
            />
            <span className="ifrules-unit">
              {/* Deliberately different wording per subject: the derived metrics are evaluated
                  once a minute by a leader loop, the polled ones once per poll. Calling both
                  "samples" is what made the delay unguessable. On a node polled less often than
                  once a minute the minutes become polls (ADR-144), which the minutes wording
                  says. */}
              {t(
                spec.cadence === 'minutes'
                  ? 'interfaces.rules.dwellMinutes'
                  : 'interfaces.rules.dwellPolls',
              )}
            </span>
          </div>
        </Field>

        {/* The rule this becomes, in the vocabulary the rules screen and the API use. Small, but it
            is the only thing tying this form to what an operator will later see listed there. */}
        <p className="ifrules-stored mono">
          {t('interfaces.rules.storedAs', {
            metric: body.metric,
            direction: body.direction,
            value: storedBound(body) ?? '—',
          })}
        </p>
        <FormError form={save} />
      </Fields>
    </Modal>
  );
}
