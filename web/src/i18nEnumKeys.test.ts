// SPDX-License-Identifier: AGPL-3.0-only
// i18n key-COVERAGE for enum-driven dynamic keys.
//
// The parity test next door proves EN and JA agree with *each other*. It cannot prove either is
// complete: ~150 call sites build their key from a value at runtime (`t(`dest.${r.dest_kind}`)`,
// `t(`format:state.${state}`)`, …), so when the backend gains an enum variant and nobody adds the
// strings, BOTH locales are equally missing it — parity passes and the UI renders the raw key
// ("dest.kafka") to the operator.
//
// These tests close that hole for the enums that actually grow. Each one iterates the runtime list
// that `types/api.ts` derives its union from, so adding a variant there without its strings fails
// here, in both languages, naming the key.

import { describe, expect, it } from 'vitest';
import {
  DIRECTIONS,
  FORWARD_DEST_KINDS,
  FORWARD_SOURCE_KINDS,
  GROUP_TYPES,
  ROLES,
  SCOPE_LEVELS,
  SEVERITIES,
  REPORT_FREQUENCIES,
  REPORT_RUN_STATES,
  REPORT_TRIGGERS,
  EVENT_ACTIONS,
  EVENT_MATCH_KINDS,
} from './types/api';
import { SEVERITY_ORDER } from './lib/nodeState';
import { KNOWN_SCALARS } from './lib/format';
import { PROFILE_CATEGORIES } from './lib/profileCategories';
import { METRIC_CARDS } from './components/NodeDetail/metricCards';
import { MONITOR_KINDS } from './pages/monitorKinds';
import { CADENCE, RUN_STATUS, SELECTABLE_FREQUENCIES } from './reports/runStatus';

import enCommon from './locales/en/common.json';
import jaCommon from './locales/ja/common.json';
import enFormat from './locales/en/format.json';
import jaFormat from './locales/ja/format.json';
import enNodes from './locales/en/nodes.json';
import jaNodes from './locales/ja/nodes.json';
import enMonitoring from './locales/en/monitoring.json';
import jaMonitoring from './locales/ja/monitoring.json';
import enReports from './locales/en/reports.json';
import jaReports from './locales/ja/reports.json';
import enAlerts from './locales/en/alerts.json';
import jaAlerts from './locales/ja/alerts.json';
import enDashboard from './locales/en/dashboard.json';
import jaDashboard from './locales/ja/dashboard.json';
import enAlertsConfig from './locales/en/alertsConfig.json';
import jaAlertsConfig from './locales/ja/alertsConfig.json';
import enSettingsForwarding from './locales/en/settings-forwarding.json';
import jaSettingsForwarding from './locales/ja/settings-forwarding.json';

type Json = Record<string, unknown>;

/** Resolve a dotted key path against a namespace object; undefined when any hop is missing. */
function lookup(ns: Json, path: string): unknown {
  return path.split('.').reduce<unknown>((cur, part) => {
    if (cur && typeof cur === 'object' && part in (cur as Json)) return (cur as Json)[part];
    return undefined;
  }, ns);
}

/** Assert every `${prefix}${member}` key resolves to a non-empty string in both locales. */
function expectKeys(
  label: string,
  locales: { en: Json; ja: Json },
  prefix: string,
  members: readonly string[],
) {
  const missing: string[] = [];
  for (const m of members) {
    const path = `${prefix}${m}`;
    for (const [lng, ns] of Object.entries(locales)) {
      const v = lookup(ns as Json, path);
      if (typeof v !== 'string' || v.trim() === '') missing.push(`${lng}:${path}`);
    }
  }
  expect({ label, missing }).toEqual({ label, missing: [] });
}

