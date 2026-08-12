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
  LINK_SOURCES,
  TOPOLOGY_MODES,
  TLS_CERT_SOURCES,
  METRIC_STATUSES,
  METRIC_DIMENSIONS,
  NODE_KINDS,
} from './types/api';
import { NODE_KIND_SPEC } from './lib/nodeKind';
import { WEEKDAY_KEYS } from './lib/cadence';
import { BACKINGS } from './dashboard/types';
import { GEO_PROBLEMS } from './components/GroupModal/geoFields';
import { CHECK_FORM_PROBLEMS } from './components/NodeDetail/checkConfigForm';
import { AI_FORM_PROBLEMS } from './pages/aiConfigForm';
import { LDAP_FORM_PROBLEMS } from './pages/ldapConfigForm';
import { BUNDLE_IMPORT_REASONS, bundleImportErrorKey } from './pages/configBundle';
import { IMPORT_BLOCKS } from './pages/tlsSettingsForm';
import { MAINTENANCE_STATUSES } from './pages/maintenanceStatus';
import { SCHEDULE_FORM_PROBLEMS } from './troubleshoot/scheduleForm';
import {
  CORRELATION_DIRECTIONS,
  FLAP_BUCKETS,
  SCAN_PATTERNS,
  TIMELINE_LANES,
  TTE_UNITS,
} from './troubleshoot/report/format';
import { DIFF_VERDICTS } from './pages/topologyDiff';
import { MERAKI_TIERS } from './pages/merakiTiers';
import { DISCOVERY_WALKS } from './pages/neighborSettings';
import { ENDPOINT_COVERAGE } from './pages/discoveredEndpoints';
import { SEVERITY_ORDER } from './lib/nodeState';
import { KNOWN_SCALARS } from './lib/format';
import { PROFILE_CATEGORIES } from './lib/profileCategories';
import { METRIC_CARDS } from './components/NodeDetail/metricCards';
import { MONITOR_KINDS } from './pages/monitorKinds';
import { ADD_MENU_LABEL_KEYS } from './pages/nodesAddMenu';
import { CAUSE_LABEL_KEYS } from './lib/suppression';
import { RUN_STATUS } from './reports/runStatus';
import { CADENCE, SELECTABLE_CADENCES } from './lib/cadence';
import { FORWARD_FILTER_FIELDS, opsForField } from './pages/forwardingOptions';
import { TERMINAL_JOB_STATES } from './troubleshoot/data';
import { FINDING_RANGES } from './troubleshoot/findingsQuery';
import { HTTP_AUTH_SCHEMES } from './pages/httpAuthCredential';
import { BUNDLE_TABLES } from './pages/configBundle';
import { RETENTION_FIELDS, RETENTION_SUBJECTS } from './pages/retentionSettings';
import {
  BODY_MATCH_MODES,
  EXPECTED_STATUS_MODES,
} from './components/NodeDetail/checkConfigForm';
import { EXPIRY_CHOICES } from './pages/tokenForm';
import { EXPIRY_LEVELS } from './pages/tlsSettingsForm';
import { OIDC_ISSUER_PARAMS, OIDC_PICKER_ORDER } from './pages/oidcPresets';

