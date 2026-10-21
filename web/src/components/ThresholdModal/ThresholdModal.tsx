// SPDX-License-Identifier: AGPL-3.0-only
// The shared create/edit dialog for a metric alert rule.
//
// Mounted from Alerts ▸ Threshold rules, where a rule is created at a profile / folder-group /
// node scope or edited at any. A NEW port rule is not filled in here: the dialog picks a node and
// a port and hands them to the port-rule dialog Node detail ▸ Interfaces opens (ADR-209), so
// there is one port-rule form. ONE dialog for adding and editing, because two would be two
// answers to "what does this form send" that drift the first time a field is added.
//
// The judgement (what the fields become, whether they may be submitted, which control each scope
// level needs) lives in `pages/thresholdRequest.ts`, because Vitest does not execute a `.tsx`.

import { useEffect, useMemo, useRef, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { api } from '../../services/api';
import { done } from '../../lib/submitState';
import { useSubmit } from '../../lib/useSubmit';
import { LIVENESS_METRIC } from '../../lib/format';
import { interfaceScopeLabel } from '../../lib/interfaceScope';
import {
  DEFAULT_DWELL,
  isThresholdReady,
  scopeAcceptsRowMatch,
  scopeIdKind,
  thresholdBody,
  thresholdFormFrom,
  type ThresholdForm,
} from '../../pages/thresholdRequest';
import {
  CREATABLE_SCOPE_LEVELS,
  type NodeGroup,
  type ProfileSummary,
  type ScopeLevel,
  type StoredThreshold,
} from '../../types/api';
import { MetricPicker } from '../MetricPicker/MetricPicker';
import { InterfaceRulesModal } from '../InterfaceRules/InterfaceRulesModal';
import { portChoices, type PortChoice } from './portChoice';
import { NodePicker } from '../NodePicker/NodePicker';
import { FormError, FormFooter } from '../ui/FormFooter';
import { Modal } from '../ui/Modal';
import { Field, Select, TextInput } from '../ui/Field';
import { MultiSelectList } from '../ui/MultiSelectList';
import { useEntityNames } from '../ui/entityNames';
import { groupOptions } from '../../lib/nodeTree';

/** The target control for the level the operator has chosen.
 *
 *  Before ADR-075 Inc.3 this was one free-text box for every level, and the id it wanted — a
 *  device profile's UUID — is printed nowhere in the WebUI, so creating a profile-scoped rule was
 *  not actually possible. A mistyped id is not an error either: the engine compares it and simply
 *  never matches, so the rule is created, listed, and silently evaluates for no node.
 *
 *  Since ADR-078 a rule may name SEVERAL targets, which is why the profile and folder-group
 *  controls are `MultiSelectList` and the node one is a list of chips fed by the picker. The two
 *  levels that stay single are the ones with no second thing to pick: an `interface` rule covers
 *  one port and is created from that port's own screen, and the legacy tag scope is free text.
 *
 *  Which control belongs to which level is `scopeIdKind`'s answer, not a second `switch` here —
 *  the same answer decides whether Save may be pressed, and two copies would let the dialog show a
 *  picker while readiness still tested a text box. */
function ScopeIdField({
  form,
  onChange,
  portNames,
}: {
  form: ThresholdForm;
  onChange: (scopeIds: string[]) => void;
  portNames?: Readonly<Record<string, string>>;
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
  const options = useMemo(
    () =>
      kind === 'profile'
        ? profiles.map((p) => ({ value: p.id, label: p.name }))
        : groupItems.map((g) => ({ value: g.id, label: g.label })),
    [kind, profiles, groupItems],
  );

  // One target, or none. Every branch below reads the list rather than a scalar, so a level that
  // is single today needs no second code path if it ever stops being.
  const single = form.scopeIds[0] ?? '';
  const toggle = (value: string) =>
    onChange(
      form.scopeIds.includes(value)
        ? form.scopeIds.filter((s) => s !== value)
        : [...form.scopeIds, value],
    );

  if (kind === 'none') {
    return (
      <div className="modal-field">
        <p className="form-status">{t('thresholds.addModal.globalScope')}</p>
      </div>
    );
  }
  // What the target list needs said, by level: a folder reaches the folders inside it, and the
  // legacy tag scope is on its way out. The others say nothing — the picker names its own targets
  // (ADR-200).
  const hint =
    kind === 'folderGroup'
      ? t('thresholds.addModal.folderGroupHint')
      : kind === 'tag'
        ? t('thresholds.addModal.legacyTagHint')
        : null;
  return (
    <div className="modal-field">
      <label className="modal-field-label">{t('thresholds.addModal.scopeId')}</label>
      {kind === 'node' ? (
        <>
          {form.scopeIds.length > 0 && (
            <ul className="thresholds-chips">
              {form.scopeIds.map((id) => (
                <li key={id} className="thresholds-chip">
                  <span>{nodeName(id)}</span>
                  <button
                    type="button"
                    onClick={() => onChange(form.scopeIds.filter((s) => s !== id))}
                    aria-label={t('thresholds.addModal.removeTarget', { name: nodeName(id) })}
                  >
                    ×
                  </button>
                </li>
              ))}
            </ul>
          )}
          {/* Always null: the picker ADDS here rather than holding the selection, so picking a
              second node cannot silently replace the first. The chips above are the selection. */}
          <NodePicker
            value={null}
            onChange={(n) => {
              if (n && !form.scopeIds.includes(n.id)) onChange([...form.scopeIds, n.id]);
            }}
            exclude={new Set(form.scopeIds)}
            placeholder={t('thresholds.addModal.scopeIdPlaceholder.node')}
          />
        </>
      ) : kind === 'profile' || kind === 'folderGroup' ? (
        <MultiSelectList
          options={options}
          selected={form.scopeIds}
          onToggle={toggle}
          onClear={() => onChange([])}
          label={t(
            kind === 'profile'
              ? 'thresholds.addModal.scopeIdPlaceholder.profile'
              : 'thresholds.addModal.scopeIdPlaceholder.group_id',
          )}
        />
      ) : kind === 'interface' ? (
        // One port of one node (ADR-076), when an existing rule is edited: the port is shown, not
        // changed — moving a rule to another port is a different rule. A NEW port rule never gets
        // here: `PortPick` takes the dialog over and hands the port to the port-rule dialog.
        <>
          <div className="thresholds-fixed mono">
            {interfaceScopeLabel(single, nodeName, portNames)}
          </div>
          <input type="hidden" value={single} readOnly />
        </>
      ) : (
        // The legacy tag scope. Free text because a tag *is* free text, and because no list of the
        // ones in use is reachable from here: since ADR-135 Inc.2 a node's tags are partly its
        // folder chain's, so "every tag in use" is a server-side question, not one this screen can
        // answer from the rule it is editing.
        <TextInput
          className="mono"
          placeholder={t('thresholds.addModal.scopeIdPlaceholder.group')}
          value={single}
          onChange={(e) => onChange(e.target.value ? [e.target.value] : [])}
        />
      )}
      {hint && <span className="form-status">{hint}</span>}
    </div>
  );
}
/** A port the operator picked, with what the port-rule dialog needs to open on it. */
type PickedPort = PortChoice & { nodeId: string };

/** A node, then one of its ports (ADR-209 decision 7). A port rule used to be creatable only from
 *  Node detail ▸ Interfaces; this is the same rule reached from the rules screen, and it is made in
 *  the same dialog that tab opens, so there is one port-rule form, not two. */
function PortPick({
  picked,
  onPick,
}: {
  picked: PickedPort | null;
  onPick: (port: PickedPort | null) => void;
}) {
  const { t } = useTranslation('alertsConfig');
  const [node, setNode] = useState<{ id: string; name: string } | null>(null);
  /** `null` while the node's ports are being read. */
  const [ports, setPorts] = useState<PortChoice[] | null>(null);
  const [failed, setFailed] = useState(false);

  useEffect(() => {
    if (!node) return undefined;
    let live = true;
    setPorts(null);
    setFailed(false);
    api
      .listNodeInterfaces(node.id)
      .then((rows) => {
        if (live) setPorts(portChoices(rows));
      })
      .catch(() => {
        if (!live) return;
        setPorts([]);
        setFailed(true);
      });
    return () => {
      live = false;
    };
  }, [node]);

  return (
    <>
      <div className="modal-field">
        <label className="modal-field-label">{t('thresholds.scopeLevel.node')}</label>
        <NodePicker
          value={node?.id ?? null}
          valueLabel={node?.name}
          onChange={(n) => {
            setNode(n);
            onPick(null);
          }}
          placeholder={t('thresholds.addModal.scopeIdPlaceholder.node')}
        />
      </div>
      {node && (
        <div className="modal-field">
          <label className="modal-field-label" htmlFor="threshold-port">
            {t('thresholds.addModal.port')}
          </label>
          {ports === null ? (
            <p className="form-status">{t('common:loading')}</p>
          ) : ports.length === 0 ? (
            <p className="form-status">
              {failed ? t('thresholds.addModal.portsFailed') : t('thresholds.addModal.noPorts')}
            </p>
          ) : (
            <Select
              id="threshold-port"
              value={picked ? String(picked.ifindex) : ''}
              onChange={(e) => {
                const p = ports.find((x) => String(x.ifindex) === e.target.value);
                onPick(p ? { ...p, nodeId: node.id } : null);
              }}
            >
              <option value="" disabled>
                {t('thresholds.addModal.portPlaceholder')}
              </option>
              {ports.map((p) => (
                <option key={p.ifindex} value={String(p.ifindex)}>
                  {p.option}
                </option>
              ))}
            </Select>
          )}
        </div>
      )}
    </>
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
  portNames,
  onClose,
  onSaved,
}: {
  mode: 'add' | 'edit';
  rule?: StoredThreshold;
  /** The list's `port_names`, so a port rule's target reads as it does in the row. */
  portNames?: Readonly<Record<string, string>>;
  onClose: () => void;
  /** After a save. `interface` when the dialog handed off to the port-rule dialog, so the list can
   *  bring port rules into view — its default leaves them out (ADR-076 decision 12). */
  onSaved: (level?: ScopeLevel) => void;
}) {
  const { t } = useTranslation('alertsConfig');
  // ⚠️ Held here, inside a conditionally-mounted component, so closing the dialog *is* the reset
  // (ui-conventions "Modals"). It briefly took a `prefill` for the Interfaces dock; that caller
  // now opens its own port-shaped dialog (ADR-076 Inc.5), and a prop with no caller is a prop
  // nothing keeps true.
  const [form, setForm] = useState<ThresholdForm>(() => thresholdFormFrom(rule));
  const save = useSubmit({
    errorFallback: t('thresholds.err.save'),
    onDone: () => {
      onSaved();
      onClose();
    },
  });
  const set = <K extends keyof ThresholdForm>(key: K, value: ThresholdForm[K]) =>
    setForm((f) => ({ ...f, [key]: value }));

  const ready = isThresholdReady(form);
  // A NEW port rule is not filled in here: the dialog picks the port, then gives way to the
  // port-rule dialog (ADR-209 decision 7). Editing one stays here, as it always did.
  const portMode = mode === 'add' && form.level === 'interface';
  const [port, setPort] = useState<PickedPort | null>(null);
  const [handoff, setHandoff] = useState<PickedPort | null>(null);
  /** Whether the port-rule dialog wrote anything. Closing it without a save is a cancel, and must
   *  not reload the list or open its level filter as if a rule had been made. */
  const portRuleChanged = useRef(false);

  // The legacy tag-based `group` level is not offered for a *new* rule — a folder group says the
  // same thing with a type on it, recursively, and is the unit RBAC already scopes by (ADR-075
  // Inc.3, the same move the maintenance-window form already made). ⚠️ The reason used to be
  // "nothing writes `nodes.tags`", which ADR-135 made false; the decision did not change with it.
  // ⚠️ It must still appear while editing a rule
  // that already sits at it: a `<select>` whose value is absent from its options renders blank,
  // and the next save would silently move the rule to whichever level rendered first.
  // A port is picked only when a rule is ADDED (ADR-209 decision 7): an existing rule cannot be
  // moved onto a port, because nothing in the edit path picks one — offering the level there
  // would be a choice that leaves the target empty.
  const levels = useMemo(() => {
    const base =
      mode === 'add'
        ? CREATABLE_SCOPE_LEVELS
        : CREATABLE_SCOPE_LEVELS.filter((l) => l !== 'interface');
    return base.includes(form.level) ? base : [...base, form.level];
  }, [mode, form.level]);

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
    const body = thresholdBody(form);
    save.submit(() => {
      const call =
        mode === 'edit' && rule ? api.updateThreshold(rule.id, body) : api.createThreshold(body);
      return call.then(() => done());
    });
  };

  if (handoff) {
    return (
      <InterfaceRulesModal
        nodeId={handoff.nodeId}
        ifindex={handoff.ifindex}
        portLabel={handoff.label}
        speedBps={handoff.speedBps}
        startAdding
        onChanged={() => {
          portRuleChanged.current = true;
        }}
        onClose={() => {
          if (portRuleChanged.current) onSaved('interface');
          onClose();
        }}
      />
    );
  }

  return (
    <Modal
      title={mode === 'edit' ? t('thresholds.editModal.title') : t('thresholds.addModal.title')}
      onClose={onClose}
      footer={
        <FormFooter
          form={save}
          onClose={onClose}
          onSubmit={portMode ? () => port && setHandoff(port) : submit}
          submitLabel={
            mode === 'edit'
              ? t('common:actions.save')
              : portMode
                ? t('thresholds.addModal.portNext')
                : t('thresholds.addModal.add')
          }
          canSubmit={portMode ? port !== null : ready}
        />
      }
    >
      <div className="modal-field">
        <label className="modal-field-label">{t('thresholds.addModal.scopeLevel')}</label>
        <Select
          value={form.level}
          onChange={(e) =>
            // Clear the targets with the level. Ids are not interchangeable across levels — a
            // profile UUID left behind on a folder-group rule is a rule that matches nothing —
            // and every control below is a picker, so nothing an operator would want is carried.
            {
              setForm((f) => ({ ...f, level: e.target.value as ScopeLevel, scopeIds: [] }));
              setPort(null);
            }
          }
        >
          {levels.map((l) => (
            <option key={l} value={l}>
              {t(`thresholds.scopeLevel.${l}`)}
            </option>
          ))}
        </Select>
      </div>
      {portMode ? (
        <PortPick picked={port} onPick={setPort} />
      ) : (
        <ScopeIdField
          form={form}
          onChange={(scopeIds) => set('scopeIds', scopeIds)}
          portNames={portNames}
        />
      )}
      {!portMode && (
        <ThresholdFields form={form} set={set} lockedMetric={lockedMetric} noBounds={noBounds} />
      )}
      <FormError form={save} />
    </Modal>
  );
}

/** Everything below the target: the metric, the row pattern, the bounds and the breach count. */
function ThresholdFields({
  form,
  set,
  lockedMetric,
  noBounds,
}: {
  form: ThresholdForm;
  set: <K extends keyof ThresholdForm>(key: K, value: ThresholdForm[K]) => void;
  lockedMetric: boolean;
  noBounds: boolean;
}) {
  const { t } = useTranslation('alertsConfig');
  return (
    <>
      <div className="modal-field">
        <label className="modal-field-label">{t('thresholds.addModal.metric')}</label>
        {lockedMetric ? (
          // Why there are no bounds is said once, beside the breach count below.
          <p className="thresholds-fixed">{t('format:liveness')}</p>
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
      {/* ADR-143: which rows of a table metric the rule reaches — one memory pool, one board. Not
          offered for a port rule, which already names exactly one thing and which the server refuses
          a pattern on; the form sends none there even if one was typed before the level changed. */}
      {scopeAcceptsRowMatch(form.level) && !noBounds && (
        // The ⓘ carries the one thing the placeholder cannot: a named rule beats an unnamed one
        // at the same scope (ADR-143), which decides whether a second rule is needed at all.
        <Field
          label={t('thresholds.addModal.rowMatch')}
          htmlFor="threshold-row-match"
          infoKey="alertsConfig:thresholds.addModal.row.info"
        >
          <TextInput
            id="threshold-row-match"
            className="mono"
            placeholder={t('thresholds.addModal.rowMatchPlaceholder')}
            value={form.rowMatch}
            onChange={(e) => set('rowMatch', e.target.value)}
          />
        </Field>
      )}
      {/* ADR-081: there is no direction selector. The rule faces whichever way the operator filled
          in, and filling both rows alerts outside a band — a dark optical link *and* an overdriven
          one, from one rule. A selector beside the numbers was a second statement of the same fact,
          and the two could disagree: the form let a rule be saved saying `above` with bounds that
          only made sense downward, which stored, listed, and never fired. */}
      {!noBounds && (
        <div className="modal-field">
          <label className="modal-field-label">{t('thresholds.addModal.bounds')}</label>
          <div className="thresholds-bound-row">
            <span className="thresholds-bound-side">{t('thresholds.addModal.belowSide')}</span>
            <TextInput
              className="thresholds-num"
              placeholder={t('thresholds.addModal.warnPlaceholder')}
              value={form.warningBelow}
              onChange={(e) => set('warningBelow', e.target.value)}
            />
            <TextInput
              className="thresholds-num"
              placeholder={t('thresholds.addModal.critPlaceholder')}
              value={form.criticalBelow}
              onChange={(e) => set('criticalBelow', e.target.value)}
            />
          </div>
          <div className="thresholds-bound-row">
            <span className="thresholds-bound-side">{t('thresholds.addModal.aboveSide')}</span>
            <TextInput
              className="thresholds-num"
              placeholder={t('thresholds.addModal.warnPlaceholder')}
              value={form.warningAbove}
              onChange={(e) => set('warningAbove', e.target.value)}
            />
            <TextInput
              className="thresholds-num"
              placeholder={t('thresholds.addModal.critPlaceholder')}
              value={form.criticalAbove}
              onChange={(e) => set('criticalAbove', e.target.value)}
            />
          </div>
        </div>
      )}
      {/* The label and the unit say what the number is; the ⓘ is the one case where a reading is
          not a poll (the per-minute interface metrics). An empty box sends the default, so the
          placeholder is that default rather than a word. */}
      <Field
        label={t('thresholds.addModal.dwell.label')}
        htmlFor="threshold-dwell"
        infoKey="alertsConfig:thresholds.addModal.dwell.info"
      >
        <div className="thresholds-bounds">
          <TextInput
            id="threshold-dwell"
            className="thresholds-num"
            placeholder={String(DEFAULT_DWELL)}
            suffix={t('thresholds.addModal.dwell.unit')}
            value={form.dwell}
            onChange={(e) => set('dwell', e.target.value)}
          />
        </div>
        {noBounds && <span className="form-status">{t('thresholds.livenessMetric')}</span>}
      </Field>
    </>
  );
}
