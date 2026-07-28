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
} from './types/api';
import { SEVERITY_ORDER } from './lib/nodeState';
import { KNOWN_SCALARS } from './lib/format';
import { PROFILE_CATEGORIES } from './lib/profileCategories';
import { METRIC_CARDS } from './components/NodeDetail/metricCards';

import enCommon from './locales/en/common.json';
import jaCommon from './locales/ja/common.json';
import enFormat from './locales/en/format.json';
import jaFormat from './locales/ja/format.json';
import enNodes from './locales/en/nodes.json';
import jaNodes from './locales/ja/nodes.json';
import enMonitoring from './locales/en/monitoring.json';
import jaMonitoring from './locales/ja/monitoring.json';
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
});
