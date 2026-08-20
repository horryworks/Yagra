// The shared create/edit dialog for a metric alert rule.
//
// Mounted from two places (ADR-076): Alerts ▸ Metric alert rules, where a rule is created at a
// profile / folder-group / node scope, and Node detail ▸ Interfaces, where the dock creates one
// for the port being looked at. ONE component rather than two, because the alternative is two
// answers to "what does this form send" that drift the first time a field is added — the same
// reason the add and edit paths are one dialog rather than two.
//
// The judgement (what the fields become, whether they may be submitted, which control each scope
// level needs) lives in `pages/thresholdRequest.ts`, because Vitest does not execute a `.tsx`.

import { useEffect, useMemo, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { api, errMsg } from '../../services/api';
import { LIVENESS_METRIC } from '../../lib/format';
import { splitInterfaceScopeId } from '../../lib/interfaceScope';
import {
  isThresholdReady,
  scopeIdKind,
  thresholdBody,
  thresholdFormFrom,
  type ThresholdForm,
} from '../../pages/thresholdRequest';
import {
  CREATABLE_SCOPE_LEVELS,
  DIRECTIONS,
  type Direction,
  type NodeGroup,
  type ProfileSummary,
  type ScopeLevel,
  type StoredThreshold,
} from '../../types/api';
import { MetricPicker } from '../MetricPicker/MetricPicker';
import { NodePicker } from '../NodePicker/NodePicker';
import { Button } from '../ui/Button';
import { Modal } from '../ui/Modal';
import { Select, TextInput } from '../ui/Field';
import { useEntityNames } from '../ui/EntityName';
import { groupOptions } from '../../lib/nodeTree';

/** The scope-id control for the level the operator has chosen.
 *
 *  Before ADR-075 増分 3 this was one free-text box for every level, and the id it wanted — a
 *  device profile's UUID — is printed nowhere in the WebUI, so creating a profile-scoped rule was
 *  not actually possible. A mistyped id is not an error either: the engine compares it and simply
 *  never matches, so the rule is created, listed, and silently evaluates for no node.
 *
 *  Which control belongs to which level is `scopeIdKind`'s answer, not a second `switch` here —
 *  the same answer decides whether Save may be pressed, and two copies would let the dialog show a
 *  picker while readiness still tested a text box. */
function ScopeIdField({
  form,
  onChange,
}: {
  form: ThresholdForm;
  onChange: (scopeId: string) => void;
}) {
  const { t } = useTranslation('alertsConfig');
  const kind = scopeIdKind(form.level);
  const [profiles, setProfiles] = useState<ProfileSummary[]>([]);
  const [groups, setGroups] = useState<NodeGroup[]>([]);
  const { nodeName } = useEntityNames();

  // Both lists are small, bounded config tables — the same two the maintenance-window form loads.
  // Failing quietly leaves an empty picker rather than blocking the dialog: the operator can still
  // switch to a level whose list did load.
  useEffect(() => {
    if (kind === 'profile') api.listProfiles().then(setProfiles).catch(() => setProfiles([]));
    if (kind === 'folderGroup') api.listNodeGroups().then(setGroups).catch(() => setGroups([]));
  }, [kind]);

  const groupItems = useMemo(() => groupOptions(groups), [groups]);

  if (kind === 'none') {
    return (
      <div className="modal-field">
        <span className="modal-hint">{t(`thresholds.addModal.scopeIdNoun.global`)}</span>
      </div>
    );
  }
  return (
    <div className="modal-field">
      <label className="modal-field-label">{t('thresholds.addModal.scopeId')}</label>
      {kind === 'node' ? (
        <NodePicker
          value={form.scopeId || null}
          valueLabel={form.scopeId ? nodeName(form.scopeId) : undefined}
          onChange={(n) => onChange(n?.id ?? '')}
          placeholder={t('thresholds.addModal.scopeIdPlaceholder.node')}
        />
      ) : kind === 'profile' ? (
        <Select value={form.scopeId} onChange={(e) => onChange(e.target.value)}>
          <option value="">{t('thresholds.addModal.scopeIdPlaceholder.profile')}</option>
          {profiles.map((p) => (
            <option key={p.id} value={p.id}>
              {p.name}
            </option>
          ))}
        </Select>
      ) : kind === 'folderGroup' ? (
        <Select value={form.scopeId} onChange={(e) => onChange(e.target.value)}>
          <option value="">{t('thresholds.addModal.scopeIdPlaceholder.group_id')}</option>
          {groupItems.map((g) => (
            <option key={g.id} value={g.id}>
              {g.label}
            </option>
          ))}
        </Select>
      ) : kind === 'interface' ? (
        // One port of one node (ADR-076). Shown, not edited: this screen has no port picker —
        // a fleet-wide one does not exist — so a rule at this level is created from Node detail ▸
        // Interfaces, where the port being looked at *is* the target. It still reaches this dialog
        // when an existing rule is edited, and its bounds and breach count are editable there.
        <>
          <div className="thresholds-fixed mono">
            {(() => {
              const [node, port] = splitInterfaceScopeId(form.scopeId);
              return port === null ? form.scopeId : `${nodeName(node)} · #${port}`;
            })()}
          </div>
          <input type="hidden" value={form.scopeId} readOnly />
        </>
      ) : (
        // The legacy tag scope. Free text because a tag value *is* free text, and no list of the
        // ones in use exists — nothing in the product writes `nodes.tags` but a bundle import.
        <TextInput
          className="mono"
          placeholder={t('thresholds.addModal.scopeIdPlaceholder.group')}
          value={form.scopeId}
          onChange={(e) => onChange(e.target.value)}
        />
      )}
      <span className="modal-hint">
        {kind === 'folderGroup'
          ? t('thresholds.addModal.folderGroupHint')
          : kind === 'tag'
            ? t('thresholds.addModal.legacyTagHint')
            : kind === 'interface'
              ? t('thresholds.addModal.interfaceHint')
              : t('thresholds.addModal.scopeIdHint', {
                  noun: t(`thresholds.addModal.scopeIdNoun.${form.level}`),
                })}
      </span>
    </div>
  );
}

/** Create or edit a threshold rule (focused-editing modal).
 *
 *  One dialog for both, the shape `EventRulesPage`'s `RuleModal` uses. Two would be two answers to
 *  "what does this form send", and the add path and the edit path would drift. The judgement —
 *  what the fields become, and whether they may be submitted — lives in `thresholdRequest.ts`,
 *  because Vitest does not execute a `.tsx`.
 *
 *  ⚠️ The form state is held here, inside a conditionally-mounted component, so closing the dialog
 *  *is* the reset (ui-conventions "Modals"). A `resetForm()` enumerating the fields would be a
 *  second copy of the field list. */
export function ThresholdModal({
  mode,
  rule,
  onClose,
  onSaved,
}: {
  mode: 'add' | 'edit';
  rule?: StoredThreshold;
  onClose: () => void;
  onSaved: () => void;
}) {
  const { t } = useTranslation('alertsConfig');
  // ⚠️ Held here, inside a conditionally-mounted component, so closing the dialog *is* the reset
  // (ui-conventions "Modals"). It briefly took a `prefill` for the Interfaces dock; that caller
  // now opens its own port-shaped dialog (ADR-076 増分 5), and a prop with no caller is a prop
  // nothing keeps true.
  const [form, setForm] = useState<ThresholdForm>(() => thresholdFormFrom(rule));
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const set = <K extends keyof ThresholdForm>(key: K, value: ThresholdForm[K]) =>
    setForm((f) => ({ ...f, [key]: value }));

  const ready = isThresholdReady(form);

  // The legacy tag-based `group` level is not offered for a *new* rule — nothing in the product
  // writes `nodes.tags`, so a rule created at it cannot match anything (ADR-075 増分 3, the same
  // move the maintenance-window form already made). ⚠️ It must still appear while editing a rule
  // that already sits at it: a `<select>` whose value is absent from its options renders blank,
  // and the next save would silently move the rule to whichever level rendered first.
  const levels = useMemo(
    () =>
      CREATABLE_SCOPE_LEVELS.includes(form.level)
        ? CREATABLE_SCOPE_LEVELS
        : [...CREATABLE_SCOPE_LEVELS, form.level],
    [form.level],
  );

  // Two derivations of "this is the reachability rule", and they are deliberately different.
  //
  //  - `lockedMetric` reads the **stored** rule, so editing that row never shows or lets anyone
  //    retype the engine's internal sentinel. Deriving it from the typed value instead would make
  //    the input disappear the moment someone typed `__liveness__` into it, with no way back.
  //  - `noBounds` reads the **current** value, so the bounds also disappear in add mode — the
  //    sentinel is offered in the metric picker, and bounds on it are read by nothing
  //    (`repo.rs`'s seed comment): the engine takes the severity from the committed `NodeState`.
  const lockedMetric = mode === 'edit' && rule?.metric === LIVENESS_METRIC;
  const noBounds = form.metric.trim() === LIVENESS_METRIC;

  const submit = () => {
    if (!ready) return;
    setBusy(true);
    setError(null);
    const body = thresholdBody(form);
    const call =
      mode === 'edit' && rule
        ? api.updateThreshold(rule.id, body)
        : api.createThreshold(body).then(() => undefined);
    call
      .then(() => {
        onSaved();
        onClose();
      })
      .catch((e: unknown) => {
        setError(errMsg(e, t('thresholds.err.save')));
        setBusy(false);
      });
  };

  return (
    <Modal
      title={mode === 'edit' ? t('thresholds.editModal.title') : t('thresholds.addModal.title')}
      onClose={onClose}
      footer={
        <>
          <Button variant="outline" onClick={onClose} disabled={busy}>
            {t('common:actions.cancel')}
          </Button>
          <Button variant="primary" onClick={submit} disabled={!ready || busy}>
            {mode === 'edit' ? t('common:actions.save') : t('thresholds.addModal.add')}
          </Button>
        </>
      }
    >
      <div className="modal-field">
        <label className="modal-field-label">{t('thresholds.addModal.scopeLevel')}</label>
        <Select
          value={form.level}
          onChange={(e) =>
            // Clear the id with the level. Ids are not interchangeable across levels — a profile
            // UUID left behind on a folder-group rule is a rule that matches nothing — and every
            // control below is now a picker, so there is nothing an operator would want carried.
            setForm((f) => ({ ...f, level: e.target.value as ScopeLevel, scopeId: '' }))
          }
        >
          {levels.map((l) => (
            <option key={l} value={l}>
              {t(`thresholds.scopeLevel.${l}`)}
            </option>
          ))}
        </Select>
      </div>
      <ScopeIdField form={form} onChange={(scopeId) => set('scopeId', scopeId)} />
      <div className="modal-field">
        <label className="modal-field-label">{t('thresholds.addModal.metric')}</label>
        {lockedMetric ? (
          <>
            <p className="thresholds-fixed">{t('format:liveness')}</p>
            <span className="modal-hint">{t('thresholds.livenessMetric')}</span>
          </>
        ) : (
          <MetricPicker
            value={form.metric}
            onChange={(m) => set('metric', m)}
            // A rule scoped to one port can only be about a metric that has a value per port. The
            // engine passes a port number only for those, and an interface rule with none matches
            // nothing — so the other 81 catalogue entries were offers of an inert rule.
            onlyPerInterface={form.level === 'interface'}
          />
        )}
      </div>
      <div className="modal-field">
        <label className="modal-field-label">{t('thresholds.addModal.direction')}</label>
        <Select
          value={form.direction}
          onChange={(e) => set('direction', e.target.value as Direction)}
        >
          {DIRECTIONS.map((d) => (
            <option key={d} value={d}>
              {t(`thresholds.direction.${d}`)}
            </option>
          ))}
        </Select>
      </div>
      <div className="modal-field">
        <label className="modal-field-label">
          {noBounds ? t('thresholds.addModal.dwellOnly') : t('thresholds.addModal.boundsDwell')}
        </label>
        <div className="thresholds-bounds">
          {!noBounds && (
            <>
              <TextInput
                className="thresholds-num"
                placeholder={t('thresholds.addModal.warnPlaceholder')}
                value={form.warning}
                onChange={(e) => set('warning', e.target.value)}
              />
              <TextInput
                className="thresholds-num"
                placeholder={t('thresholds.addModal.critPlaceholder')}
                value={form.critical}
                onChange={(e) => set('critical', e.target.value)}
              />
            </>
          )}
          <TextInput
            className="thresholds-num"
            placeholder={t('thresholds.addModal.dwellPlaceholder')}
            value={form.dwell}
            onChange={(e) => set('dwell', e.target.value)}
            title={t('thresholds.addModal.dwellTitle')}
          />
        </div>
        <span className="modal-hint">
          {noBounds ? t('thresholds.livenessMetric') : t('thresholds.addModal.boundsHint')}
        </span>
      </div>
      {error && <p className="form-error">{error}</p>}
    </Modal>
  );
}
