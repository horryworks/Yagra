// SPDX-License-Identifier: AGPL-3.0-only
// ADR-200 G3, G4 and G5: the amount of explanatory prose only goes down.
//
// The WebUI had about 100,000 characters of explanation on its screens (2026-10-05), written one
// feature at a time under ADR-055's "the text on screen is the manual". Nothing stopped the next
// paragraph, so deleting prose did not last. These three tables are that stop:
//
// - G3 `PROSE_CEILING` — per namespace, the characters in strings of five or more English words.
//   Fails above the ceiling, and also far below it (a ceiling left high is a ceiling that lets the
//   next paragraph back in): lower the number in the same change that removed the prose.
// - G4 `LONG_LEGACY` — every string over 200 English / 120 Japanese characters. Only shrinks.
//   A new long string needs a row in `LONG_ALLOWED` with the reason.
// - G5 `HOVER_LEGACY` — every `title={t('…')}` over 60 English characters. Hover-only text cannot
//   be read on touch (ADR-055 R4). Only shrinks.
//
// Raising a ceiling needs a reason in the commit message — a new screen's state message, say.
// What prose may stay at all is in `.claude/rules/ui-conventions.md` (the ADR-200 section).

import { describe, expect, it } from 'vitest';
import { flattenStrings, loadLocales, namespacesOf, resolveKey } from './testSupport/locales';
import {
  hoverKeysIn,
  isProse,
  longKeys,
  NOT_MEASURED,
  proseMass,
  wordCount,
} from './testSupport/prose';
import { readSources, SRC } from './testSupport/sources';

/** `[en, ja]` characters of prose per namespace. Measured 2026-10-06; lower as prose goes. */
const PROSE_CEILING: Record<string, [number, number]> = {
  access: [2758, 1544],
  alertNames: [166, 97],
  alerts: [1584, 936],
  alertsConfig: [12764, 6860],
  auth: [79, 43],
  common: [717, 385],
  dashboard: [7597, 4128],
  format: [0, 0],
  metrics: [127, 65],
  monitoring: [15065, 8889],
  nav: [3416, 1615],
  nodes: [23885, 13324],
  rca: [1038, 532],
  reports: [773, 477],
  'settings-ai': [1947, 1033],
  'settings-auth': [5756, 3314],
  'settings-forwarding': [3454, 1893],
  'settings-relocation': [5026, 2560],
  'settings-tls': [2614, 1345],
  'settings-tokens': [2039, 1128],
  'settings-upgrade': [5401, 3046],
  settings: [687, 374],
  suppression: [1584, 1013],
  system: [20894, 11428],
  topology: [4376, 2365],
  troubleshoot: [7786, 4027],
};

/** How far below its ceiling a namespace may sit before the ceiling must come down. */
const SLACK: [number, number] = [300, 200];

