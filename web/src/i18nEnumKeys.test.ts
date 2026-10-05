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
//
// Not every runtime-built key belongs here, and the exclusions are deliberate rather than missed.
// **Three families pass a `defaultValue` and are therefore allowed to be partial:**
// `settings-forwarding:valuePlaceholder.*` (falls back to ''), `settings-auth:ldap.stage.*` (falls
// back to the stage name the server sent — an unbounded set), and
// `dashboard:widgets.eventFeed.*` (falls back to the row's own key). A family with a fallback
// degrades to something readable; a family without one renders `dest.kafka` at the operator. That
// is the line: if you add a runtime-built key with no `defaultValue`, it needs a case below.

import { describe, expect, it } from 'vitest';
import {
  AUDIT_ACTIONS,
  AUDIT_STATUS_CLASSES,
  DELIVERY_EVENTS,
  DELIVERY_RESULTS,
  DELIVERY_SIDES,
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
  WLAN_AP_STATES,
  LINK_SOURCES,
  MAP_ENDPOINT_KINDS,
  MAP_ROLES,
  MAP_ROLE_REASONS,
  TOPOLOGY_MODES,
  TLS_CERT_SOURCES,
  METRIC_STATUSES,
  METRIC_DIMENSIONS,
  NODE_KINDS,
  UPGRADE_PROGRESS_COMMANDS,
  SITE_ID_BUILT_INS,
  DUPLICATE_CONFIDENCES,
  DUPLICATE_CONTRADICTIONS,
  DUPLICATE_EVIDENCE_KINDS,
  OVERLAP_KINDS,
  OVERLAP_STATUSES,
  OVERLAP_HINT_KINDS,
  EXCLUSION_REASONS,
  MERAKI_LISTINGS,
  CLAIM_PORT_STATES,
  CHANNEL_KINDS,
} from './types/api';
import { NODE_KIND_SPEC } from './lib/nodeKind';
import {
  NODE_EDIT_KIND_SPEC,
  NODE_EDIT_SECTIONS,
} from './components/NodeDetail/nodeEditForm';
import { CREDENTIAL_KINDS } from './lib/credentialKinds';
import { TEXT_MODES } from './lib/columnFilter';
import { EXPLAINED_METRICS, OVERVIEW_FAMILIES } from './lib/metricMeaning';
import metricUnits from './api/metricUnits.json';
import { WEEKDAY_KEYS } from './lib/cadence';
import { BACKINGS } from './dashboard/types';
import { GEO_PROBLEMS } from './components/GroupModal/geoFields';
import { PREFIX_PROBLEMS } from './components/GroupModal/prefixFields';
import { AP_IMPORT_STATES } from './components/NodeDetail/tabFilters';
import {
  NEIGHBOR_ADDRESS_STATES,
  NEIGHBOR_DETAIL_KEYS,
  SETUP_BLOCKED_REASONS,
} from './components/NodeDetail/neighbors';
import { CHECK_FORM_PROBLEMS } from './components/NodeDetail/checkConfigForm';
import { LABEL_PROBLEMS } from './components/ui/labelRules';
import { AI_FORM_PROBLEMS } from './pages/aiConfigForm';
import { CREATABLE_USER_KINDS } from './pages/userKinds';
import { LDAP_FORM_PROBLEMS } from './pages/ldapConfigForm';
import { BUNDLE_IMPORT_REASONS, bundleImportErrorKey } from './pages/configBundle';
import { IMPORT_BLOCKS } from './pages/tlsSettingsForm';
import { MAINTENANCE_STATUSES } from './pages/maintenanceStatus';
import { SCHEDULE_FORM_PROBLEMS } from './troubleshoot/scheduleForm';
import { ANALYSIS_WINDOWS } from './troubleshoot/analysisDefaults';
import {
  CORRELATION_DIRECTIONS,
  FLAP_BUCKETS,
  SCAN_PATTERNS,
  TIMELINE_LANES,
  TTE_UNITS,
} from './troubleshoot/report/format';
import { DIFF_VERDICTS } from './pages/topologyDiff';
import { SKIES } from './pages/geoDayNight';
import { MERAKI_TIERS } from './pages/merakiTiers';
import { PREVIEW_SAMPLES, TEMPLATE_EVENTS, UNSUPPORTED_REASONS } from './pages/templateModel';
import { TEMPLATE_PRESETS } from './pages/templatePresets';
import { BUILTIN_JSON_KEYS, TEMPLATE_FORMS } from './pages/templateForm';
import { TEMPLATE_VARIABLE_GROUPS, TEMPLATE_VARIABLE_NAMES } from './pages/templateVariables';
import { MERAKI_UPLINK_STATES } from './components/NodeDetail/merakiCard';
import { MERAKI_REGION_KEYS } from './pages/integrations/merakiRegions';
import { DISCOVERY_WALKS } from './pages/neighborSettings';
import {
  ENDPOINT_COVERAGE,
  ENDPOINT_DEST_LINES,
  ENDPOINT_SOURCES,
} from './pages/discoveredEndpoints';
import { UNSWEEPABLE_REASONS } from './pages/siteTargets';
import { DESTINATION_KINDS } from './pages/importFiling';
import {
  GROUP_ORIGINS,
  MERAKI_DEVICE_STATES,
  MERAKI_FILING_REASONS,
  MERAKI_HA_ROLES,
  MERAKI_PAIR_STATES,
  MERAKI_SYNC_FAILURES,
  NEIGHBOR_PEER_STATES,
  PREFIX_SOURCES,
} from './types/api';
import { SEVERITY_ORDER } from './lib/nodeState';
import { KNOWN_SCALARS } from './lib/format';
import { PROFILE_CATEGORIES } from './lib/profileCategories';
import { METRIC_CARDS } from './components/NodeDetail/metricCards';
import { PREFIX_GAP_KINDS } from './components/NodeDetail/prefixGaps';
import { MISSING_PREFIX_VIEWS, SITE_GAP_STATUSES } from './pages/missingPrefixes';
import { FAULT_SERIES, OPTICAL_SERIES } from './components/NodeDetail/interfaceMetrics';
import { DUPLEX_STATES, SPEED_TIERS } from './components/NodeDetail/linkMode';
import { MONITOR_KINDS } from './pages/monitorKinds';
import { ADD_MENU_LABEL_KEYS } from './pages/nodesAddMenu';
import { CAUSE_LABEL_KEYS, PANEL_LABEL_KEYS } from './lib/suppression';
import { RUN_STATUS } from './reports/runStatus';
import { SCAN_STATE_SPECS } from './pages/discoveryScans';
import { CADENCE, SELECTABLE_CADENCES } from './lib/cadence';
import { FORWARD_FILTER_FIELDS, opsForField } from './pages/forwardingOptions';
import { TERMINAL_JOB_STATES, TOOL_GROUPS } from './troubleshoot/data';
import { HTTP_AUTH_SCHEMES } from './pages/httpAuthCredential';
import { BUNDLE_TABLES } from './pages/configBundle';
import { RETENTION_FIELDS, RETENTION_SUBJECTS } from './pages/retentionSettings';
import {
  BODY_MATCH_MODES,
  EXPECTED_STATUS_MODES,
} from './components/NodeDetail/checkConfigForm';
import { EXPIRY_CHOICES, TOKEN_STATE_INFO, TOKEN_STATES } from './pages/tokenForm';
import { EXPIRY_LEVELS } from './pages/tlsSettingsForm';
import { OIDC_ISSUER_PARAMS, OIDC_PICKER_ORDER, OIDC_PRESETS } from './pages/oidcPresets';

