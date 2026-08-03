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
  FORWARD_FILTER_MODES,
  FORWARD_SOURCE_KINDS,
  GROUP_TYPES,
  ROLES,
  SCOPE_LEVELS,
  SEVERITIES,
  CADENCES,
  REPORT_RUN_STATES,
  REPORT_TRIGGERS,
  EVENT_ACTIONS,
  EVENT_MATCH_KINDS,
  DNS_FAILURE_KINDS,
  ANALYSIS_SCHEDULE_STATUSES,
  FINDING_SEVERITIES,
  RCA_CONFIDENCES,
  TOKEN_SURFACES,
  USER_KINDS,
  BUNDLE_NOTE_CODES,
  NEIGHBOR_CAPABILITIES,
  NEIGHBOR_PROTOS,
} from './types/api';
import { SEVERITY_ORDER } from './lib/nodeState';
import { KNOWN_SCALARS } from './lib/format';
import { PROFILE_CATEGORIES } from './lib/profileCategories';
import { METRIC_CARDS } from './components/NodeDetail/metricCards';
import { MONITOR_KINDS } from './pages/monitorKinds';
import { RUN_STATUS } from './reports/runStatus';
import { CADENCE, SELECTABLE_CADENCES } from './lib/cadence';
import { FORWARD_FILTER_FIELDS, opsForField } from './pages/forwardingOptions';
import { TERMINAL_JOB_STATES } from './troubleshoot/data';
import { FINDING_RANGES } from './troubleshoot/findingsQuery';
import { HTTP_AUTH_SCHEMES } from './pages/httpAuthCredential';
import { BUNDLE_TABLES } from './pages/configBundle';
import { RETENTION_FIELDS, RETENTION_SUBJECTS } from './pages/retentionSettings';

