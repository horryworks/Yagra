// SPDX-License-Identifier: AGPL-3.0-only
import { describe, expect, it } from 'vitest';
import { catalogBySection, defaultLayout, getDefinition, REGISTRY, registryView } from './registry';
import enDashboard from '../locales/en/dashboard.json';
import jaDashboard from '../locales/ja/dashboard.json';

describe('widget registry', () => {
  it('has unique, stable type ids', () => {
    const types = REGISTRY.map((d) => d.type);
    expect(new Set(types).size).toBe(types.length);
    expect(types.every((t) => /^[a-z][a-z0-9-]*$/.test(t))).toBe(true);
  });

  it('keeps each defaultSpan within its allowedSpans', () => {
    for (const d of REGISTRY) {
      expect(d.allowedSpans.length).toBeGreaterThan(0);
      expect(d.allowedSpans).toContain(d.defaultSpan);
      expect(d.section).toBeTruthy();
      expect(d.title).toBeTruthy();
    }
  });

  it('keeps allowedRowSpans well-formed and containing the (defaulted) default height', () => {
    for (const d of REGISTRY) {
      if (!d.allowedRowSpans) continue; // fixed-height widget — no height control
      expect(d.allowedRowSpans.length).toBeGreaterThan(0);
      expect(d.allowedRowSpans).toContain(d.defaultRowSpan ?? 1);
      expect(d.allowedRowSpans).toContain(1); // standard height must always be selectable
    }
  });

  it('exposes a registryView consistent with the catalog', () => {
    for (const d of REGISTRY) {
      expect(registryView.isKnownType(d.type)).toBe(true);
      expect(registryView.allowedSpansFor(d.type)).toEqual(d.allowedSpans);
      expect(registryView.defaultSpanFor(d.type)).toBe(d.defaultSpan);
      expect(registryView.allowedRowSpansFor(d.type)).toEqual(d.allowedRowSpans ?? [1]);
      expect(registryView.defaultRowSpanFor(d.type)).toBe(d.defaultRowSpan ?? 1);
    }
    expect(registryView.isKnownType('does-not-exist')).toBe(false);
    // an unknown type reports fixed-height defaults
    expect(registryView.allowedRowSpansFor('does-not-exist')).toEqual([1]);
    expect(registryView.defaultRowSpanFor('does-not-exist')).toBe(1);
  });

  it('produces a current-version default layout: one board of known types with valid spans', () => {
    const layout = defaultLayout();
    expect(layout.version).toBe(3);
    expect(layout.boards).toHaveLength(1);
    const widgets = layout.boards[0].widgets;
    expect(widgets.length).toBeGreaterThan(0);
    const ids = widgets.map((w) => w.instanceId);
    expect(new Set(ids).size).toBe(ids.length); // stable, unique ids
    for (const w of widgets) {
      const def = getDefinition(w.type);
      expect(def).toBeDefined();
      expect(def!.allowedSpans).toContain(w.span);
    }
  });

  it('has a title and a blurb in both locales for every widget', () => {
    // EN/JA parity cannot catch this: a new widget's strings are missing from *both* files, so
    // parity passes and the catalog shows the operator `registry.widgets.metric-top.title`.
    const strings = (bundle: Record<string, unknown>, key: string): unknown =>
      key.split('.').reduce<unknown>((o, k) => (o as Record<string, unknown> | undefined)?.[k], bundle);
    for (const d of REGISTRY) {
      for (const [lang, bundle] of [
        ['en', enDashboard],
        ['ja', jaDashboard],
      ] as const) {
        expect(strings(bundle, d.title), `${lang}: ${d.title}`).toBeTruthy();
        expect(strings(bundle, d.blurb), `${lang}: ${d.blurb}`).toBeTruthy();
      }
    }
  });

  // ── The Customize / view split (ADR-072) ────────────────────────────────────────────────
  // A control that chooses what a card is *about* belongs behind the ⚙ the frame draws while the
  // board is being customized; a time window or a display lens stays in the header. The type
  // system enforces the write (`Actions` receives `ViewSettings`, so a subject key does not
  // compile) — what it cannot say is *which* widgets ended up on which side, so that is pinned
  // here by name. A new widget with a subject picker fails this test until someone decides.

  /** Every widget whose settings choose a subject. Deliberately spelled out, not counted. */
  const WITH_SETTINGS = ['metric-chart', 'metric-top', 'interface-traffic'];

  it('gives a ⚙ panel to exactly the widgets that choose a subject', () => {
    const withSettings = REGISTRY.filter((d) => d.Settings).map((d) => d.type);
    expect(withSettings.slice().sort()).toEqual(WITH_SETTINGS.slice().sort());
  });

  it('leaves the metric chart no view-mode actions at all', () => {
    // Both of its controls pick the subject (which node, which metric); it has no window and no
    // lens, so its header outside Customize is just the title.
    expect(getDefinition('metric-chart')!.Actions).toBeUndefined();
  });

  it('keeps the view-mode actions the split was supposed to leave behind', () => {
    // The accepting half. Without it, "every Actions was deleted" passes the two tests above —
    // an absence-only check cannot tell a correct split from a demolition.
    expect(getDefinition('metric-top')!.Actions).toBeDefined(); // now / 1h-max window
    expect(getDefinition('interface-traffic')!.Actions).toBeDefined(); // unit + time window
    expect(getDefinition('top-cpu')!.Actions).toBeDefined(); // a plain Top-N still has its window
    // …and a widget with no settings of any kind gets no ⚙.
    expect(getDefinition('top-cpu')!.Settings).toBeUndefined();
    expect(getDefinition('status-summary')!.Actions).toBeUndefined();
    expect(getDefinition('status-summary')!.Settings).toBeUndefined();
  });

  it('groups the catalog by section covering every widget exactly once', () => {
    const grouped = catalogBySection();
    const flat = grouped.flatMap((g) => g.widgets);
    expect(flat.length).toBe(REGISTRY.length);
    expect(new Set(flat.map((d) => d.type)).size).toBe(REGISTRY.length);
  });
});
