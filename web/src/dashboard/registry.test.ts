import { describe, expect, it } from 'vitest';
import { catalogBySection, defaultLayout, getDefinition, REGISTRY, registryView } from './registry';

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

  it('exposes a registryView consistent with the catalog', () => {
    for (const d of REGISTRY) {
      expect(registryView.isKnownType(d.type)).toBe(true);
      expect(registryView.allowedSpansFor(d.type)).toEqual(d.allowedSpans);
      expect(registryView.defaultSpanFor(d.type)).toBe(d.defaultSpan);
    }
    expect(registryView.isKnownType('does-not-exist')).toBe(false);
  });

  it('produces a default layout referencing only known types with valid spans', () => {
    const layout = defaultLayout();
    expect(layout.widgets.length).toBeGreaterThan(0);
    const ids = layout.widgets.map((w) => w.instanceId);
    expect(new Set(ids).size).toBe(ids.length); // stable, unique ids
    for (const w of layout.widgets) {
      const def = getDefinition(w.type);
      expect(def).toBeDefined();
      expect(def!.allowedSpans).toContain(w.span);
    }
  });

  it('groups the catalog by section covering every widget exactly once', () => {
    const grouped = catalogBySection();
    const flat = grouped.flatMap((g) => g.widgets);
    expect(flat.length).toBe(REGISTRY.length);
    expect(new Set(flat.map((d) => d.type)).size).toBe(REGISTRY.length);
  });
});