import enAccess from './locales/en/access.json';
import enSettingsTokens from './locales/en/settings-tokens.json';
import jaSettingsTokens from './locales/ja/settings-tokens.json';
import jaAccess from './locales/ja/access.json';
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
import enRca from './locales/en/rca.json';
import jaRca from './locales/ja/rca.json';
import enAlertsConfig from './locales/en/alertsConfig.json';
import jaAlertsConfig from './locales/ja/alertsConfig.json';
import enSettingsForwarding from './locales/en/settings-forwarding.json';
import jaSettingsForwarding from './locales/ja/settings-forwarding.json';
import enTroubleshoot from './locales/en/troubleshoot.json';
import jaTroubleshoot from './locales/ja/troubleshoot.json';
import enSystem from './locales/en/system.json';
import jaSystem from './locales/ja/system.json';

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

  it('every HTTP auth scheme has a label (access:cred.http.schemeName.*)', () => {
    // The credential dialog builds the key from the runtime array, so a scheme added without
    // strings ships as a raw key in *both* locales — which parity passes and nobody notices.
    expectKeys(
      'http auth scheme',
      { en: enAccess, ja: jaAccess },
      'cred.http.schemeName.',
      HTTP_AUTH_SCHEMES,
    );
  });

  it('every forwarding source and destination kind has a label (settings-forwarding)', () => {
    const locales = { en: enSettingsForwarding, ja: jaSettingsForwarding };
    expectKeys('source kind', locales, 'source.', FORWARD_SOURCE_KINDS);
    expectKeys('dest kind', locales, 'dest.', FORWARD_DEST_KINDS);
  });

  it('every forwarding filter field, operator and mode has a label (settings-forwarding)', () => {
    // The filter builder renders `field.${f}` / `op.${op}` and the destinations table renders
    // `filter.mode.${mode}` — a FilterField variant added without strings shipped as a raw key
    // with nothing failing. (`valuePlaceholder.*` passes `defaultValue: ''` — deliberately
    // partial, so it is not demanded here.)
    const locales = { en: enSettingsForwarding, ja: jaSettingsForwarding };
    expectKeys('filter field', locales, 'field.', FORWARD_FILTER_FIELDS);
    const ops = [...new Set(FORWARD_FILTER_FIELDS.flatMap((f) => opsForField(f)))];
    expectKeys('filter op', locales, 'op.', ops);
    expectKeys('filter mode', locales, 'filter.mode.', FORWARD_FILTER_MODES);
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
    const cadenceKeys = CADENCES.map((f) =>
      CADENCE[f].labelKey.replace(/^reports:/, ''),
    );
    expectKeys('cadence', locales, '', cadenceKeys);
    // The schedule form's option labels, for the subset an operator may pick.
    expectKeys('freq option', locales, 'schedule.freq.', SELECTABLE_CADENCES);
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

  it('every DNS failure kind has a label (nodes:dns.failure.*)', () => {
    // Until these existed, DnsHealth rendered the raw token, so a Japanese operator read
    // "nx_domain" in the resolution column.
    expectKeys('dns failure', { en: enNodes, ja: jaNodes }, 'dns.failure.', DNS_FAILURE_KINDS);
  });

  it('every RCA confidence level has a label (rca:confidence.*)', () => {
    expectKeys('confidence', { en: enRca, ja: jaRca }, 'confidence.', RCA_CONFIDENCES);
  });

  it('every terminal analysis-job state has a label (troubleshoot:runs.state.*)', () => {
    // `AnalysisJob.state` is a bare string in the schema; the report shell and the runs list only
    // build `runs.state.${state}` for the terminal subset, so that subset is what must resolve.
    expectKeys(
      'terminal job state',
      { en: enTroubleshoot, ja: jaTroubleshoot },
      'runs.state.',
      TERMINAL_JOB_STATES,
    );
  });

  it('every analysis-schedule status has a label (troubleshoot:schedule.status.*)', () => {
    // The schedules table renders `schedule.status.${last_status}` from a Record; a status the
    // backend gains without strings would put a raw key in the table's outcome column.
    expectKeys(
      'analysis schedule status',
      { en: enTroubleshoot, ja: jaTroubleshoot },
      'schedule.status.',
      ANALYSIS_SCHEDULE_STATUSES,
    );
  });

  it('every finding severity has a label (troubleshoot:findings.severity.*)', () => {
    // `severity` is a bare string in the schema (Rust: `FINDING_SEVERITIES`), and the Saved-findings
    // screen builds both the column and the filter option from `findings.severity.${sev}` — so a
    // bucket added without strings would put a raw key in a dropdown an operator picks from.
    expectKeys(
      'finding severity',
      { en: enTroubleshoot, ja: jaTroubleshoot },
      'findings.severity.',
      FINDING_SEVERITIES,
    );
  });

  it('every findings time range has a label (troubleshoot:findings.range.*)', () => {
    expectKeys(
      'findings range',
      { en: enTroubleshoot, ja: jaTroubleshoot },
      'findings.range.',
      FINDING_RANGES,
    );
  });

  it('every addable monitor kind has its three strings (nodes:add.*/err.*)', () => {
    // The select option, the modal title and the failure message are all read from the registry
    // now. A kind added without strings would put a raw key in the dropdown an operator picks from.
    const keys = MONITOR_KINDS.flatMap((k) => [k.optionKey, k.titleKey, k.errorKey]);
    expectKeys('monitor kind', { en: enNodes, ja: jaNodes }, '', keys);
  });

  it('every token surface has a label and a hint (settings-tokens:surface.*)', () => {
    // The label names the surface in the list and the dialog; the hint is what tells an admin what
    // they are handing out. A surface added without either would offer an operator a raw key at the
    // exact moment they decide how much power a credential carries.
    const locales = { en: enSettingsTokens, ja: jaSettingsTokens };
    expectKeys('token surface', locales, 'surface.', TOKEN_SURFACES);
    expectKeys('token surface hint', locales, 'surfaceHint.', TOKEN_SURFACES);
  });

  it('every token state has an explanation (settings-tokens:state.*)', () => {
    // A token can be dead for four independent reasons and the operator needs the real one — "the
    // owner is disabled" and "this expired" call for different actions.
    expectKeys(
      'token state',
      { en: enSettingsTokens, ja: jaSettingsTokens },
      'state.',
      ['active', 'revoked', 'expired', 'no-owner', 'owner-disabled'],
    );
  });

  it('every account kind has a label (access:users.kind.*)', () => {
    // The users list badges each account by kind, and the add-user dialog offers the creatable
    // ones. A kind without strings shows an operator a raw key while they decide what an account is.
    expectKeys('user kind', { en: enAccess, ja: jaAccess }, 'users.kind.', USER_KINDS);
  });

  it('every account kind has a hint (access:users.kindHint.*)', () => {
    // The badge's tooltip is what tells an admin why an account has no password and who decides its
    // role — the question a `Directory` or `SSO` pill immediately raises. Same runtime-key exposure
    // as the label above, so it needs the same guard rather than relying on EN/JA parity.
    expectKeys('user kind hint', { en: enAccess, ja: jaAccess }, 'users.kindHint.', USER_KINDS);
  });

  it('every config-bundle section and note has strings (system:bundle.*)', () => {
    // Both are rendered from a value the server chose, and both would otherwise show the operator a
    // raw database identifier at exactly the moment they are deciding whether to overwrite their
    // configuration. The note codes matter most: each one is the only place the UI says what an
    // import silently left out.
    const locales = { en: enSystem, ja: jaSystem };
    expectKeys('bundle table', locales, 'bundle.table.', BUNDLE_TABLES);
    expectKeys('bundle note', locales, 'bundle.notes.', BUNDLE_NOTE_CODES);
  });

  it('every neighbor protocol and capability has strings (nodes:neighbors.*)', () => {
    // Both are rendered from a value the device supplied and the backend normalized. The
    // capability vocabulary is the whole point of that normalization — it exists so one legend
    // serves LLDP and CDP — so a missing string here would put a raw token like `wlan_ap` in the
    // one column that is supposed to be human-readable.
    const locales = { en: enNodes, ja: jaNodes };
    expectKeys('neighbor proto', locales, 'neighbors.proto.', NEIGHBOR_PROTOS);
    expectKeys('neighbor capability', locales, 'neighbors.capability.', NEIGHBOR_CAPABILITIES);
    // The empty state and the diff kinds are also built at runtime, and each says something an
    // operator would otherwise have to guess ("nothing recorded" vs "genuinely no neighbours").
    expectKeys('neighbor empty reason', locales, 'neighbors.empty.', [
      'disabled',
      'unrecorded',
      'none',
    ]);
    expectKeys('neighbor diff kind', locales, 'neighbors.diff.', ['added', 'removed', 'changed']);
  });

  it('every retention subject has strings (system:settings.retention.subject.*)', () => {
    // Pre-existing gap, closed alongside the subject this ADR adds: the retention card builds
    // `settings.retention.subject.${row.subject}` from whatever the server listed, with a
    // `defaultValue` fallback that renders the raw token. A new retained table therefore reached
    // both locales missing — parity passes, and the screen an operator opens to decide how long
    // data is kept shows them `neighbor_changes`.
    const locales = { en: enSystem, ja: jaSystem };
    expectKeys('retention subject', locales, 'settings.retention.subject.', RETENTION_SUBJECTS);
    expectKeys('retention field', locales, 'settings.retention.field.', RETENTION_FIELDS);
  });

  it('every scope label state has strings (access:users.scope.* and settings-tokens:scope.*)', () => {
    // `scopeLabelKey` picks one of three states at runtime, and both screens render it. The three
    // must be told apart in every locale: "All groups" and "No groups" are opposites, and reading
    // one as the other is the whole failure mode group scoping exists to prevent.
    //
    // `groups` is pluralized, so the stored keys carry i18next's suffixes — `_one`/`_other` in EN,
    // `_other` alone in JA, which has one plural form. Listed explicitly rather than derived,
    // because the suffix set is a property of the language, not of the union.
    for (const locales of [
      { prefix: 'users.scope.', en: enAccess, ja: jaAccess },
      { prefix: 'scope.', en: enSettingsTokens, ja: jaSettingsTokens },
    ]) {
      const { prefix, en, ja } = locales;
      expectKeys('scope label', { en, ja }, prefix, ['all', 'none', 'groups_other']);
      expectKeys('scope label (en plural)', { en, ja: en }, prefix, ['groups_one']);
    }
  });
});
