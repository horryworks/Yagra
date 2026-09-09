// SPDX-License-Identifier: AGPL-3.0-only
import { describe, expect, it } from 'vitest';
import { REGISTRY, defaultLayout, emptyPublicLayout } from './registry';
import { NOT_PUBLIC, catalogFor, whyNotPublic } from './publicCatalog';

describe('the public board default', () => {
  it('starts empty, unlike the other two boards', () => {
    // 🚨 A publishing decision, not a presentation one. Seeding it with the five-widget default
    // would mean turning the switch on published a fleet summary nobody chose to publish — and,
    // because the anonymous route allow-list is derived from the widgets on this board, it would
    // open their API routes too. An admin composes what strangers see, deliberately.
    const pub = emptyPublicLayout();
    expect(pub.boards).toHaveLength(1);
    expect(pub.boards[0].widgets).toEqual([]);
    // The contrast is the point: the shared/my default is not empty.
    expect(defaultLayout().boards[0].widgets.length).toBeGreaterThan(0);
  });

  it('shares the document version with the other boards', () => {
    // A board saved at a different version would be migrated by `sanitizeLayout` on the next load,
    // which for an empty board looks identical — so this is checked rather than assumed.
    expect(emptyPublicLayout().version).toBe(defaultLayout().version);
  });
});

describe('the public board catalog', () => {
  it('excludes the audit widget, and says why', () => {
    // The recognition test. A deny-list whose entries all disappeared would still pass every
    // "returns a list" assertion, so the one case everybody pictures is named outright.
    expect(whyNotPublic('audit')).toMatch(/view_audit/);
    expect(catalogFor(true).some((d) => d.type === 'audit')).toBe(false);
    expect(catalogFor(false).some((d) => d.type === 'audit')).toBe(true);
  });

  it('names only widgets that exist', () => {
    // 🚨 A renamed widget would leave a dead entry here, and a dead entry excludes nothing — the
    // widget it was meant to keep off the public board would quietly become placeable again.
    const known = new Set(REGISTRY.map((d) => d.type));
    for (const type of Object.keys(NOT_PUBLIC)) {
      expect(known.has(type), `NOT_PUBLIC names '${type}', which is not a widget`).toBe(true);
    }
  });

  it('gives every exclusion a non-empty reason', () => {
    for (const [type, why] of Object.entries(NOT_PUBLIC)) {
      expect(why.length, `${type} is excluded with no reason`).toBeGreaterThan(10);
    }
  });

  it('keeps the flow widgets available', () => {
    // Not an oversight: the flow tier being off is a typed 503 for everyone on every board, which
    // is a deployment saying "not configured" rather than a permission the visitor lacks. Pinned
    // because "it might 503" is a plausible-sounding reason to add them, and doing so would make
    // the public board the one place a configured flow tier could not be shown.
    for (const type of ['flow-top-talkers', 'flow-trend', 'flow-conversations']) {
      expect(whyNotPublic(type)).toBeUndefined();
    }
  });

  it('excludes only a small minority — the public catalog is not a stub', () => {
    // A floor on what survived. If the filter ever started matching too much, a nearly empty
    // catalog would look like a very careful one.
    expect(catalogFor(true).length).toBeGreaterThanOrEqual(REGISTRY.length - 5);
    expect(catalogFor(true).length).toBeGreaterThan(40);
  });
});
