// SPDX-License-Identifier: AGPL-3.0-only
// Create a maintenance window — shared by the Maintenance page ("+ Add window") and the All Nodes
// right-click "Custom…" path. When `initialScope` is set the scope is fixed to that node/folder
// group (the operator already picked the target); otherwise the scope is chosen here (node /
// folder group / device profile). A folder group is `group_id`, recursive incl. subgroups
// (ADR-022); the old label-scoped windows were deleted by migration 0164 (ADR-229).
//
// The time fields follow ADR-261: the target comes first, then what the work is (blank saves the
// default name), then a year-first start date and a 24-hour start time in a zone the operator can
// change from its name beside the time, then the end, always as a date and a time (Inc.6), with
// shortcuts that fill it from the start. Moving the start moves the end with it, so the length
// stays. A new window opens from the next whole hour to an hour after it (Inc.7), and the end is
// never left at or before the start. The reading of all of that is `lib/maintenanceTime.ts`; the
// zone the times were typed in is sent with the window and stored.
//
// The same dialog edits a window that exists (ADR-261 Inc.5): every field starts from the window
// on the window's own clock, and Save replaces all of it.

import { useMemo, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { api } from '../../services/api';
import type {
  MaintenanceScopeLevel,
  MaintenanceWindow,
  NodeGroup,
  ProfileSummary,
} from '../../types/api';
import { done, partialOutcome } from '../../lib/submitState';
import { useSubmit } from '../../lib/useSubmit';
import { Modal } from '../ui/Modal';
import { FormError, FormFooter } from '../ui/FormFooter';
import { Field, TextInput, Select } from '../ui/Field';
import { FieldGroup, Fields } from '../ui/FormLayout';
import { Inline } from '../ui/Page';
import { Button } from '../ui/Button';
import {
  DateInput,
  TimeInput,
  WallTimeCols,
  WallTimeRows,
  WallTimeZone,
} from '../ui/WallTimeInput';
import { ZonePicker } from '../ui/ZonePicker';
import { NodePicker } from '../NodePicker/NodePicker';
import { groupOptions } from '../../lib/nodeTree';
import { GroupPicker } from '../ui/GroupPicker';
import { displayTimeZone, zoneAbbreviation } from '../../lib/timeZone';
import { formatDuration } from '../../lib/format';
import { targetNodeNames, type ActionTarget } from '../../lib/actionTarget';
import {
  TONIGHT_HOUR,
  NEW_WINDOW_MINUTES,
  WINDOW_LENGTHS,
  dateText,
  endAfter,
  followStart,
  instantToWall,
  keptLength,
  newWindowFields,
  nextWholeHour,
  rangeLine,
  settleEnd,
  stepEnd,
  stepWallText,
  targetZone,
  timeText,
  tonightStart,
  windowFields,
  windowName,
  windowTimes,
  type Wall,
  type WallText,
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
  /** A window to edit instead of adding one, with its target's display name (ADR-261 Inc.5). */
  editing?: { win: MaintenanceWindow; scopeName: string };
  onClose: () => void;
  onSaved: () => void;
}

export function AddMaintenanceWindowModal({
  groups,
  profiles = [],
  initialScope,
  editing,
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
  const win = editing?.win;
  const [name, setName] = useState(win?.name ?? '');
  const [scope, setScope] = useState<CreateScope>(
    win
      ? (win.scope_level as CreateScope)
      : initialScope && initialScope.kind === 'group'
        ? 'group_id'
        : 'node',
  );
  const [scopeId, setScopeId] = useState(
    win ? win.scope_id : initialScope && initialScope.kind !== 'nodes' ? initialScope.id : '',
  );
  // Resolved name for the node picker's trigger (typeahead over the lazily-loaded inventory, so it
  // scales past the old flat <select> of the first 100 nodes — S12).
  const [nodeLabel, setNodeLabel] = useState(
    win?.scope_level === 'node'
      ? (editing?.scopeName ?? '')
      : initialScope?.kind === 'node'
        ? (initialScope.name ?? '')
        : '',
  );
  const mine = displayTimeZone();
  // An edited window keeps the clock it was asked in; one that recorded none reads on mine.
  const [zone, setZone] = useState(win?.time_zone ?? mine);
  // A new window opens from the next whole hour to an hour after it (decision 40).
  const [initial] = useState(() =>
    win
      ? windowFields(win.starts_at, win.ends_at, win.time_zone ?? mine)
      : newWindowFields(Date.now(), mine),
  );
  const [startDate, setStartDate] = useState(initial.startDate);
  const [startTime, setStartTime] = useState(initial.startTime);
  const [endDate, setEndDate] = useState(initial.endDate);
  const [endTime, setEndTime] = useState(initial.endTime);
  // The length the end keeps while the start moves (decision 34): the window's own length from
  // the outset, and again whenever the end is set after a start that reads (decision 41).
  const [kept, setKept] = useState(
    () =>
      keptLength(
        { date: initial.startDate, time: initial.startTime },
        { date: initial.endDate, time: initial.endTime },
        zone,
      ) ?? NEW_WINDOW_MINUTES * 60_000,
  );
  const recentZones = usePrefsStore((s) => s.recentZones);
  const rememberZone = usePrefsStore((s) => s.rememberZone);
  const form = useSubmit({
    errorFallback: win ? t('maintenanceForm.err.edit') : t('maintenanceForm.err.add'),
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
  // A batch names each window after its own node: `{node}` is filled in by the server.
  const fallbackName = batch
    ? t('maintenanceForm.defaultName', { name: '{node}' })
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

  const start: WallText = { date: startDate, time: startTime };
  const end: WallText = { date: endDate, time: endTime };
  // What is measured and saved: the end where it settles once it is left (decision 41), so an end
  // still being typed is never reported as before the start.
  const settled = settleEnd(start, end, kept, zone) ?? end;
  const times = windowTimes({
    startDate,
    startTime,
    endDate: settled.date,
    endTime: settled.time,
    zone,
  });
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

  // Every way the start changes goes through here, so the end follows it (decision 34).
  const moveStart = (next: WallText) => {
    setStartDate(next.date);
    setStartTime(next.time);
    const followed = followStart(next, kept, zone);
    if (followed) {
      setEndDate(followed.date);
      setEndTime(followed.time);
    }
  };
  // And every way the end changes goes through here, so the length it now makes is the one kept -
  // while it is after the start; one still being typed keeps the length it had (decision 41).
  const moveEnd = (next: WallText) => {
    setEndDate(next.date);
    setEndTime(next.time);
    setKept((k) => keptLength(start, next, zone) ?? k);
  };
  const settleTo = (next: WallText) => moveEnd(settleEnd(start, next, kept, zone) ?? next);
  const setStart = (w: Wall) => moveStart({ date: dateText(w.date), time: timeText(w.time) });

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
      const body = {
        name: savedName,
        scope_level: scope as MaintenanceScopeLevel,
        scope_id: scopeId.trim(),
        starts_at: startsAt,
        ends_at: endsAt,
        time_zone: zone,
      };
      return (win
        ? api.updateMaintenanceWindow(win.id, body)
        : api.createMaintenanceWindow(body).then(() => undefined)
      ).then(() => done());
    });
  };


  return (
    <Modal
      title={win ? t('maintenanceForm.editTitle') : t('maintenanceForm.title')}
      onClose={onClose}
      footer={
        <FormFooter
          form={form}
          onClose={onClose}
          onSubmit={submit}
          submitLabel={win ? t('common:actions.save') : t('maintenanceForm.submit')}
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
          status={
            !name.trim() && fallbackName
              ? t('maintenanceForm.savesAs', {
                  name: batch
                    ? t('maintenanceForm.defaultName', { name: t('maintenanceForm.eachNode') })
                    : fallbackName,
                })
              : null
          }
        >
          <TextInput
            placeholder={t('maintenanceForm.namePlaceholder')}
            value={name}
            onChange={(e) => setName(e.target.value)}
            autoFocus
          />
        </Field>

        <WallTimeRows>
          <Field label={t('maintenanceForm.start')} htmlFor="maint-start-date" error={startError}>
            <>
              <WallTimeCols>
                <DateInput
                  id="maint-start-date"
                  value={startDate}
                  onChange={(date) => moveStart({ ...start, date })}
                  onStep={(dir) =>
                    moveStart(stepWallText(start, 'day', dir, nextWholeHour(Date.now(), zone)))
                  }
                  invalid={times.kind === 'invalid' && times.field === 'startDate'}
                />
                <TimeInput
                  value={startTime}
                  onChange={(time) => moveStart({ ...start, time })}
                  onStep={(unit, dir) =>
                    moveStart(stepWallText(start, unit, dir, nextWholeHour(Date.now(), zone)))
                  }
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
              </WallTimeCols>
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
            </>
          </Field>

          <Field
            label={t('maintenanceForm.end')}
            htmlFor="maint-end-date"
            error={endError}
            status={
              times.kind === 'ok'
                ? t('maintenanceForm.lengthIs', {
                    duration: formatDuration((times.endMs - times.startMs) / 1000, {
                      units: 3,
                      smallest: 'm',
                      skipZero: true,
                    }),
                  })
                : null
            }
          >
            <>
              <WallTimeCols onLeave={() => settleTo(end)}>
                <DateInput
                  id="maint-end-date"
                  value={endDate}
                  onChange={(date) => moveEnd({ ...end, date })}
                  onPick={(date) => settleTo({ ...end, date })}
                  onStep={(dir) => {
                    const next = stepEnd(start, end, 'day', dir, kept, zone, Date.now());
                    if (next) moveEnd(next);
                  }}
                  invalid={times.kind === 'invalid' && times.field === 'endDate'}
                />
                <TimeInput
                  value={endTime}
                  onChange={(time) => moveEnd({ ...end, time })}
                  onStep={(unit, dir) => {
                    const next = stepEnd(start, end, unit, dir, kept, zone, Date.now());
                    if (next) moveEnd(next);
                  }}
                  invalid={times.kind === 'invalid' && times.field === 'endTime'}
                  aria-label={t('maintenanceForm.endTime')}
                />
                <WallTimeZone>{zoneAbbreviation(zone, at)}</WallTimeZone>
              </WallTimeCols>
              <Inline gap={6}>
                {WINDOW_LENGTHS.map((l) => (
                  <Button
                    key={l.key}
                    type="button"
                    variant="ghost"
                    disabled={endAfter(start, l.minutes, zone) == null}
                    onClick={() => {
                      const next = endAfter(start, l.minutes, zone);
                      if (next) moveEnd(next);
                    }}
                  >
                    {t(`maintenanceForm.later.${l.key}`)}
                  </Button>
                ))}
              </Inline>
            </>
          </Field>
        </WallTimeRows>

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