import enAccess from './locales/en/access.json';
import enSettingsTokens from './locales/en/settings-tokens.json';
import jaSettingsTokens from './locales/ja/settings-tokens.json';
import enSettingsTls from './locales/en/settings-tls.json';
import jaSettingsTls from './locales/ja/settings-tls.json';
import enSettingsUpgrade from './locales/en/settings-upgrade.json';
import jaSettingsUpgrade from './locales/ja/settings-upgrade.json';
import {
  UPGRADE_BUILD_KINDS,
  UPGRADE_OFFER_BLOCKS,
  UPGRADE_OFFER_DIRECTIONS,
  UPGRADE_RUN_STATES,
  UPGRADE_RUN_STEPS,
} from './pages/upgradeStatus';
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

  it('every ＋-menu label resolves (nodes:tree.*/addMenu.*)', () => {
    // The inventory ＋ picks its two labels at runtime from the tree selection, and `t()` is not
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

  it('every node kind has a label (nodes:kind.*)', () => {
    // The label is the badge's tooltip — the only place the badge's two or three letters are spelled
    // out. This is the *display* set, so unlike MONITOR_KINDS above it must cover `meraki` too.
    const keys = NODE_KINDS.map((k) => NODE_KIND_SPEC[k].labelKey);
    expectKeys('node kind', { en: enNodes, ja: jaNodes }, '', keys);
  });

  it('every token surface has a label and a hint (settings-tokens:surface.*)', () => {
    // The label names the surface in the list and the dialog; the hint is what tells an admin what
    // they are handing out. A surface added without either would offer an operator a raw key at the
    // exact moment they decide how much power a credential carries.
    const locales = { en: enSettingsTokens, ja: jaSettingsTokens };
    expectKeys('token surface', locales, 'surface.', TOKEN_SURFACES);
    expectKeys('token surface hint', locales, 'surfaceHint.', TOKEN_SURFACES);
  });

  it('every IdP product has a name and setup guidance (settings-auth:idp.*)', () => {
    // The hint is the whole increment: it is where "Entra refuses a non-standard scope" and
    // "Google puts no groups in the ID token" are told to the operator. A product added without one
    // is a picker entry that offers no more help than the free-text form it replaced.
    const locales = { en: enSettingsAuth, ja: jaSettingsAuth };
    expectKeys('IdP product', locales, 'idp.', OIDC_PICKER_ORDER);
    expectKeys('IdP product hint', locales, 'idpHint.', OIDC_PICKER_ORDER);
  });

  it('every product-specific issuer field is labelled (settings-auth:field.*)', () => {
    // A product whose issuer is built from one field needs a label, a placeholder and a hint for
    // it — the operator is being asked for a tenant id or an Okta domain, not for a URL.
    const keys = OIDC_ISSUER_PARAMS.flatMap((p) => [p, `${p}Placeholder`, `${p}Hint`]);
    expectKeys(
      'issuer field',
      { en: enSettingsAuth, ja: jaSettingsAuth },
      'field.',
      keys,
    );
  });

  it('every token state has an explanation (settings-tokens:state.* / stateHint.*)', () => {
    // A token can be dead for four independent reasons and the operator needs the real one — "the
    // owner is disabled" and "this expired" call for different actions.
    //
    // `tokenForm.ts`'s `TokenState` is a plain union, so the members are listed here rather than
    // iterated; both key families are derived from this one list so they cannot drift apart.
    const states = ['active', 'revoked', 'expired', 'no-owner', 'owner-disabled'];
    const locales = { en: enSettingsTokens, ja: jaSettingsTokens };
    expectKeys('token state', locales, 'state.', states);
    // The badge's tooltip, which is the only place the UI says *why* a token stopped
    // authenticating. `active` and `revoked` deliberately have none (the label already says it),
    // so the hint set is the rest — a sixth dead-state added without one would put a raw key in a
    // `title` attribute, where even a reviewer would not see it.
    expectKeys(
      'token state hint',
      locales,
      'stateHint.',
      states.filter((s) => s !== 'active' && s !== 'revoked'),
    );
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
    expectKeys(
      'upgrade build kind hint',
      { en: enSettingsUpgrade, ja: jaSettingsUpgrade },
      'buildKindHint.',
      UPGRADE_BUILD_KINDS,
    );
    // Direction is the one this page got wrong in front of an operator: every button read
    // "upgrade to this" while offering versions older than the running one. Four key families are
    // keyed off it now, and a missing one would put a raw key on the button that replaces a
    // production deployment.
    for (const prefix of [
      'offerAction.',
      'apply.confirmTitle.',
      'apply.confirmBody.',
      'apply.confirm.',
    ]) {
      expectKeys(
        `upgrade offer direction (${prefix})`,
        { en: enSettingsUpgrade, ja: jaSettingsUpgrade },
        prefix,
        UPGRADE_OFFER_DIRECTIONS,
      );
    }
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

  it('every Meraki collection tier has a label (system:meraki.tier.*)', () => {
    // This one had already gone wrong. The org list prints `t(`meraki.tier.${x}`)` for whatever the
    // server stored, and `PUT /meraki/orgs/{id}/cadence` accepts all four tiers — but only three
    // had strings, so an org with `inventory` enabled showed the operator the raw key. The cadence
    // dialog's checkboxes are a deliberate subset (`SELECTABLE_MERAKI_TIERS`); the *labels* are not.
    expectKeys('meraki tier', { en: enSystem, ja: jaSystem }, 'meraki.tier.', MERAKI_TIERS);
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

  it('every link source has strings (topology:map.source.*)', () => {
    // The map labels each edge with the evidence behind it, from a value the server derived —
    // `t(`map.source.${link.source}`)`. Increments 2-4 add `route`, `bgp` and `ospf`, so this is
    // an enum that is *going* to grow, which is exactly the case parity cannot catch.
    const locales = { en: enTopology, ja: jaTopology };
    expectKeys('link source', locales, 'map.source.', LINK_SOURCES);
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

  it('every discovery walk has a name and help (system:settings.neighbors.walk.*)', () => {
    // The card renders one block per walk from `DISCOVERY_WALKS`, so a fourth walk with no strings
    // gives the operator two raw keys where the control's label and explanation should be — on the
    // screen that decides whether a fleet-wide SNMP walk is issued at all.
    const locales = { en: enSystem, ja: jaSystem };
    for (const walk of DISCOVERY_WALKS) {
      expectKeys('discovery walk', locales, `settings.neighbors.walk.${walk}.`, [
        'name',
        'help',
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

  it('every user-list filter segment has a label (access:users.filter.*)', () => {
    // The segmented control is `['all', ...ROLES]`, built in `UsersPage.tsx`. Rebuilt here from the
    // same union rather than imported, because the page is a `.tsx` and Vitest never loads one —
    // the list is one expression long, so restating it beats moving the control's layout into a
    // module for the test's sake. A fourth role reaches this test through `ROLES`.
    expectKeys('user filter segment', { en: enAccess, ja: jaAccess }, 'users.filter.', [
      'all',
      ...ROLES,
    ]);
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
    expectKeys(
      'check config problem',
      { en: enNodes, ja: jaNodes },
      'checkEdit.err.',
      CHECK_FORM_PROBLEMS,
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
});
