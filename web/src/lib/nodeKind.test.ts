// SPDX-License-Identifier: AGPL-3.0-only
import { readFileSync, readdirSync } from 'node:fs';
import { join } from 'node:path';
import { describe, it, expect } from 'vitest';
import { BADGE_ICONS, NODE_KIND_SPEC, badgeIconClass, nodeBadges } from './nodeKind';
import { NODE_KINDS } from '../types/api';

describe('NODE_KIND_SPEC', () => {
  it('covers exactly the backend node kinds', () => {
    expect(Object.keys(NODE_KIND_SPEC).sort()).toEqual([...NODE_KINDS].sort());
  });

  // The unmarked default is the point: an inventory tree is overwhelmingly ordinary devices, and a
  // badge on every one of 50k rows would carry no information. A badge means "not a normal device".
  it('leaves the ordinary device unbadged and badges every other kind', () => {
    for (const kind of NODE_KINDS) {
      const badge = NODE_KIND_SPEC[kind].badge;
      if (kind === 'device') expect(badge).toBeNull();
      else expect(badge, kind).toBeTruthy();
    }
  });

  it('gives each kind a distinct badge and a distinct label key', () => {
    const badges = NODE_KINDS.map((k) => NODE_KIND_SPEC[k].badge).filter((b) => b !== null);
    expect(new Set(badges).size).toBe(badges.length);
    const labels = NODE_KINDS.map((k) => NODE_KIND_SPEC[k].labelKey);
    expect(new Set(labels).size).toBe(labels.length);
    for (const k of labels) expect(k).toMatch(/^kind\./);
  });

  // The tree row is a single 30px flex line (`ROW_H` in NodeTree.tsx) that already carries a status
  // dot, the name and up to two suppression marks. A long badge pushes the marks out of view.
  it('keeps badges short enough for a tree row', () => {
    for (const kind of NODE_KINDS) {
      const badge = NODE_KIND_SPEC[kind].badge;
      if (badge) expect(badge.length, kind).toBeLessThanOrEqual(8);
    }
  });

  // Each kind is polled over a different protocol, so no single metric answers "did we hear from
  // it". Asking every kind for icmp_rtt_ms is what left three of the four with no "seen" line.
  it('gives each kind its own liveness metric', () => {
    const metrics = NODE_KINDS.map((k) => NODE_KIND_SPEC[k].livenessMetric);
    expect(new Set(metrics).size).toBe(metrics.length);
    for (const m of metrics) expect(m).toMatch(/^[a-z][a-z0-9_]*$/);
    expect(NODE_KIND_SPEC.device.livenessMetric).toBe('icmp_rtt_ms');
    expect(NODE_KIND_SPEC.url.livenessMetric).toBe('http_up');
    expect(NODE_KIND_SPEC.dns.livenessMetric).toBe('dns_up');
    expect(NODE_KIND_SPEC.meraki.livenessMetric).toBe('meraki_device_up');
  });
});

// ── The mirror ───────────────────────────────────────────────────────────────
//
// `livenessMetric` is the same fact as Rust's `NodeKind::liveness_metric`, and for a while it was a
// one-way mirror: this file knew a URL monitor is never pinged while the backend's fleet-coverage
// query asked every node for `icmp_rtt_ms` anyway, reporting three of the four kinds as silent
// forever (ADR-059). Adding the Rust side made it a real mirror, and `extensibility.md` §2 says a
// mirror ships with the test that fails when it drifts. The follower checks itself against the
// source: a `t()`-style parity gate could not, because both sides would be equally wrong.

const COMMON_SRC = join(__dirname, '..', '..', '..', 'crates', 'yagra-common', 'src');

/** Every `pub const METRIC_X: &str = "…"` in `yagra-common`, as name → value. */
function rustMetricConstants(): Map<string, string> {
  const out = new Map<string, string>();
  for (const file of readdirSync(COMMON_SRC)) {
    if (!file.endsWith('.rs')) continue;
    const src = readFileSync(join(COMMON_SRC, file), 'utf8');
    for (const m of src.matchAll(/pub const (METRIC_\w+): &str = "([^"]+)";/g)) {
      out.set(m[1], m[2]);
    }
  }
  return out;
}

/** `NodeKind::liveness_metric`'s arms, as the serialized kind token → metric name. */
function rustLivenessMetrics(): Record<string, string> {
  const src = readFileSync(join(COMMON_SRC, 'node_kind.rs'), 'utf8');
  const body = /pub const fn liveness_metric\(self\) -> &'static str \{([\s\S]*?)\n {4}\}/.exec(src);
  expect(body, 'liveness_metric not found — the parser, not the code, is what broke').toBeTruthy();
  const constants = rustMetricConstants();
  const out: Record<string, string> = {};
  for (const arm of body![1].matchAll(/Self::(\w+) => (\w+),/g)) {
    const value = constants.get(arm[2]);
    expect(value, `${arm[2]} is not a METRIC_* const in yagra-common`).toBeTruthy();
    out[arm[1].replace(/([a-z])([A-Z])/g, '$1_$2').toLowerCase()] = value!;
  }
  return out;
}

