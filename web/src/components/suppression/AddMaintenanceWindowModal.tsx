// SPDX-License-Identifier: AGPL-3.0-only
// Create a maintenance window — shared by the Maintenance page ("+ Add window") and the All Nodes
// right-click "Custom…" path. When `initialScope` is set the scope is fixed to that node/folder
// group (the operator already picked the target); otherwise the scope is chosen here (node /
// folder group / device profile). A folder group is `group_id`, recursive incl. subgroups
// (ADR-022); the old label-scoped windows were deleted by migration 0164 (ADR-229).
//
// The time fields follow ADR-261: the target comes first, then what the work is (blank saves the
// default name), then a year-first start date and a 24-hour start time in a zone the operator can
// change from its name beside the time, then the length. The reading of all of that is
// `lib/maintenanceTime.ts`; the zone the times were typed in is sent with the window and stored.

import { useMemo, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { api } from '../../services/api';
import type { MaintenanceScopeLevel, NodeGroup, ProfileSummary } from '../../types/api';
import { done, partialOutcome } from '../../lib/submitState';
import { useSubmit } from '../../lib/useSubmit';
import { Modal } from '../ui/Modal';
import { FormError, FormFooter } from '../ui/FormFooter';
import { Field, TextInput, Select } from '../ui/Field';
import { FieldGroup, Fields } from '../ui/FormLayout';
import { Inline, Stack } from '../ui/Page';
import { Button } from '../ui/Button';
import { Segmented } from '../ui/Segmented';
import { DateInput, TimeInput } from '../ui/WallTimeInput';
import { ZonePicker } from '../ui/ZonePicker';
import { NodePicker } from '../NodePicker/NodePicker';
import { groupOptions } from '../../lib/nodeTree';
import { GroupPicker } from '../ui/GroupPicker';
import { displayTimeZone, zoneAbbreviation } from '../../lib/timeZone';
import { targetNodeNames, type ActionTarget } from '../../lib/actionTarget';
import {
  DEFAULT_WINDOW_LENGTH,
  TONIGHT_HOUR,
  WINDOW_LENGTHS,
  dateText,
  instantToWall,
  rangeLine,
  targetZone,
  timeText,
  tonightStart,
  wallLine,
  windowName,
  windowTimes,
  type WindowLengthKey,
} from '../../lib/maintenanceTime';
import { usePrefsStore } from '../../prefs';

/** Scope choices when not locked to a right-click target. */
type CreateScope = 'node' | 'group_id' | 'profile';

interface Props {
  groups: NodeGroup[];
  /** Offered as the "profile" scope when present; omit (or empty) to hide that choice. */
  profiles?: ProfileSummary[];
  /** When set, the scope is fixed to this node, folder or working set (the All Nodes right-click
   *  "Custom…" path, and the selection bar's More… menu since ADR-124 Inc.11). */
  initialScope?: ActionTarget;
  onClose: () => void;
  onSaved: () => void;
}

export function AddMaintenanceWindowModal({
  groups,
  profiles = [],
  initialScope,
  onClose,
  onSaved,
}: Props) {
  const { t } = useTranslation('suppression');
  const locked = !!initialScope;
  // A set of nodes is written through the bulk endpoint, which takes ids rather than one scope —
  // so the scope controls below are locked exactly as they are for a single target.
  const batch = initialScope?.kind === 'nodes' ? initialScope : null;
  const names = initialScope ? targetNodeNames(initialScope) : [];
  // Blank on purpose (ADR-261 decision 2): the field asks what the work is, and saving it blank
  // falls back to the name below, which the field shows while it is blank.
  const [name, setName] = useState('');
  const [scope, setScope] = useState<CreateScope>(
    initialScope && initialScope.kind === 'group' ? 'group_id' : 'node',
  );
  const [scopeId, setScopeId] = useState(
    initialScope && initialScope.kind !== 'nodes' ? initialScope.id : '',
  );
  // Resolved name for the node picker's trigger (typeahead over the lazily-loaded inventory, so it
  // scales past the old flat <select> of the first 100 nodes — S12).
  const [nodeLabel, setNodeLabel] = useState(
    initialScope?.kind === 'node' ? (initialScope.name ?? '') : '',
  );
  const mine = displayTimeZone();
  const [zone, setZone] = useState(mine);
  const [startDate, setStartDate] = useState('');
  const [startTime, setStartTime] = useState('');
  const [length, setLength] = useState<WindowLengthKey>(DEFAULT_WINDOW_LENGTH);
  const [endDate, setEndDate] = useState('');
  const [endTime, setEndTime] = useState('');
  const recentZones = usePrefsStore((s) => s.recentZones);
  const rememberZone = usePrefsStore((s) => s.rememberZone);
  const form = useSubmit({
    errorFallback: t('maintenanceForm.err.add'),
    onDone: () => {
      onSaved();
      onClose();
    },
  });

  const groupItems = groupOptions(groups);

  // The name the window gets when the work field is left blank: the target's own name.
  const targetName = batch
    ? null
    : scope === 'node'
      ? nodeLabel
      : scope === 'profile'
        ? (profiles.find((p) => p.id === scopeId)?.name ?? '')
        : (groups.find((g) => g.id === scopeId)?.name ??
          (initialScope?.kind === 'group' ? initialScope.name : ''));
  const fallbackName = batch
    ? t('maintenanceForm.defaultNameMany', { count: batch.nodes.length })
    : targetName
      ? t('maintenanceForm.defaultName', { name: targetName })
      : '';
  const savedName = windowName(name, fallbackName);

  // The zone the target's site keeps (decision 6): a folder's own, a node's folder's, or the one
  // every selected node shares. A profile names a class, not a place, so it has none.
  const site = useMemo(() => {
    const zoneOf = (id: string) => groups.find((g) => g.id === id)?.effective_time_zone;
    if (batch) return targetZone(batch.nodes.map((n) => n.group_id), zoneOf);
    if (scope === 'group_id') return scopeId ? targetZone([scopeId], zoneOf) : null;
    if (scope === 'node' && initialScope?.kind === 'node') {
      return targetZone([initialScope.groupId], zoneOf);
    }
    return null;
  }, [batch, scope, scopeId, initialScope, groups]);

  const times = windowTimes({ startDate, startTime, length, endDate, endTime, zone });
  // What the abbreviations are read at: the start once it is known, else now.
  const at = times.kind === 'ok' ? times.startMs : Date.now();

  const fieldError = (field: 'startDate' | 'startTime' | 'endDate' | 'endTime') =>
    times.kind === 'invalid' && times.field === field
      ? field.endsWith('Date')
        ? t('maintenanceForm.err.date')
        : t('maintenanceForm.err.time')
      : null;
  const startError =
    fieldError('startDate') ??
    fieldError('startTime') ??
    (times.kind === 'gap' && times.field === 'start'
      ? t('maintenanceForm.err.gap', { zone })
      : null);
  const endError =
    fieldError('endDate') ??
    fieldError('endTime') ??
    (times.kind === 'gap' && times.field === 'end'
      ? t('maintenanceForm.err.gap', { zone })
      : times.kind === 'backwards'
        ? t('maintenanceForm.err.backwards')
        : null);

  const ready =
    !!savedName && (!!batch || !!scopeId.trim()) && times.kind === 'ok';

  const setStart = (w: { date: { year: number; month: number; day: number }; time: { hour: number; minute: number } }) => {
    setStartDate(dateText(w.date));
    setStartTime(timeText(w.time));
  };

  const submit = () => {
    if (!ready || times.kind !== 'ok') return;
    const startsAt = new Date(times.startMs).toISOString();
    const endsAt = new Date(times.endMs).toISOString();
    if (zone !== mine) rememberZone(zone);
    form.submit(() => {
      if (batch) {
        // 🚨 A shortfall keeps the dialog open. Closing on `created < requested` would read as
        // "every node is covered tonight", which is the belief a maintenance window exists to
        // make true.
        return api
          .createMaintenanceWindows({
            node_ids: batch.nodes.map((n) => n.id),
            name: savedName,
            starts_at: startsAt,
            ends_at: endsAt,
            time_zone: zone,
          })
          .then((r) =>
            partialOutcome(
              r.created,
              r.requested,
              ({ applied, requested }) =>
                t('maintenanceForm.partial', { created: applied, requested }),
              false,
            ),
          );
      }
      return api
        .createMaintenanceWindow({
          name: savedName,
          scope_level: scope as MaintenanceScopeLevel,
          scope_id: scopeId.trim(),
          starts_at: startsAt,
          ends_at: endsAt,
          time_zone: zone,
        })
        .then(() => done());
    });
  };

  const lengthOptions = [
    ...WINDOW_LENGTHS.map((l) => ({ value: l.key, label: t(`maintenanceForm.lengths.${l.key}`) })),
    { value: 'custom', label: t('maintenanceForm.lengths.custom') },
  ];

  return (
    <Modal
      title={t('maintenanceForm.title')}
      onClose={onClose}
      footer={
        <FormFooter
          form={form}
          onClose={onClose}
          onSubmit={submit}
          submitLabel={t('maintenanceForm.submit')}
          canSubmit={ready}
        />
      }
    >
      <Fields>
        {batch ? (
          // A group: the scope here is a list of nodes, not one control.
          <FieldGroup label={t('maintenanceForm.scope')}>
            <p className="form-status">{t('maintenanceForm.lockedNodes', { count: names.length })}</p>
            <ul className="form-targets scroll-y">
              {names.map((n, i) => (
                <li key={`${n}-${i}`}>{n}</li>
              ))}
            </ul>
          </FieldGroup>
        ) : locked ? (
          <Field label={t('maintenanceForm.scope')}>
            <p className="form-status">
              {initialScope?.kind === 'group'
                ? t('maintenanceForm.lockedGroup')
                : t('maintenanceForm.lockedNode')}
              :{' '}
              <strong>{initialScope && initialScope.kind !== 'nodes' ? initialScope.name : ''}</strong>
              {initialScope?.kind === 'group' && t('maintenanceForm.inclSubgroups')}
            </p>
          </Field>
        ) : (
          <>
            <Field label={t('maintenanceForm.scopeLevel')}>
              <Select
                value={scope}
                onChange={(e) => {
                  setScope(e.target.value as CreateScope);
                  setScopeId('');
                  setNodeLabel('');
                }}
              >
                <option value="node">{t('maintenanceForm.level.node')}</option>
                <option value="group_id">
                  {`${t('maintenanceForm.level.groupId')}${t('maintenanceForm.inclSubgroups')}`}
                </option>
                {profiles.length > 0 && (
                  <option value="profile">{t('maintenanceForm.level.profile')}</option>
                )}
              </Select>
            </Field>
            <Field label={t('maintenanceForm.scope')}>
              {scope === 'node' ? (
                <NodePicker
                  id="maint-node"
                  value={scopeId || null}
                  valueLabel={nodeLabel || undefined}
                  onChange={(n) => {
                    setScopeId(n?.id ?? '');
                    setNodeLabel(n?.name ?? '');
                  }}
                  placeholder={t('maintenanceForm.pickNode')}
                />
              ) : scope === 'profile' ? (
                <Select value={scopeId} onChange={(e) => setScopeId(e.target.value)}>
                  <option value="">{t('maintenanceForm.pickProfile')}</option>
                  {profiles.map((p) => (
                    <option key={p.id} value={p.id}>
                      {p.name}
                    </option>
                  ))}
                </Select>
              ) : (
                <GroupPicker
                  options={groupItems}
                  value={scopeId}
                  onChange={setScopeId}
                  emptyOption={t('maintenanceForm.pickGroup')}
                />
              )}
            </Field>
          </>
        )}

        <Field
          label={t('maintenanceForm.name')}
          status={!name.trim() && fallbackName ? t('maintenanceForm.savesAs', { name: fallbackName }) : null}
        >
          <TextInput
            placeholder={t('maintenanceForm.namePlaceholder')}
            value={name}
            onChange={(e) => setName(e.target.value)}
            autoFocus
          />
        </Field>

        <Field label={t('maintenanceForm.start')} htmlFor="maint-start-date" error={startError}>
          <Stack gap={6}>
            <Inline gap={8}>
              <DateInput
                id="maint-start-date"
                value={startDate}
                onChange={setStartDate}
                invalid={times.kind === 'invalid' && times.field === 'startDate'}
              />
              <TimeInput
                value={startTime}
                onChange={setStartTime}
                invalid={times.kind === 'invalid' && times.field === 'startTime'}
                aria-label={t('maintenanceForm.startTime')}
              />
              <ZonePicker
                zone={zone}
                onChange={setZone}
                at={at}
                mine={mine}
                site={site}
                recent={recentZones}
              />
            </Inline>
            <Inline gap={6}>
              <Button
                type="button"
                variant="ghost"
                onClick={() => setStart(instantToWall(Date.now(), zone))}
              >
                {t('maintenanceForm.now')}
              </Button>
              <Button
                type="button"
                variant="ghost"
                onClick={() => setStart(tonightStart(Date.now(), zone))}
              >
                {t('maintenanceForm.tonight', { time: timeText({ hour: TONIGHT_HOUR, minute: 0 }) })}
              </Button>
            </Inline>
          </Stack>
        </Field>

        <Field
          label={t('maintenanceForm.length')}
          htmlFor="maint-length"
          status={
            times.kind === 'ok' && length !== 'custom'
              ? t('maintenanceForm.ends', { at: wallLine(times.endMs, zone) })
              : null
          }
        >
          <Segmented
            options={lengthOptions}
            value={length}
            onChange={(v) => setLength(v as WindowLengthKey)}
            ariaLabel={t('maintenanceForm.length')}
          />
        </Field>

        {length === 'custom' && (
          <Field label={t('maintenanceForm.end')} htmlFor="maint-end-date" error={endError}>
            <Inline gap={8}>
              <DateInput
                id="maint-end-date"
                value={endDate}
                onChange={setEndDate}
                invalid={times.kind === 'invalid' && times.field === 'endDate'}
              />
              <TimeInput
                value={endTime}
                onChange={setEndTime}
                invalid={times.kind === 'invalid' && times.field === 'endTime'}
                aria-label={t('maintenanceForm.endTime')}
              />
              <span className="form-status">{zoneAbbreviation(zone, at)}</span>
            </Inline>
          </Field>
        )}

        {times.kind === 'ok' && zone !== mine && (
          <p className="form-status">
            {t('maintenanceForm.yourTime', { range: rangeLine(times.startMs, times.endMs, mine) })}
          </p>
        )}
        <FormError form={form} />
      </Fields>
    </Modal>
  );
}