/** Strings over 200 EN / 120 JA characters that predate ADR-200. Remove entries; never add. */
const LONG_LEGACY: string[] = [
  'access:users.scopeModal.intro',
  'alerts:row.merakiOrgSubjectHint',
  'alertsConfig:routing.template.builtinView.condNote',
  'alertsConfig:routing.template.builtinView.jsonNote',
  'alertsConfig:routing.template.freeLayout.hint',
  'alertsConfig:routing.template.intro',
  'alertsConfig:routing.test.intro',
  'alertsConfig:thresholds.addModal.boundsHint',
  'alertsConfig:thresholds.addModal.rowMatchHint',
  'alertsConfig:thresholds.explainer',
  'monitoring:discovery.examplesHint',
  'monitoring:discovery.seen.coverage.off',
  'monitoring:discovery.seen.note',
  'monitoring:duplicates.hint',
  'monitoring:missingPrefixes.note',
  'monitoring:subnetOverlaps.rules.text',
  'nav:shell.comingSoonText',
  'nodes:bulkTag.note',
  'nodes:deleteNode.body',
  'nodes:deleteNodes.body',
  'nodes:field.tagHint',
  'nodes:group.prefixesHint',
  'nodes:interfaces.colAddressesTitle',
  'nodes:interfaces.colNeighborsTitle',
  'nodes:interfaces.duplexHint',
  'nodes:interfaces.mediaHint',
  'nodes:neighbors.detail.note',
  'nodes:neighbors.peer.hint',
  'nodes:rediscover.applyHint',
  'settings-ai:field.maxTokensHint',
  'settings-ai:note',
  'settings-auth:idpHint.entra',
  'settings-auth:idpHint.google',
  'settings-auth:idpHint.okta',
  'settings-auth:ldap.field.groupSearchHint',
  'settings-auth:ldap.note',
  'settings-auth:publicDashboard.hint',
  'settings-auth:redirectUriMismatch',
  'settings-forwarding:field.fidelityHint',
  'settings-forwarding:field.fidelityRowsOnly',
  'settings-forwarding:field.serviceAccountHint',
  'settings-forwarding:filter.flowAnyRecord',
  'settings-forwarding:note',
  'settings-relocation:warning.secrets',
  'settings-tls:regenerate.intro',
  'settings-tls:warning.apiPortPublic',
  'settings-tls:warning.keyUnreadable',
  'settings-tokens:note',
  'settings-tokens:ssoIdle',
  'settings-upgrade:bundle.howTo',
  'settings-upgrade:mechanism.unsupportedHint',
  'settings-upgrade:sitePrep.fix',
  'settings-upgrade:sitePrep.warning',
  'settings-upgrade:sitePrep.warning_other',
  'suppression:mutes.note',
  'system:bundle.notBackup',
  'system:health.netNote',
  'system:netbox.form.baseUrlHint',
  'system:pollers.anchor.hint',
  'system:pollers.pool.coverWarning',
  'system:pollers.pool.createNote',
  'system:pollers.pool.renameBlockedWhy',
  'system:pollers.register.intro',
  'system:pollers.token.selfUpgrade.hint',
  'system:settings.neighbors.walk.arp.help',
  'system:settings.neighbors.walk.media.help',
  'system:settings.neighbors.walk.routing.help',
  'system:supportBundle.contents',
  'troubleshoot:catalog.note',
  'troubleshoot:report.event_flap.note',
  'troubleshoot:report.event_storm.note',
  'troubleshoot:report.flow_scan.note',
  'troubleshoot:report.new_destination.note',
  'troubleshoot:report.rule_gap.note',
  'troubleshoot:report.saturation.note',
  'troubleshoot:report.severity_shift.note',
  'troubleshoot:report.traffic_anomaly.note',
];

/** Long strings allowed on purpose, with the reason. */
const LONG_ALLOWED: Record<string, string> = {};

/** `title={t('…')}` strings over 60 EN characters that predate ADR-200. Remove; never add. */
const HOVER_LEGACY: string[] = [
  'access:audit.exportHint',
  'alerts:acked.title',
  'alerts:active.muteHint',
  'alerts:row.merakiOrgSubjectHint',
  'alerts:row.poolSubjectHint',
  'alertsConfig:routing.template.status.confirmTitle',
  'alertsConfig:thresholds.addModal.dwellTitle',
  'alertsConfig:thresholds.meaningUnknown',
  'dashboard:widgets.ifTraffic.unitTitle',
  'dashboard:widgets.pollerHealth.mirrorWritesHint',
  'dashboard:widgets.pollerHealth.poolsHint',
  'dashboard:widgets.pollerHealth.workingSetHint',
  'monitoring:discovery.seen.detect.hint',
  'nodes:interfaces.colAddressesTitle',
  'nodes:interfaces.colInOutTitle',
  'nodes:interfaces.colNeighborsTitle',
  'nodes:interfaces.duplexHint',
  'nodes:interfaces.rulesButtonTitle',
  'nodes:inventory.needAttentionOnlyHint',
  'rca:meta.cachedHint',
  'settings-ai:test.hint',
  'settings-forwarding:health.degradedHint',
  'system:pollers.gaps.passiveHint',
  'system:pollers.selfUpgradeHint',
  'system:pollers.skewHint',
  'topology:dependency.optOutHelp',
  'topology:map.search.stepHint',
  'troubleshoot:report.event_flap.balanceTitle',
];