describe('NODE_KIND_SPEC mirrors the Rust NodeKind', () => {
  it('agrees with `NodeKind::liveness_metric` on every kind', () => {
    const rust = rustLivenessMetrics();
    // Assert the parse found everything *before* comparing: a regex that silently matched nothing
    // would make an empty comparison look like agreement.
    expect(Object.keys(rust).sort()).toEqual([...NODE_KINDS].sort());
    for (const kind of NODE_KINDS) {
      expect(NODE_KIND_SPEC[kind].livenessMetric, kind).toBe(rust[kind]);
    }
  });
});

// An access point's badge is the Wi-Fi mark, black on white (user decision, 2026-09-23) — for the
// controller-walked AP and for a Meraki MR alike, since both name what the device is.
describe('node badges drawn as a glyph', () => {
  it('draws both kinds of access point as the Wi-Fi mark, and nothing else as a glyph', () => {
    expect(NODE_KIND_SPEC.wireless_ap.badgeIcon).toBe('wifi');
    for (const kind of NODE_KINDS) {
      if (kind !== 'wireless_ap') expect(NODE_KIND_SPEC[kind].badgeIcon, kind).toBeNull();
    }
    expect(nodeBadges({ kind: 'wireless_ap' })).toEqual([
      { text: 'AP', brand: null, icon: 'wifi', labelKey: 'kind.wireless_ap' },
    ]);
    expect(
      nodeBadges({ kind: 'meraki', merakiProductType: 'wireless' }).map((b) => [
        b.text,
        b.icon,
        b.brand,
      ]),
    ).toEqual([
      ['Meraki', null, 'meraki'],
      ['AP', 'wifi', null],
    ]);
    expect(
      nodeBadges({ kind: 'meraki', merakiProductType: 'switch' }).map((b) => b.icon),
    ).toEqual([null]);
  });

  // ADR-175: a mesh repeater is an MR, so "Repeater" follows the AP mark — and only there. The
  // flag on anything that is not a Meraki access point draws nothing.
  it('marks a Meraki access point that is a mesh repeater, after the AP mark', () => {
    expect(
      nodeBadges({ kind: 'meraki', merakiProductType: 'wireless', merakiRepeater: true }).map(
        (b) => [b.text, b.icon, b.labelKey],
      ),
    ).toEqual([
      ['Meraki', null, 'kind.meraki'],
      ['AP', 'wifi', 'kindBadge.accessPoint'],
      ['Repeater', null, 'kindBadge.meshRepeater'],
    ]);
    expect(
      nodeBadges({ kind: 'meraki', merakiProductType: 'switch', merakiRepeater: true }).map(
        (b) => b.text,
      ),
    ).toEqual(['Meraki']);
    expect(nodeBadges({ kind: 'wireless_ap', merakiRepeater: true }).map((b) => b.text)).toEqual([
      'AP',
    ]);
  });

  it('adds a class only for a glyph', () => {
    expect(badgeIconClass(null)).toBe('');
    expect(badgeIconClass('wifi')).toBe(' is-wifi');
  });

  // A glyph with no rule draws in the default accent on the default pill — the badge looks
  // deliberate and is simply not what was asked for, which nothing else here can see.
  it('has a rule in both badge stylesheets for every glyph, reading its two tokens', () => {
    const src = join(__dirname, '..');
    const tokens = readFileSync(join(src, 'styles/tokens.css'), 'utf8');
    const pills: [string, string][] = [
      ['.nd-kind', 'components/NodeDetail/NodeDetail.css'],
      ['.ntree-badge', 'components/NodeTree/NodeTree.css'],
    ];
    let checked = 0;
    for (const icon of BADGE_ICONS) {
      expect(tokens).toMatch(new RegExp(`--badge-${icon}-bg:\\s*#`));
      expect(tokens).toMatch(new RegExp(`--badge-${icon}-fg:\\s*#`));
      for (const [pill, file] of pills) {
        const css = readFileSync(join(src, file), 'utf8');
        const rule = new RegExp(`\\${pill}\\.is-${icon}\\s*\\{([^}]*)\\}`).exec(css);
        expect(rule, `${pill}.is-${icon} in ${file}`).not.toBeNull();
        expect(rule?.[1]).toContain(`var(--badge-${icon}-bg)`);
        expect(rule?.[1]).toContain(`var(--badge-${icon}-fg)`);
        expect(css, `${pill} .badge-glyph in ${file}`).toContain(`${pill} .badge-glyph {`);
        checked++;
      }
    }
    expect(checked).toBe(BADGE_ICONS.length * pills.length);
  });
});