import enAccess from './locales/en/access.json';
import enSettingsTokens from './locales/en/settings-tokens.json';
import jaSettingsTokens from './locales/ja/settings-tokens.json';
import enSettingsTls from './locales/en/settings-tls.json';
import jaSettingsTls from './locales/ja/settings-tls.json';
import enSettingsRelocation from './locales/en/settings-relocation.json';
import jaSettingsRelocation from './locales/ja/settings-relocation.json';
import enSettingsUpgrade from './locales/en/settings-upgrade.json';
import jaSettingsUpgrade from './locales/ja/settings-upgrade.json';
import {
  COMPONENT_REASONS,
  CONVERGE_STATES,
  MECHANISM_KEYS,
  MECHANISMS,
  UPGRADE_BUILD_KIND_HINTS,
  UPGRADE_BUILD_KINDS,
  UPGRADE_OFFER_BLOCKS,
  UPGRADE_OFFER_DIRECTIONS,
  UPGRADE_RUN_STATES,
  UPGRADE_RUN_STEPS,
} from './pages/upgradeStatus';
import {
  RELOCATION_AUTH_KINDS,
  RELOCATION_READINESS,
  RELOCATION_STAGES,
} from './pages/relocationStatus';
import enSettingsAuth from './locales/en/settings-auth.json';
import jaSettingsAuth from './locales/ja/settings-auth.json';
import enSettingsAi from './locales/en/settings-ai.json';
import jaSettingsAi from './locales/ja/settings-ai.json';
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
import enMetricMeanings from './locales/en/metricMeanings.json';
import jaMetricMeanings from './locales/ja/metricMeanings.json';
import enSettingsForwarding from './locales/en/settings-forwarding.json';
import jaSettingsForwarding from './locales/ja/settings-forwarding.json';
import enTroubleshoot from './locales/en/troubleshoot.json';
import jaTroubleshoot from './locales/ja/troubleshoot.json';
import enSystem from './locales/en/system.json';
import jaSystem from './locales/ja/system.json';
import enTopology from './locales/en/topology.json';
import jaTopology from './locales/ja/topology.json';
import enSuppression from './locales/en/suppression.json';
import jaSuppression from './locales/ja/suppression.json';

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

  it('every role has a label (common:role.* and access:role.*)', () => {
    // Two namespaces, two independent copies of the same three strings: badges elsewhere read
    // `common:role.*`, while the Users screen — the role picker, the read-only role cell and the
    // add-user dialog — is mounted on `access` and builds `role.${r}` there. Pinning only `common`
    // left the screen that *assigns* roles free to render `role.auditor` at a raw key.
    expectKeys('role', { en: enCommon, ja: jaCommon }, 'role.', ROLES);
    expectKeys('role (access)', { en: enAccess, ja: jaAccess }, 'role.', ROLES);
  });

  it('every text-filter mode has a label (common:filter.mode.*)', () => {
    // `TextConditionEditor` builds `filter.mode.${m}` from the runtime array, so a third mode added
    // to `TEXT_MODES` without its strings would render a raw key inside every column filter on
    // every list — in BOTH locales, which parity passes.
    expectKeys('text filter mode', { en: enCommon, ja: jaCommon }, 'filter.mode.', TEXT_MODES);
  });

  it('every node-group type has a label (nodes:groupType.*)', () => {
    expectKeys('group type', { en: enNodes, ja: jaNodes }, 'groupType.', GROUP_TYPES);
  });

  it('every Overview family has a section heading (nodes:overview.family.*)', () => {
    // `OverviewSections` builds `overview.family.${family}` from the generated catalog's token
    // (ADR-046 Inc.8), so a sixth family added in Rust without its strings would head a section
    // with a raw key — in both locales, which parity passes.
    expectKeys('overview family', { en: enNodes, ja: jaNodes }, 'overview.family.', OVERVIEW_FAMILIES);
  });

  it('every threshold scope level and direction has a label (alertsConfig)', () => {
    const locales = { en: enAlertsConfig, ja: jaAlertsConfig };
    expectKeys('scope level', locales, 'thresholds.scopeLevel.', SCOPE_LEVELS);
    // The placeholder is checked over a *subset*, derived rather than listed: the scope-id input
    // is not rendered for `global` (a fleet-wide rule has nothing to point at), so demanding a
    // string there would demand one nobody can ever see — and an unread string is what drifts.
    // `interface` has no input either: its target is shown, not edited (ThresholdModal).
    // (The per-level `scopeIdNoun` family went with the hint it filled, ADR-200 Inc.14.)
    const WITH_SCOPE_ID = SCOPE_LEVELS.filter((l) => l !== 'global' && l !== 'interface');
    expectKeys('scope id placeholder', locales, 'thresholds.addModal.scopeIdPlaceholder.', WITH_SCOPE_ID);
    expectKeys('direction', locales, 'thresholds.direction.', DIRECTIONS);
  });

  it('every explained metric has a meaning (metricMeanings:*)', () => {
    // The key is built at runtime from the metric name, so a member of `EXPLAINED_METRICS` with
    // no strings renders a raw key. Two screens read it: the column an operator uses to find out
    // what a rule is watching, and the picker they read *before* choosing. A raw key in either is
    // worse than no text at all.
    //
    // ⚠️ **What this test now checks is the Japanese half.** Since ADR-079 the sentences are
    // owned by `crates/yagra-core/src/metric_meaning.rs` and the English file is generated from
    // it, so `EXPLAINED_METRICS` *is* the English key list and that side cannot be short. The
    // failure it still catches is the one parity never could: a metric explained in Rust and
    // never translated, which would render the English sentence in a Japanese UI (i18next falls
    // back) — readable, but not what the operator asked for.
    expectKeys(
      'metric meaning',
      { en: enMetricMeanings, ja: jaMetricMeanings },
      '',
      EXPLAINED_METRICS,
    );
  });

  it('every credential kind has a label (access:cred.kind.*)', () => {
    // The label names the secret in the list, the create dialog and every picker that filters on
    // these kinds; a kind with no strings would offer the operator a raw key while they choose
    // what to store.
    expectKeys('credential kind', { en: enAccess, ja: jaAccess }, 'cred.kind.', CREDENTIAL_KINDS);
  });

  it('every alert-history phase has a label (alerts:history.phase.*)', () => {
    // Built at runtime from the `as const` arrays, so a range or phase added without strings ships
    // as a raw key in *both* locales — which parity passes and nobody notices until an operator is
    // staring at `history.phase.x` in a filter. (The ranges are `common:filter.range.*` since
    // ADR-184 — `filterPresets.test.ts` holds every window to a label.)
    const locales = { en: enAlerts, ja: jaAlerts };
    // The phase filter's two options and the row badge read the same keys, so one miss shows twice.
    expectKeys('history phase', locales, 'history.phase.', ['fired', 'cleared'] as const);
  });

  it('every audit action and status class has a label (access:audit.*)', () => {
    // The toolbar builds `audit.action.${a}` / `audit.statusClass.${s}` from the arrays pinned to
    // the Rust enums, so a variant added there ships as a raw key in *both* locales — which parity
    // passes and nobody notices until an operator is staring at `audit.action.head` in a filter.
    const locales = { en: enAccess, ja: jaAccess };
    expectKeys('audit action', locales, 'audit.action.', AUDIT_ACTIONS);
    expectKeys('audit status class', locales, 'audit.statusClass.', AUDIT_STATUS_CLASSES);
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

  it('every counted unit has a noun (format:unit.*)', () => {
    // The nouns come from `api/metricUnits.json`, which is generated from Rust (ADR-046 Inc.7), so
    // a `MetricUnit::Counted("frames")` added there fails here until someone writes the word in
    // both languages. ⚠️ EN⟷JA parity alone cannot catch that: a new noun is missing from *both*
    // files, so parity passes while the card renders `format:unit.frames` beside the number.
    const nouns = [...new Set(Object.values(metricUnits.counted))];
    expect(nouns.length).toBeGreaterThanOrEqual(14);
    expectKeys('counted unit', { en: enFormat, ja: jaFormat }, 'unit.', nouns);
  });

  it('every Device-health metric card has a label (nodes:overview.*)', () => {
    // The card's label is now `t(spec.labelKey)` — a key read from the registry, so a card added
    // without its strings renders the raw key ("overview.gpuLoad") in both languages.
    const keys = METRIC_CARDS.map((c) => c.labelKey);
    expectKeys('metric card', { en: enNodes, ja: jaNodes }, '', keys);
  });

  it('every interface fault line has a label (nodes:interfaces.*)', () => {
    // The errors/discards chart labels its four lines from the registry, so a line added without
    // its strings renders the raw key ("interfaces.discOut") in the one legend that says which
    // colour is which — leaving four indistinguishable lines with no key.
    const keys = FAULT_SERIES.map((s) => s.labelKey);
    expectKeys('fault series', { en: enNodes, ja: jaNodes }, '', keys);
  });

  it('every optical power line has a label (nodes:interfaces.*)', () => {
    // Same hazard as the fault lines, and one degree worse: the optical chart's two lines are the
    // ONLY thing distinguishing receive from transmit. A raw key in that legend leaves two
    // same-shaped lines with nothing to tell them apart.
    const keys = OPTICAL_SERIES.map((s) => s.labelKey);
    expectKeys('optical series', { en: enNodes, ja: jaNodes }, '', keys);
  });

  it('every speed tier and duplex bucket has a label (nodes:interfaces.*)', () => {
    // Three runtime-built families with no `defaultValue`, all rendered where a raw key would be
    // most confusing: `speed.*` and `duplex.*` name the options inside a filter dropdown, so a
    // missing string offers the operator a choice spelled "interfaces.speed.2.5g"; `duplexEmpty.*`
    // is the tooltip that explains why a cell is blank, where a raw key answers the question with
    // another question.
    const locales = { en: enNodes, ja: jaNodes };
    expectKeys('speed tier', locales, 'interfaces.speed.', SPEED_TIERS);
    expectKeys('duplex bucket', locales, 'interfaces.duplex.', DUPLEX_STATES);
    // Not a union — the two reasons `duplexEmptyReason` can return, listed because they are built
    // into a key the same way and share the same failure.
    expectKeys('duplex empty reason', locales, 'interfaces.duplexEmpty.', [
      'unknown',
      'notApplicable',
    ]);
    expectKeys('media empty reason', locales, 'interfaces.mediaEmpty.', [
      'unknown',
      'notApplicable',
    ]);
  });

  it('every discovery scan state has a badge label (monitoring:discovery.scans.state.*)', () => {
    // `t(spec.labelKey)` is built at runtime from the registry, so neither tsc nor the EN⟷JA parity
    // gate can see a missing string: a state absent from *both* locales renders as its raw key.
    // Covers `unknown` too — that one is this side's invention and has no backend variant to
    // remind anyone it exists.
    const keys = (['running', 'cancelling', 'cancelled', 'done', 'unknown'] as const).map(
      (s) => SCAN_STATE_SPECS[s].labelKey,
    );
    expectKeys('discovery scan state', { en: enMonitoring, ja: jaMonitoring }, '', keys);
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

  it('every LLM provider has a label (settings-ai:provider.*)', () => {
    // Mirrors `ProviderKind` in `crates/yagra-core/src/rca/mod.rs` (the enum `rca/store.rs` parses
    // its stored `provider` column through). The Settings ▸ AI picker renders one option per entry
    // of the *backend-fetched* provider list — `t(`provider.${p.key}`)` — so a fourth vendor would
    // arrive as an option labelled `provider.openai`, in both locales, at the moment an admin is
    // choosing where incident context is sent. Listed here rather than iterated because the choice
    // list is fetched at runtime and there is no `as const` array on the TS side to walk.
    expectKeys(
      'llm provider',
      { en: enSettingsAi, ja: jaSettingsAi },
      'provider.',
      ['vertex', 'gemini', 'claude'],
    );
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

  it('every Troubleshoot tool group has a heading (troubleshoot:catalog.group.*)', () => {
    // The catalog builds `catalog.group.${group}` while iterating TOOL_GROUPS (ADR-055 Inc.5), so a
    // group added without strings renders its own key as a heading — and does so in BOTH locales,
    // which is exactly what parity cannot see.
    expectKeys(
      'tool group',
      { en: enTroubleshoot, ja: jaTroubleshoot },
      'catalog.group.',
      TOOL_GROUPS,
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

  it('every analysis window has a label (troubleshoot:launch.windows.*)', () => {
    // The launch drawer, the schedule editor and the quick run all render `t(w.labelKey)` from
    // the one list in `analysisDefaults.ts` (ADR-184 increment 6), so a window added there
    // without its strings would put a raw key on a button in BOTH locales, which parity passes.
    expectKeys(
      'analysis window',
      { en: enTroubleshoot, ja: jaTroubleshoot },
      '',
      ANALYSIS_WINDOWS.map((w) => w.labelKey),
    );
  });

  it('every addable monitor kind has its three strings (nodes:add.*/err.*)', () => {
    // The select option, the modal title and the failure message are all read from the registry
    // now. A kind added without strings would put a raw key in the dropdown an operator picks from.
    const keys = MONITOR_KINDS.flatMap((k) => [k.optionKey, k.titleKey, k.errorKey]);
    expectKeys('monitor kind', { en: enNodes, ja: jaNodes }, '', keys);
  });

  it('every reason a site range cannot be swept has a sentence (monitoring:discovery.site.cannot.*)', () => {
    // The picker builds this key at runtime from the row's reason, and `t()` is not typed against
    // a key union — so a third reason added to `prefixRows` without strings would render the raw
    // key in the one place that explains why a range has no checkbox. EN/JA parity cannot catch
    // it: a key missing from both locales is "in parity".
    expectKeys(
      'unsweepable reason',
      { en: enMonitoring, ja: jaMonitoring },
      'discovery.site.cannot.',
      [...UNSWEEPABLE_REASONS],
    );
  });

  it('every import destination has a sentence (monitoring:discovery.dest.why.*)', () => {
    // The Folder column builds this key at runtime from the server's answer (ADR-131). A fourth
    // answer added without strings would render a raw key in the cell that says where a device is
    // about to land — and EN/JA parity cannot catch it, because a key missing from both is "in
    // parity". `noRanges` is in the list but not in DESTINATION_KINDS: it is the deployment-wide
    // case (no folder has a range at all), not one of the three per-address answers.
    // ADR-179 Inc.8: the sentence over Monitor saying where an endpoint import lands.
    expectKeys(
      'endpoint import destination',
      { en: enMonitoring, ja: jaMonitoring },
      'discovery.seen.dest.line.',
      ENDPOINT_DEST_LINES,
    );
    expectKeys(
      'import destination',
      { en: enMonitoring, ja: jaMonitoring },
      'discovery.dest.why.',
      [...DESTINATION_KINDS, 'noRanges', 'wouldMatch'],
    );
  });

  it('every prefix source has a label (nodes:group.prefixSource.*)', () => {
    // The range editor labels a row by who owns it, keyed at runtime. A third source would
    // otherwise show a raw key beside a control the operator cannot use.
    expectKeys(
      'prefix source',
      { en: enNodes, ja: jaNodes },
      'group.prefixSource.',
      [...PREFIX_SOURCES],
    );
  });

  it('every prefix-gap kind has a label (nodes:prefixGaps.kind.*)', () => {
    // The folder pane falls back to the bare kind when a range is withheld by scope (ADR-170).
    expectKeys('prefix-gap kind', { en: enNodes, ja: jaNodes }, 'prefixGaps.kind.', [
      ...PREFIX_GAP_KINDS,
    ]);
  });

  it('every ＋-menu label resolves (nodes:tree.*/addMenu.*)', () => {
    // The inventory + picks its two labels at runtime from the tree selection, and `t()` is not
    // typed against a key union — so a typo or a half-added key would render raw text on the one
    // control that creates nodes. EN/JA parity cannot catch it: a key missing from both is "in
    // parity".
    expectKeys('add menu label', { en: enNodes, ja: jaNodes }, '', ADD_MENU_LABEL_KEYS);
  });

  it('every suppression-cause label resolves (nodes:tree.suppression.from.*)', () => {
    // `causesFor` picks the "where does this come from" line at runtime, and it is the sentence
    // that tells an operator whether releasing acts on this row or on a group above it. A raw key
    // there would leave the panel's buttons unexplained. `suppression.test.ts` pins the produced
    // set to this list from the other side.
    expectKeys('suppression cause label', { en: enNodes, ja: jaNodes }, '', CAUSE_LABEL_KEYS);
  });

  it('every suppression-panel heading, button and note resolves (nodes:tree.suppression.*)', () => {
    // The panel picks its heading and its one control per block at runtime, from what is actually
    // silencing the row. A raw key here is on the button that changes alerting — and it is the
    // wording that says which of "end this window", "take this node out" and "put it back" the
    // click will do. `suppression.test.ts` pins the produced set to this list from the other side.
    expectKeys('suppression panel label', { en: enNodes, ja: jaNodes }, '', PANEL_LABEL_KEYS);
  });

  it('every node kind has an edit-dialog title and profile label (nodes:*)', () => {
    // Both are read out of NODE_EDIT_KIND_SPEC at render time, so a fifth kind added without its
    // strings would put a raw key in the dialog's own header.
    const keys = NODE_KINDS.flatMap((k) => [
      NODE_EDIT_KIND_SPEC[k].titleKey,
      NODE_EDIT_KIND_SPEC[k].profileLabelKey,
    ]);
    expectKeys('node edit dialog', { en: enNodes, ja: jaNodes }, '', keys);
  });

  it('every node-edit section has a heading (nodes:editNode.section.*)', () => {
    expectKeys(
      'node edit section',
      { en: enNodes, ja: jaNodes },
      'editNode.section.',
      NODE_EDIT_SECTIONS,
    );
  });

  it('every node kind has a label (nodes:kind.*)', () => {
    // The label is the badge's tooltip — the only place the badge's two or three letters are spelled
    // out. This is the *display* set, so unlike MONITOR_KINDS above it must cover `meraki` too.
    const keys = NODE_KINDS.map((k) => NODE_KIND_SPEC[k].labelKey);
    expectKeys('node kind', { en: enNodes, ja: jaNodes }, '', keys);
  });

  it('every token surface has a label and a hint (settings-tokens:surface.*)', () => {
    // The label names the surface in the list and the dialog; the hint — a few words beside the
    // checkbox since ADR-200 — is what tells an admin what they are handing out. A surface added
    // without either would offer an operator a raw key at the exact moment they decide how much
    // power a credential carries.
    const locales = { en: enSettingsTokens, ja: jaSettingsTokens };
    expectKeys('token surface', locales, 'surface.', TOKEN_SURFACES);
    expectKeys('token surface hint', locales, 'surfaceHint.', TOKEN_SURFACES);
  });

  it('every upgrade half has a badge label (system:pollers.upgradeStep.*)', () => {
    // The badge is the only thing on screen while a site pulls an image over a WAN link, which is
    // minutes (ADR-051 Inc.4). A variant with no strings would put a raw key there at the one
    // moment an operator is checking whether the upgrade they started is moving at all.
    expectKeys(
      'upgrade progress',
      { en: enSystem, ja: jaSystem },
      'pollers.upgradeStep.',
      UPGRADE_PROGRESS_COMMANDS,
    );
  });

  it('every IdP product has a name and setup steps (settings-auth:idp.* / idpSteps.*)', () => {
    // The steps are the whole increment: they are where "turn on the groups claim in Entra" and
    // "a custom Okta authorization server is Other" are told to the operator. A product added
    // without them is a picker entry that offers no more help than the free-text form it replaced.
    const locales = { en: enSettingsAuth, ja: jaSettingsAuth };
    expectKeys('IdP product', locales, 'idp.', OIDC_PICKER_ORDER);
    expectKeys(
      'IdP setup step',
      locales,
      '',
      OIDC_PICKER_ORDER.flatMap((k) => [...OIDC_PRESETS[k].setupSteps]),
    );
  });

  it('every product-specific issuer field is labelled (settings-auth:field.*)', () => {
    // A product whose issuer is built from one field needs a label and a placeholder for it — the
    // operator is being asked for a tenant id or an Okta domain, not for a URL. The rule for what
    // goes in it is said by the error line, only when broken (ADR-200).
    const keys = OIDC_ISSUER_PARAMS.flatMap((p) => [p, `${p}Placeholder`]);
    expectKeys(
      'issuer field',
      { en: enSettingsAuth, ja: jaSettingsAuth },
      'field.',
      keys,
    );
  });

  it('every token state has a label, and its explanation (settings-tokens:state.* / stateInfo.*)', () => {
    // A token can be dead for four independent reasons and the operator needs the real one — "the
    // owner is disabled" and "this expired" call for different actions.
    const locales = { en: enSettingsTokens, ja: jaSettingsTokens };
    expectKeys('token state', locales, 'state.', TOKEN_STATES);
    // What the badge opens when pressed (ADR-200), the only place the UI says *why* a token stopped
    // authenticating. `TOKEN_STATE_INFO` is a `Record`, so a new state has to answer whether it has
    // one; the states that do must have the strings, or the press opens a raw key.
    const info = TOKEN_STATES.flatMap((s) => {
      const key = TOKEN_STATE_INFO[s];
      return key ? [key.replace(/^settings-tokens:/, '')] : [];
    });
    expect(info.length).toBeGreaterThan(0);
    expectKeys('token state info', locales, '', info);
  });

  it('every expected-status mode has a label (nodes:checkEdit.statusMode.*)', () => {
    // `expected_status_mode` is a backend field: the HTTP check's status matcher. The dialog
    // renders `checkEdit.statusMode.${m}` from the runtime array, so a mode added on the Rust side
    // and mirrored here without strings ships as a raw key in both locales.
    expectKeys(
      'expected status mode',
      { en: enNodes, ja: jaNodes },
      'checkEdit.statusMode.',
      EXPECTED_STATUS_MODES,
    );
  });

  it('every body-match mode has a label (nodes:checkEdit.bodyMode.*)', () => {
    // Mirrors Rust's `BodyMatchMode` (ADR-047 Inc.2). Same failure as above and the reason this
    // file exists: EN/JA parity passes when a variant is missing from *both*, so only iterating
    // the runtime array catches a mode that renders as `checkEdit.bodyMode.regex`.
    expectKeys(
      'body match mode',
      { en: enNodes, ja: jaNodes },
      'checkEdit.bodyMode.',
      BODY_MATCH_MODES,
    );
  });

  it('every metric status has a label (nodes:collection.status.*)', () => {
    // Mirrors Rust's `MetricStatus` (ADR-046). The three statuses are the whole reason the metric
    // inventory joins two sources — a variant rendering as `collection.status.stale` would leave
    // the operator unable to tell "not configured" from "configured but silent", which is exactly
    // the ambiguity the join exists to remove.
    expectKeys(
      'metric status',
      { en: enNodes, ja: jaNodes },
      'collection.status.',
      METRIC_STATUSES,
    );
  });

  it('every metric dimension has a label (nodes:collection.dimension.*)', () => {
    // Mirrors Rust's `MetricDimension`. This label is what tells an operator that a per-row metric
    // is being shown as a node maximum rather than as the row they were looking for.
    expectKeys(
      'metric dimension',
      { en: enNodes, ja: jaNodes },
      'collection.dimension.',
      METRIC_DIMENSIONS,
    );
  });

  it('every token expiry choice has a label (settings-tokens:expiry.*)', () => {
    expectKeys(
      'token expiry choice',
      { en: enSettingsTokens, ja: jaSettingsTokens },
      'expiry.',
      EXPIRY_CHOICES,
    );
  });

  it('every certificate expiry level has a label (settings-tls:expiry.*)', () => {
    // Same `expiry.` leaf as the token page, but a different namespace and a different set —
    // pinning only one of them would leave the other free to drift.
    expectKeys(
      'certificate expiry level',
      { en: enSettingsTls, ja: jaSettingsTls },
      'expiry.',
      EXPIRY_LEVELS,
    );
  });

  it('every certificate source has a label and a hint (settings-tls:source*.*)', () => {
    // The TLS page badges the certificate by where it came from, and the hint beside it is what
    // says whether Yagra may replace it on its own — the difference between "this renews itself"
    // and "nothing will touch this but you". Both keys are built from the value at runtime, so a
    // source added later would render raw in BOTH locales and parity would still pass.
    const locales = { en: enSettingsTls, ja: jaSettingsTls };
    expectKeys('certificate source', locales, 'source.', TLS_CERT_SOURCES);
    expectKeys('certificate source hint', locales, 'sourceHint.', TLS_CERT_SOURCES);
  });

  it('every upgrade run state has a label (settings-upgrade:runState.*)', () => {
    // The Upgrade page renders the updater's own run state, and it is the screen an operator is
    // staring at while their monitoring is down. A state added to the sidecar without strings would
    // render "runState.quiesced" at exactly that moment, in both locales, with parity passing.
    expectKeys(
      'upgrade run state',
      { en: enSettingsUpgrade, ja: jaSettingsUpgrade },
      'runState.',
      UPGRADE_RUN_STATES,
    );
    // And the build kinds beside them, which are what separate a release from a flash build.
    expectKeys(
      'upgrade build kind',
      { en: enSettingsUpgrade, ja: jaSettingsUpgrade },
      'buildKind.',
      UPGRADE_BUILD_KINDS,
    );
    // Only the kinds that carry a hint: a release's label says all its hint did (ADR-200).
    expectKeys(
      'upgrade build kind hint',
      { en: enSettingsUpgrade, ja: jaSettingsUpgrade },
      'buildKindHint.',
      UPGRADE_BUILD_KIND_HINTS,
    );
    // Direction is the one this page got wrong in front of an operator: every button read
    // "upgrade to this" while offering versions older than the running one. Four key families are
    // keyed off it now, and a missing one would put a raw key on the button that replaces a
    // production deployment.
    for (const prefix of ['offerAction.']) {
      expectKeys(
        `upgrade offer direction (${prefix})`,
        { en: enSettingsUpgrade, ja: jaSettingsUpgrade },
        prefix,
        UPGRADE_OFFER_DIRECTIONS,
      );
    }
    // Why a component cannot move, or moves differently. Rendered in the row rather than in a
    // tooltip, so a missing key is a raw string an operator reads rather than one they hover.
    expectKeys(
      'upgrade component reason',
      { en: enSettingsUpgrade, ja: jaSettingsUpgrade },
      'componentReason.',
      COMPONENT_REASONS,
    );
    // Where a site has got to. A core newer than this bundle can name a state this build has never
    // heard of — `convergeState()` catches that — but a state this build *declares* and has no
    // string for would render as a raw key with EN/JA parity still passing.
    expectKeys(
      'poller convergence state',
      { en: enSettingsUpgrade, ja: jaSettingsUpgrade },
      'convergeState.',
      CONVERGE_STATES,
    );
    expectKeys(
      'upgrade offer block',
      { en: enSettingsUpgrade, ja: jaSettingsUpgrade },
      'offerBlock.',
      UPGRADE_OFFER_BLOCKS,
    );
    // The step is what the progress bar names while an operator watches their monitoring go down,
    // and until it had strings the page printed the sidecar's own shell token — `backup`, `pull` —
    // untranslated, in both locales, with parity passing. A phase added to the sidecar without
    // strings would do it again.
    expectKeys(
      'upgrade run step',
      { en: enSettingsUpgrade, ja: jaSettingsUpgrade },
      'runStep.',
      UPGRADE_RUN_STEPS,
    );
    // The state of the mechanism itself, which until now had no coverage at all: `Mechanism` was a
    // bare union, so a state could be added with no strings in either locale and nothing would
    // fail — the page would simply render nothing where the explanation goes. The stems are not
    // the state names (`absent` renders under `mechanism.disabled`), so this iterates the map
    // rather than the union.
    expectKeys(
      'upgrade mechanism state',
      { en: enSettingsUpgrade, ja: jaSettingsUpgrade },
      'mechanism.',
      MECHANISMS.map((m) => MECHANISM_KEYS[m]),
    );
  });

  it('every relocation token has a label (settings-relocation:*)', () => {
    // The same exposure as the upgrade page's, on a screen an operator is watching while their
    // whole deployment is being copied to another host. Every one of these keys is built from a
    // value at runtime, so a stage or a state added to the sidecar without strings would render a
    // raw shell token — `tier2`, `preflight` — in both locales, with EN/JA parity passing.
    const locales = { en: enSettingsRelocation, ja: jaSettingsRelocation };
    expectKeys('relocation stage', locales, 'stage.', RELOCATION_STAGES);
    expectKeys('relocation readiness', locales, 'readiness.', RELOCATION_READINESS);
    expectKeys('relocation auth kind', locales, 'authKind.', RELOCATION_AUTH_KINDS);
    // The validation messages are keys for the same reason: `validateTarget` returns key stems so
    // both locales are covered here rather than by whichever one the author happened to write.
    expectKeys('relocation validation', locales, 'invalid.', [
      'badHost',
      'badPort',
      'badUser',
      'badDir',
      'noSecret',
    ]);
  });

  it('every account kind has a label (access:users.kind.*)', () => {
    // The users list badges each account by kind, and the add-user dialog offers the creatable
    // ones. A kind without strings shows an operator a raw key while they decide what an account is.
    expectKeys('user kind', { en: enAccess, ja: jaAccess }, 'users.kind.', USER_KINDS);
  });

  // The badge's explanation (`access:users.kindInfo.<kind>.info`) is not a runtime key: the
  // `Record<UserKind, …>` in `pages/userKinds.ts` names each one, so a new kind is a compile error
  // there and ADR-200 G8 checks each named key exists (ADR-200 Inc.8 replaced the hover hint).

  it('every creatable account kind has a sub-label (access:users.kindSub.*)', () => {
    // The add-user dialog writes what the kind is for beside its name, in the option itself.
    expectKeys('user kind sub-label', { en: enAccess, ja: jaAccess }, 'users.kindSub.', CREATABLE_USER_KINDS);
  });

  it('every role has a sub-label in the add-user dialog (access:users.roleSub.*)', () => {
    expectKeys('role sub-label', { en: enAccess, ja: jaAccess }, 'users.roleSub.', ROLES);
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

  it('every Meraki collection tier has a label (system:meraki.tier.*)', () => {
    // This one had already gone wrong. The org list prints `t(`meraki.tier.${x}`)` for whatever the
    // server stored, and `PUT /meraki/orgs/{id}/cadence` accepts all four tiers — but only three
    // had strings, so an org with `inventory` enabled showed the operator the raw key. The cadence
    // dialog's checkboxes are a deliberate subset (`SELECTABLE_MERAKI_TIERS`); the *labels* are not.
    expectKeys('meraki tier', { en: enSystem, ja: jaSystem }, 'meraki.tier.', MERAKI_TIERS);
  });

  it('every Meraki collect listing has a label (system:meraki.listing.*)', () => {
    // A tier's failure line names which of its reads failed (ADR-164 decision 25) through
    // `meraki.listing.<token>`. A listing added to the list without its label would show the raw key.
    expectKeys('meraki listing', { en: enSystem, ja: jaSystem }, 'meraki.listing.', MERAKI_LISTINGS);
  });

  it('every Meraki uplink state has a word (nodes:overview.uplinkState.*)', () => {
    // The Meraki card renders `overview.uplinkState.<state>` for each WAN uplink (ADR-164 decision 24).
    // A state added to the decoder without its word would show the operator the raw key.
    expectKeys(
      'meraki uplink state',
      { en: enNodes, ja: jaNodes },
      'overview.uplinkState.',
      MERAKI_UPLINK_STATES,
    );
  });

  it('every warm-spare role and pair state has a word (nodes:overview.haRole.* / pairState.*)', () => {
    // The Meraki card renders the node's role, its partner's role and the pair's state from the
    // tokens the server sent (ADR-164 decision 26). A sixth state would reach the card as a raw key.
    expectKeys('meraki ha role', { en: enNodes, ja: jaNodes }, 'overview.haRole.', MERAKI_HA_ROLES);
    expectKeys('meraki pair state', { en: enNodes, ja: jaNodes }, 'overview.pairState.', MERAKI_PAIR_STATES);
  });

  it('every warm-spare role has its device-list label (system:meraki.devices.haRole.*)', () => {
    // The organization's device list puts the role under the model (`modelSubLine`).
    expectKeys('meraki device ha role', { en: enSystem, ja: jaSystem }, 'meraki.devices.haRole.', MERAKI_HA_ROLES);
  });

  it('every Meraki API region has a label (system:meraki.regions.*)', () => {
    // The "Add organization" dialog renders `t(`meraki.regions.${r.key}`)` for each entry of
    // `MERAKI_REGIONS`. The list moved out of the page into a registry (ADR-164), which made the
    // key runtime-built — so a fifth region needs its strings demanded here.
    expectKeys(
      'meraki region',
      { en: enSystem, ja: jaSystem },
      'meraki.regions.',
      MERAKI_REGION_KEYS,
    );
  });

  it('every reason a Meraki sync can fail has its sentence (system:meraki.sync.reason.*)', () => {
    // The organization's row renders `t(`meraki.sync.reason.${reason}`)` inside "Sync failed: …".
    // The vocabulary is closed on the backend (`MerakiSyncFailure`) and pinned to it by
    // `schemaEnumPins`, so an eleventh reason fails to compile there and is demanded here — instead
    // of reaching an operator as "Sync failed: meraki.sync.reason.quota" (ADR-164).
    expectKeys(
      'meraki sync failure',
      { en: enSystem, ja: jaSystem },
      'meraki.sync.reason.',
      MERAKI_SYNC_FAILURES,
    );
  });

  it('every state a Meraki device can be in has a label (system:meraki.devices.state.*)', () => {
    // The organization's page renders `t(`meraki.devices.state.${d.state}`)` in the State column and
    // offers the same five as its filter. The label is the *whole* signal there — the integrations
    // screens carry no status dot — so a sixth state with no strings would put a raw key in the one
    // column that says whether a device is monitored. Pinned to the backend by `schemaEnumPins`.
    expectKeys(
      'meraki device state',
      { en: enSystem, ja: jaSystem },
      'meraki.devices.state.',
      MERAKI_DEVICE_STATES,
    );
  });

  it('every reason a Meraki device is filed where it is has its sentence (system:meraki.devices.filing.*)', () => {
    // The Destination cell renders `t(`meraki.devices.filing.${reason}`)` under where an import
    // would put the device. `FilingReason` is closed on the backend; a sixth reason arriving with
    // no strings would explain a destination with its own key.
    expectKeys(
      'meraki filing reason',
      { en: enSystem, ja: jaSystem },
      'meraki.devices.filing.',
      MERAKI_FILING_REASONS,
    );
  });

  it('every folder origin has its sentence (nodes:tree.origin.*)', () => {
    // The inventory tree titles a folder's badge with `t(`tree.origin.${origin}`)`. The badge text
    // itself is a brand name and is not translated (`lib/groupOrigin.ts`); this is the sentence
    // that says what the badge means. `GroupOrigin` is closed on the backend and pinned by
    // `schemaEnumPins`, so a third integration that starts keeping folders stops compiling there
    // and is demanded here (ADR-164 Inc.7).
    expectKeys('group origin', { en: enNodes, ja: jaNodes }, 'tree.origin.', GROUP_ORIGINS);
  });

  it('every built-in Site ID field has a label (system:netbox.siteIdField.*)', () => {
    // The NetBox form renders `t(`netbox.siteIdField.${v}`)` for each built-in. A fourth built-in
    // added in Rust would reach both locales missing — parity passes, and the picker offers a row
    // spelling out its own key to an operator who has to guess what it means.
    expectKeys(
      'site id field',
      { en: enSystem, ja: jaSystem },
      'netbox.siteIdField.',
      SITE_ID_BUILT_INS,
    );
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
    // ADR-180: the address state is the chip, the filter option and its explanation.
    expectKeys('neighbor address state', locales, 'neighbors.peer.state.', NEIGHBOR_ADDRESS_STATES);
    expectKeys('neighbor address explain', locales, 'neighbors.peer.explain.', NEIGHBOR_ADDRESS_STATES);
    // ADR-180 Inc.3: a row matched on its MAC is never ambiguous and always has a chassis.
    expectKeys('neighbor explain by MAC', locales, 'neighbors.peer.explainMac.', [
      'node',
      'outside_scope',
      'unregistered',
    ]);
    // ADR-180 Inc.4: a name picks one of several claimants only when it picks a node the caller
    // can see (decision 6); the other claimants say whether their port has link.
    expectKeys('neighbor explain by name', locales, 'neighbors.peer.explainName.', ['node']);
    expectKeys('neighbor claimant link', locales, 'neighbors.peer.also.port.', CLAIM_PORT_STATES);
    // ADR-179 Inc.9: why a "Not monitored" row has no setup button.
    expectKeys('neighbor setup blocked', locales, 'neighbors.setup.blocked.', SETUP_BLOCKED_REASONS);
    // The badge beside a neighbour's name (ADR-179 Inc.3): every state but "none", which draws none.
    expectKeys('neighbor monitored badge', locales, 'neighbors.peer.badge.', NEIGHBOR_PEER_STATES);
    expectKeys('neighbor detail label', locales, 'neighbors.detail.', NEIGHBOR_DETAIL_KEYS);
  });

  it('every access-point state and monitoring state has strings (nodes:ap.*)', () => {
    // Both are built at runtime from a value the controller supplied, and both are the whole
    // content of their column — a raw `not_associated` in the State column is the one cell an
    // operator reads to decide whether an AP is a problem.
    const locales = { en: enNodes, ja: jaNodes };
    // `unknown` is not one of the three the API can answer: it is what the screen shows when the
    // controller sent a token this build does not know, and it needs a string exactly as much.
    expectKeys('wireless ap state', locales, 'ap.state.', [...WLAN_AP_STATES, 'unknown']);
    expectKeys('wireless ap import state', locales, 'ap.import.', AP_IMPORT_STATES);
  });

  it('every link source has strings (topology:map.source.*)', () => {
    // The map labels each edge with the evidence behind it, from a value the server derived —
    // `t(`map.source.${link.source}`)`. Increments 2-4 add `route`, `bgp` and `ospf`, so this is
    // an enum that is *going* to grow, which is exactly the case parity cannot catch.
    const locales = { en: enTopology, ja: jaTopology };
    expectKeys('link source', locales, 'map.source.', LINK_SOURCES);
  });

  it('every map box kind has strings (topology:map.kind.*)', () => {
    // The map panel's legend names each kind of box with `t(`map.kind.${k}`)` (ADR-191).
    const locales = { en: enTopology, ja: jaTopology };
    expectKeys('map endpoint kind', locales, 'map.kind.', MAP_ENDPOINT_KINDS);
  });

  it('every map role and its reason has strings (topology:map.role.*, map.roleReason.*)', () => {
    // The side panel names a node's role and why with keys built from the server's enums (ADR-191
    // Inc.6).
    const locales = { en: enTopology, ja: jaTopology };
    expectKeys('map role', locales, 'map.role.', MAP_ROLES);
    expectKeys('map role reason', locales, 'map.roleReason.', MAP_ROLE_REASONS);
  });

  it('every sky has strings (topology:geo.dayNight.sky.*)', () => {
    // The Geo map's legend and each site's tooltip build `geo.dayNight.sky.${sky}` (ADR-189).
    expectKeys('geo sky', { en: enTopology, ja: jaTopology }, 'geo.dayNight.sky.', SKIES);
  });

  it('every topology mode has strings (topology:dependency.mode.*)', () => {
    // The Dependencies banner builds `dependency.mode.${mode}` and `${mode}Note` from a value the
    // server returns. A fourth mode would reach both locales missing, and the screen that decides
    // how the whole fleet suppresses alerts would render the raw token.
    const locales = { en: enTopology, ja: jaTopology };
    expectKeys('topology mode', locales, 'dependency.mode.', TOPOLOGY_MODES);
    expectKeys(
      'topology mode note',
      locales,
      'dependency.mode.',
      TOPOLOGY_MODES.map((m) => `${m}Note`),
    );
  });

  it('every comparison verdict has strings (topology:dependency.verdict.*)', () => {
    // Built as `dependency.verdict.${row.verdict}` plus a `verdictHelp` tooltip, from the pure
    // classifier — so both key families are checked, not just the visible label.
    const locales = { en: enTopology, ja: jaTopology };
    expectKeys('diff verdict', locales, 'dependency.verdict.', DIFF_VERDICTS);
    expectKeys('diff verdict help', locales, 'dependency.verdictHelp.', DIFF_VERDICTS);
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
    // The unit beside each editable box, built as `settings.retention.unit.${row.unit || 'days'}`
    // from a server value (`retention.rs` `Field::unit`). Two units today, and getting one wrong is
    // not cosmetic — it is the difference between keeping unmatched events for 72 hours and 72 days.
    expectKeys('retention unit', locales, 'settings.retention.unit.', ['days', 'hours']);
  });

  it('every discovery walk has a name and info (system:settings.neighbors.walk.*)', () => {
    // The card renders one block per walk from `DISCOVERY_WALKS`, so a fourth walk with no strings
    // gives the operator two raw keys where the control's label and explanation should be — on the
    // screen that decides whether a fleet-wide SNMP walk is issued at all. The explanation opens
    // from the name (ADR-200), keyed through `WALK_INFO`.
    const locales = { en: enSystem, ja: jaSystem };
    for (const walk of DISCOVERY_WALKS) {
      expectKeys('discovery walk', locales, `settings.neighbors.walk.${walk}.`, [
        'name',
        'info',
      ]);
    }
  });

  it('every endpoint coverage state has a string (monitoring:discovery.seen.coverage.*)', () => {
    // The three states are not shades of the same message: `off` means nobody looked and `complete`
    // means nothing was found. Rendering a raw key for either — or, worse, having one fall through
    // to the other's text — is the exact confusion `coverageOf` exists to prevent.
    expectKeys(
      'endpoint coverage',
      { en: enMonitoring, ja: jaMonitoring },
      'discovery.seen.coverage.',
      ENDPOINT_COVERAGE,
    );
  });

  it('every place an endpoint can be seen has a label (monitoring:discovery.seen.source.*)', () => {
    // Built from the generated union by `sourcesOf` (ADR-179). A source the backend gains fails the
    // type check in discoveredEndpoints.ts first; this is the half that proves it has words.
    expectKeys(
      'endpoint source',
      { en: enMonitoring, ja: jaMonitoring },
      'discovery.seen.source.',
      ENDPOINT_SOURCES,
    );
  });

  it('every weekday has a label on both surfaces that render one', () => {
    // One token list, two key prefixes: the schedule labels (`reports:weekday.*`) and the alert
    // calendar's row headers (`dashboard:widgets.alertCalendar.dow.*`). The list used to be
    // declared twice — and the *order* is load-bearing on both sides, because the index is
    // `Date.getDay()`, so a private copy could silently label Sunday's row as Monday.
    expectKeys('weekday', { en: enReports, ja: jaReports }, 'weekday.', WEEKDAY_KEYS);
    expectKeys(
      'alert calendar weekday',
      { en: enDashboard, ja: jaDashboard },
      'widgets.alertCalendar.dow.',
      WEEKDAY_KEYS,
    );
  });

  it('every widget backing tag has a label (dashboard:catalog.backing.*)', () => {
    // Badged on every card in the "add widget" picker, from the registry's own field.
    expectKeys('widget backing', { en: enDashboard, ja: jaDashboard }, 'catalog.backing.', BACKINGS);
  });

  it('every maintenance-window status has a label (suppression:maintenance.status.*)', () => {
    // The badge in the windows table. `disabled` and `ended` look alike and mean opposite things
    // to an operator asking "are my alerts muted right now" — a raw key for either is worse than
    // useless on that screen.
    expectKeys(
      'maintenance status',
      { en: enSuppression, ja: jaSuppression },
      'maintenance.status.',
      MAINTENANCE_STATUSES,
    );
  });

  it('every certificate import block has an explanation (settings-tls:import.block.*)', () => {
    // This hint is the *only* thing telling an operator why the Import button will not light up.
    expectKeys(
      'certificate import block',
      { en: enSettingsTls, ja: jaSettingsTls },
      'import.block.',
      IMPORT_BLOCKS,
    );
  });

  it('every form refusal code has a sentence, on each form that has one', () => {
    // Five dialogs each render `t(`<prefix>.${code}`)` with **no** `defaultValue`, so a code added
    // to the validator without its strings is a raw key where the only explanation should be. They
    // are grouped into one test because they share a failure mode, not a namespace.
    expectKeys(
      'group geo problem',
      { en: enNodes, ja: jaNodes },
      'err.',
      GEO_PROBLEMS,
    );
    // ADR-131: the range editor's refusals, rendered the same way and with the same exposure.
    expectKeys(
      'group prefix problem',
      { en: enNodes, ja: jaNodes },
      'err.',
      [...PREFIX_PROBLEMS],
    );
    expectKeys(
      'check config problem',
      { en: enNodes, ja: jaNodes },
      'checkEdit.err.',
      CHECK_FORM_PROBLEMS,
    );
    // 🚨 ADR-135 shipped `t(`field.tagErr.${problem}`)` with **no** entry here, so its three codes
    // were held up by EN/JA parity alone — which cannot see a code missing from both. Adding a
    // fourth would have rendered a raw key in every language. ADR-135 Inc.2 closes that while
    // replacing the codes.
    expectKeys(
      'node label problem',
      { en: enNodes, ja: jaNodes },
      'field.tagErr.',
      [...LABEL_PROBLEMS],
    );
    expectKeys('ai form problem', { en: enSettingsAi, ja: jaSettingsAi }, 'err.', AI_FORM_PROBLEMS);
    expectKeys(
      'ldap form problem',
      { en: enSettingsAuth, ja: jaSettingsAuth },
      'ldap.err.',
      LDAP_FORM_PROBLEMS,
    );
    expectKeys(
      'schedule form problem',
      { en: enTroubleshoot, ja: jaTroubleshoot },
      'schedule.err.',
      SCHEDULE_FORM_PROBLEMS,
    );
    // Walked through `bundleImportErrorKey` rather than used raw: `unsupported-version` renders a
    // sentence carrying the version it found, so its leaf is `version`, not the reason.
    expectKeys(
      'bundle import refusal',
      { en: enSystem, ja: jaSystem },
      'bundle.import.err.',
      BUNDLE_IMPORT_REASONS.map(bundleImportErrorKey),
    );
  });

  it('every Troubleshoot report bucket has a label (troubleshoot:report.*)', () => {
    // Five buckets the report bodies compute client-side and render through `t()`. They are derived
    // in TypeScript rather than returned by the API — three of them deliberately re-implement a
    // backend classification that only exists inside an English sentence — so nothing on the Rust
    // side would ever surface a missing string.
    const locales = { en: enTroubleshoot, ja: jaTroubleshoot };
    // The runway is pluralized ("1 day left" / "3 days left"), so the stored keys carry i18next's
    // suffixes — the `users.scope.groups` case below, and the same reason: the suffix set is a
    // property of the language, not of the union. JA has one plural form and therefore only
    // `_other`, which is why the `_one` pass compares EN against itself.
    expectKeys(
      'capacity tte unit',
      locales,
      'report.capacity.tte.',
      TTE_UNITS.map((u) => `${u}_other`),
    );
    expectKeys(
      'capacity tte unit (en singular)',
      { en: enTroubleshoot, ja: enTroubleshoot },
      'report.capacity.tte.',
      TTE_UNITS.map((u) => `${u}_one`),
    );
    expectKeys('correlation direction', locales, 'report.correlation.dir.', CORRELATION_DIRECTIONS);
    expectKeys('flap bucket', locales, 'report.flap.bucket.', FLAP_BUCKETS);
    expectKeys('scan pattern', locales, 'report.flow_scan.pattern.', SCAN_PATTERNS);
    expectKeys('timeline lane', locales, 'report.incident_correlate.lane.', TIMELINE_LANES);
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
  it('every duplicate evidence kind, confidence and contradiction has strings (monitoring:duplicates.*)', () => {
    // Nodes ▸ Duplicates names each piece of evidence, and each reason a group is only "to check",
    // from a value the server sent, so a new kind on the server must arrive with words in both
    // languages rather than as a raw key on the operator's screen.
    const locales = { en: enMonitoring, ja: jaMonitoring };
    expectKeys('duplicate evidence kind', locales, 'duplicates.kind.', DUPLICATE_EVIDENCE_KINDS);
    expectKeys('duplicate confidence', locales, 'duplicates.confidence.', DUPLICATE_CONFIDENCES);
    expectKeys('duplicate contradiction', locales, 'duplicates.contradiction.', DUPLICATE_CONTRADICTIONS);
  });
  it('every site status and layout has strings (monitoring:missingPrefixes.*)', () => {
    // Nodes ▸ Missing IP prefixes names its tabs from a token the server sent (ADR-170 Inc.2).
    const locales = { en: enMonitoring, ja: jaMonitoring };
    expectKeys('site gap status', locales, 'missingPrefixes.tabs.', SITE_GAP_STATUSES);
    expectKeys('site gap empty state', locales, 'missingPrefixes.empty.', SITE_GAP_STATUSES);
    expectKeys('missing-prefix layout', locales, 'missingPrefixes.views.', MISSING_PREFIX_VIEWS);
  });
  it('every overlap kind, status, hint and rule reason has strings (monitoring:subnetOverlaps.*)', () => {
    // Nodes ▸ Subnet overlaps names each of these from a token the server sent (ADR-187).
    const locales = { en: enMonitoring, ja: jaMonitoring };
    expectKeys('overlap kind', locales, 'subnetOverlaps.kind.', OVERLAP_KINDS);
    expectKeys('overlap kind help', locales, 'subnetOverlaps.kindHelp.', OVERLAP_KINDS);
    expectKeys('overlap tab', locales, 'subnetOverlaps.tabs.', OVERLAP_STATUSES);
    expectKeys('overlap column', locales, 'subnetOverlaps.whyHeader.', OVERLAP_STATUSES);
    expectKeys('overlap empty state', locales, 'subnetOverlaps.empty.', OVERLAP_STATUSES);
    expectKeys('overlap hint', locales, 'subnetOverlaps.hint.', OVERLAP_HINT_KINDS);
    expectKeys('overlap hint tag', locales, 'subnetOverlaps.hintTag.', OVERLAP_HINT_KINDS);
    expectKeys('exclusion reason', locales, 'subnetOverlaps.rules.reason.', EXCLUSION_REASONS);
  });

  it('every part of the visual template editor has its words (alertsConfig:routing.template.*)', () => {
    // ADR-039 Inc.2. Every one of these keys is built from a runtime value: a variable's name and
    // explanation from the server's catalogue, a group, a tab, a sample, a preset, and the reason a
    // stored template opens as code. A name added in Rust reaches the insert list with a raw key in
    // both locales, which parity passes.
    const locales = { en: enAlertsConfig, ja: jaAlertsConfig };
    const p = 'routing.template.';
    expectKeys('template variable', locales, `${p}vars.`, TEMPLATE_VARIABLE_NAMES.map((n) => `${n}.label`));
    expectKeys('template variable meaning', locales, `${p}vars.`, TEMPLATE_VARIABLE_NAMES.map((n) => `${n}.desc`));
    expectKeys('template variable group', locales, `${p}groups.`, TEMPLATE_VARIABLE_GROUPS);
    expectKeys('template tab', locales, `${p}tabs.`, TEMPLATE_EVENTS);
    expectKeys('preview sample', locales, `${p}samples.`, PREVIEW_SAMPLES.map((s) => s.id));
    expectKeys('template preset', locales, `${p}preset.`, TEMPLATE_PRESETS);
    expectKeys('unsupported template', locales, `${p}unsupported.`, UNSUPPORTED_REASONS);
    expectKeys('edit mode', locales, `${p}mode.`, ['visual', 'code']);
    expectKeys('field hint', locales, p, ['subjectHint.jsm', 'subjectHint.email', 'bodyHint.jsm', 'bodyHint.email']);
    // ADR-197 Inc.2: what each kind sends, each key of the built-in JSON, and the note on a tab
    // whose event the kind never renders — all built from a runtime value.
    expectKeys('what a channel sends', locales, `${p}sends.`, CHANNEL_KINDS);
    expectKeys('built-in JSON key', locales, `${p}jsonKeys.`, BUILTIN_JSON_KEYS);
    expectKeys(
      'close-only tab note',
      locales,
      `${p}builtinView.closeOnly.`,
      [...new Set(Object.values(TEMPLATE_FORMS).flatMap((f) => f.unusedAt))],
    );
  });

  it('every delivery-log kind, result and side has its words (alertsConfig:routing.log.*)', () => {
    // ADR-195. The badge, the filter options and the row's explanation are built from the token the
    // server sent, so a fourth side added in Rust would ship as a raw key in both locales.
    const locales = { en: enAlertsConfig, ja: jaAlertsConfig };
    expectKeys('delivery event', locales, 'routing.log.event.', DELIVERY_EVENTS);
    expectKeys('delivery result', locales, 'routing.log.result.', DELIVERY_RESULTS);
    expectKeys('delivery side', locales, 'routing.log.side.', DELIVERY_SIDES);
    expectKeys('delivery side explanation', locales, 'routing.log.sideExplain.', DELIVERY_SIDES);
  });
});