describe('i18n coverage for enum-driven dynamic keys', () => {
  it('every node state has a label (format:state.*)', () => {
    expectKeys('node state', { en: enFormat, ja: jaFormat }, 'state.', SEVERITY_ORDER);
  });

  it('every alert severity has a label (format:severity.*)', () => {
    expectKeys('severity', { en: enFormat, ja: jaFormat }, 'severity.', SEVERITIES);
  });

  it('every role has a label (common:role.*)', () => {
    expectKeys('role', { en: enCommon, ja: jaCommon }, 'role.', ROLES);
  });

  it('every node-group type has a label (nodes:groupType.*)', () => {
    expectKeys('group type', { en: enNodes, ja: jaNodes }, 'groupType.', GROUP_TYPES);
  });

  it('every threshold scope level and direction has a label (alertsConfig)', () => {
    const locales = { en: enAlertsConfig, ja: jaAlertsConfig };
    expectKeys('scope level', locales, 'thresholds.scopeLevel.', SCOPE_LEVELS);
    expectKeys('scope id placeholder', locales, 'thresholds.addModal.scopeIdPlaceholder.', SCOPE_LEVELS);
    expectKeys('scope id noun', locales, 'thresholds.addModal.scopeIdNoun.', SCOPE_LEVELS);
    expectKeys('direction', locales, 'thresholds.direction.', DIRECTIONS);
  });

  it('every forwarding source and destination kind has a label (settings-forwarding)', () => {
    const locales = { en: enSettingsForwarding, ja: jaSettingsForwarding };
    expectKeys('source kind', locales, 'source.', FORWARD_SOURCE_KINDS);
    expectKeys('dest kind', locales, 'dest.', FORWARD_DEST_KINDS);
  });

  it('every device-profile category has a label (monitoring:categories.*)', () => {
    // PROFILE_CATEGORIES stores fully-qualified `monitoring:categories.x` keys; strip the namespace.
    const tokens = PROFILE_CATEGORIES.map((c) => c.labelKey.replace(/^monitoring:categories\./, ''));
    expectKeys('profile category', { en: enMonitoring, ja: jaMonitoring }, 'categories.', tokens);
  });

  it('every known scalar has a label (format:scalar.*)', () => {
    // `scalarDisplay` only falls back to the raw metric name for names NOT in this set. A name in
    // the set with no strings renders the literal key to the operator instead.
    expectKeys('scalar', { en: enFormat, ja: jaFormat }, 'scalar.', [...KNOWN_SCALARS]);
  });

  it('every Device-health metric card has a label (nodes:overview.*)', () => {
    // The card's label is now `t(spec.labelKey)` — a key read from the registry, so a card added
    // without its strings renders the raw key ("overview.gpuLoad") in both languages.
    const keys = METRIC_CARDS.map((c) => c.labelKey);
    expectKeys('metric card', { en: enNodes, ja: jaNodes }, '', keys);
  });

  it('every report run state has a badge label (reports:run.status.*)', () => {
    // RUN_STATUS holds namespace-relative keys (the METRIC_CARDS shape), so read them from the
    // registry rather than rebuilding the prefix here.
    const keys = REPORT_RUN_STATES.map((s) => RUN_STATUS[s].labelKey);
    expectKeys('run status', { en: enReports, ja: jaReports }, '', keys);
  });

  it('every report trigger and cadence has a label (reports)', () => {
    const locales = { en: enReports, ja: jaReports };
    expectKeys('trigger', locales, 'trigger.', REPORT_TRIGGERS);
    // CADENCE keys are fully qualified (`reports:cadence.x`) — strip the namespace.
    const cadenceKeys = REPORT_FREQUENCIES.map((f) =>
      CADENCE[f].labelKey.replace(/^reports:/, ''),
    );
    expectKeys('cadence', locales, '', cadenceKeys);
    // The schedule form's option labels, for the subset an operator may pick.
    expectKeys('freq option', locales, 'schedule.freq.', SELECTABLE_FREQUENCIES);
  });

  it('every event action and match kind has a label', () => {
    // Two surfaces render the same set from different namespaces, so both are checked. The event
    // log short-circuits `none` to "—" today, but the key exists so a future render of it — or a
    // sixth action — is not a raw key in the operator's face.
    expectKeys('event action', { en: enAlerts, ja: jaAlerts }, 'eventLog.action.', EVENT_ACTIONS);
    expectKeys(
      'event triage action',
      { en: enDashboard, ja: jaDashboard },
      'widgets.eventTriage.action.',
      EVENT_ACTIONS,
    );
    expectKeys(
      'match kind',
      { en: enAlertsConfig, ja: jaAlertsConfig },
      'eventRules.matchKind.',
      EVENT_MATCH_KINDS,
    );
  });

  it('every addable monitor kind has its three strings (nodes:add.*/err.*)', () => {
    // The select option, the modal title and the failure message are all read from the registry
    // now. A kind added without strings would put a raw key in the dropdown an operator picks from.
    const keys = MONITOR_KINDS.flatMap((k) => [k.optionKey, k.titleKey, k.errorKey]);
    expectKeys('monitor kind', { en: enNodes, ja: jaNodes }, '', keys);
  });
});
