// SPDX-License-Identifier: AGPL-3.0-only
// Every search box in the WebUI is the one shared box (ADR-132), and that box still suppresses the
// browser's own clear button.
//
// WHY THIS READS SOURCE AS TEXT. The call sites are `.tsx` files and the rest is CSS — Vitest
// executes neither (`vite.config.ts` sets `include: ['src/**/*.test.ts']`, `environment: 'node'`),
// so there is nothing to import and ask. `tableIds.test.ts` solves the same problem the same way and
// this file copies its shape, including the part that matters most:
//
// 🚨 **The floors are the load-bearing half.** Every check here has "found nothing" as its healthy
// answer, so it has to be able to tell that apart from "looked at nothing". A regex that stops
// matching — someone wraps the component, renames the prop, reformats the attributes — would
// otherwise report a clean tree forever. The floors count what was *inspected*
// (`floor-must-count-what-was-checked`).
//
// What each check is about, because none of them is tidiness:
//   - a hand-rolled box has no ✕ at all, and the operator's only way to empty it is Ctrl+A;
//   - a `type="search"` outside the component keeps the browser's ✕, so Chrome draws two and
//     Firefox draws none — the per-browser answer this ADR removed;
//   - the room reserved on the right is a `padding-right` in a **compound** selector on purpose. A
//     call site's own `padding: 6px 8px` is one class, so a single-class rule here would be decided
//     by stylesheet order and lose, and the text would run under the ✕. That reads as "a bit
//     cramped", not as a bug, which is exactly the kind of defect a test has to hold.
import { describe, expect, it } from 'vitest';
import { readdirSync, readFileSync } from 'node:fs';
import { join, relative } from 'node:path';

const SRC = join(__dirname);
const COMPONENT = 'components/ui/SearchField.tsx';
const STYLESHEET = join(SRC, 'components/ui/SearchField.css');

function filesUnder(dir: string, ext: string, out: string[] = []): string[] {
  for (const e of readdirSync(dir, { withFileTypes: true })) {
    const p = join(dir, e.name);
    if (e.isDirectory()) filesUnder(p, ext, out);
    else if (e.name.endsWith(ext)) out.push(p);
  }
  return out;
}

const rel = (p: string) => relative(SRC, p).split('\\').join('/');

const TSX = filesUnder(SRC, '.tsx').filter((f) => !f.includes('.test.'));

interface Tag {
  where: string;
  file: string;
  text: string;
}

/** Every `<input` and `<TextInput` opening tag in the tree.
 *
 *  Bounded by the tag rather than by a line count, so a reformat cannot quietly shrink what is
 *  inspected. `<TextInput` counts because it is the generic form field — two of the boxes this ADR
 *  converted were search boxes wearing it, which is why they had no ✕ and no magnifier. */
function inputTags(): Tag[] {
  const out: Tag[] = [];
  for (const file of TSX) {
    const src = readFileSync(file, 'utf8');
    for (const m of src.matchAll(/<(?:input|TextInput)\b/g)) {
      const i = m.index ?? 0;
      const close = src.indexOf('>', i);
      out.push({
        where: `${rel(file)}:${src.slice(0, i).split('\n').length}`,
        file: rel(file),
        text: src.slice(i, close === -1 ? src.length : close),
      });
    }
  }
  return out;
}

function callSites(): string[] {
  const out: string[] = [];
  for (const file of TSX) {
    const src = readFileSync(file, 'utf8');
    for (const m of src.matchAll(/<SearchField\b/g)) {
      out.push(`${rel(file)}:${src.slice(0, m.index ?? 0).split('\n').length}`);
    }
  }
  return out;
}

describe('the search box', () => {
  it('inspected the inputs this product actually has', () => {
    // Floors, not statistics. 197 `.tsx`, 235 input tags and 11 call sites were measured when this
    // shipped; a detector that silently stopped matching would otherwise pass with zero.
    expect(TSX.length).toBeGreaterThanOrEqual(150);
    expect(inputTags().length).toBeGreaterThanOrEqual(200);
    expect(callSites().length).toBeGreaterThanOrEqual(11);
  });

  it('is the only place a search input is declared', () => {
    // Both directions. Losing it from the component is as bad as a second one appearing: the
    // suppression of the browser's own ✕ is attached to this input's class.
    const typed = inputTags().filter((t) => t.text.includes('type="search"'));
    expect(typed.map((t) => t.file)).toEqual([COMPONENT]);
  });

  it('is what every search box in the tree goes through', () => {
    const handRolled = inputTags()
      .filter((t) => t.file !== COMPONENT && /search/i.test(t.text))
      .map((t) => `${t.where} — a search box that is not <SearchField>, so it has no way to clear itself`);
    expect(handRolled.sort()).toEqual([]);
  });
});

describe("the shared stylesheet", () => {
  const css = readFileSync(STYLESHEET, 'utf8');

  /** Every rule block in the stylesheet, as `{ selector, body }`. */
  const rules = [...css.matchAll(/([^{}]+)\{([^}]*)\}/g)].map((m) => ({
    selector: m[1].replace(/\/\*[\s\S]*?\*\//g, '').trim().split('\n').pop()!.trim(),
    body: m[2],
  }));

  it('read the rules it is about', () => {
    expect(rules.length).toBeGreaterThanOrEqual(10);
  });

  it("suppresses the browser's own clear button", () => {
    // Blink and WebKit draw one, Gecko draws none. Without this, "can I empty this box" is a
    // question about the browser again — and where it is drawn, there are two ✕ side by side.
    const suppress = rules.filter((r) => r.selector.includes('::-webkit-search-cancel-button'));
    expect(suppress.length).toBe(1);
    expect(suppress[0].body).toMatch(/display:\s*none/);
  });

  it('reserves the room for the ✕ with a compound selector', () => {
    // 🚨 The failure this exists for is silent: a single-class `.sfield-input { padding-right }`
    // ties with the four call sites that declare their own `padding`, and a tie is decided by
    // stylesheet order — which this component loses, since every consumer imports it first.
    const reserving = rules.filter((r) => /padding-right/.test(r.body));
    expect(reserving.length).toBeGreaterThanOrEqual(1);
    for (const r of reserving) expect(r.selector).toContain('> .sfield-input');
  });

  it('widens the ✕ for touch through the one canonical spelling', () => {
    // A 20px square is not a touch target, and `ui-conventions.md` names `@media (hover: none)` as
    // the only spelling for a touch affordance — a `data-viewport` selector would miss a tablet
    // held to the desktop shell. Tier1 runs at 1280px and can never see this.
    expect(css).toContain('@media (hover: none)');
  });
});