const locales = loadLocales();
const measured = Object.keys(locales).filter((ns) => !NOT_MEASURED.includes(ns));

describe('G3: prose per namespace stays under its ceiling', () => {
  it('counts words the way the budget means', () => {
    expect(wordCount('Save')).toBe(1);
    expect(wordCount('Delete {{count}} <b>nodes</b> now')).toBe(3);
    expect(isProse('No API tokens yet.')).toBe(false);
    expect(isProse('No API tokens exist yet.')).toBe(true);
    expect(proseMass({ a: 'one two three four five', b: 'short' }, { a: 'いちにさん' })).toEqual([
      23, 5,
    ]);
    expect(
      proseMass({ n_one: 'one node is not polled now', n_other: 'x' }, { n_other: '台' }),
    ).toEqual([26, 1]);
  });

  it('has one ceiling per measured namespace, and no other', () => {
    expect(measured.length).toBeGreaterThan(20);
    expect(Object.keys(PROSE_CEILING).sort()).toEqual([...measured].sort());
  });

  it('no namespace is over its ceiling or far under it', () => {
    const off = measured.flatMap((ns) => {
      const [en, ja] = proseMass(locales[ns].en, locales[ns].ja);
      const [ce, cj] = PROSE_CEILING[ns] ?? [0, 0];
      const bad = en > ce || ja > cj || ce - en > SLACK[0] || cj - ja > SLACK[1];
      return bad ? [`${ns}: measured [${en}, ${ja}], ceiling [${ce}, ${cj}]`] : [];
    });
    expect(off).toEqual([]);
  });
});

describe('G4: no new string longer than 200 EN / 120 JA characters', () => {
  it('finds a long string and skips a short one', () => {
    const fake = {
      x: {
        en: { a: 'e'.repeat(201), b: 'ok' },
        ja: { a: 'j', b: 'j'.repeat(121) },
      },
    };
    expect(longKeys(fake)).toEqual(['x:a', 'x:b']);
  });

  it('the long strings are exactly the legacy list and the allowed ones', () => {
    const found = longKeys(locales);
    expect(found.length).toBeGreaterThan(0);
    const listed = [...LONG_LEGACY, ...Object.keys(LONG_ALLOWED)].sort();
    expect(found).toEqual(listed);
  });
});

describe('G5: no new sentence that only a hover can read', () => {
  const files = readSources(SRC, { exts: ['.tsx'] });
  const sites = files.flatMap(([file, src]) => {
    const ns = namespacesOf(src);
    return hoverKeysIn(src).map((k) => ({
      file,
      key: resolveKey(k, ns, locales),
    }));
  });

  it('inspected the tree it is supposed to be reading', () => {
    // 315 literal title={t(…)} sites on 2026-10-06.
    expect(sites.length).toBeGreaterThan(250);
    expect(
      hoverKeysIn('<b title={t(\'a.b\')} /><i title={t("c:d", { n })} /><u title={x} />'),
    ).toEqual(['a.b', 'c:d']);
  });

  it('the long hover strings are exactly the legacy list', () => {
    const long = new Set<string>();
    for (const { key } of sites) {
      if (!key) continue;
      const cut = key.indexOf(':');
      const v = flattenStrings(locales[key.slice(0, cut)].en)[key.slice(cut + 1)] ?? '';
      if (v.length > 60) long.add(key);
    }
    expect([...long].sort()).toEqual([...HOVER_LEGACY].sort());
  });
});
