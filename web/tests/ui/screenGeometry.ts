// SPDX-License-Identifier: AGPL-3.0-only
// The geometry every screen owes, inspected in a browser (ADR-088 × ADR-052).
//
// WHY THIS FILE EXISTS. Four of the last ten `fix(web)` commits were the same shape: the element
// was in the DOM, its text was in the DOM, and a human still could not read or press it. Row action
// icons were `opacity: 0` on ten screens at once; a scope name was cut off; a picker opened outside
// the viewport. **Every text assertion passes on all three** — `toHaveText` reads the DOM, not the
// pixels — and Playwright's `isVisible()` counts an `opacity: 0` element as visible, so even the
// clicks succeeded. The only witness is the layout engine, which is why this is a `page.evaluate`
// over real boxes and not an assertion about DOM shape.
//
// It rides the visit the route walk already makes, so covering every screen costs no navigation.
// The two checks here are the ones that need no interaction at all; the ones that need a hover or a
// different viewport width live in their own specs (`rowActions.spec.ts`, `overflowMenu.spec.ts`),
// the way `filterGeometry.spec.ts` holds the parts of ADR-053 that the walk cannot carry.
//
// ⚠️ **These are floors, not opinions about the design** — the same rule `filterSurface.ts` states.
// Each number below names where it comes from. A threshold someone invented gets re-tuned the first
// time it is inconvenient, and then it guards nothing.
//
// 🚨 **`inspected` is not a statistic, it is the guard.** A selector that stops matching makes a
// browser check pass with the same face as a screen that is fine — this repo has shipped that
// failure three times (ADR-083's moved `.split()`, ADR-085's stale needle, ADR-086's two sides of a
// comparison sharing one source). The call site asserts the count, so a traversal that silently
// stops finding anything fails instead of going green.

import type { Page } from '@playwright/test';

/** One thing wrong on one screen. `where` locates it for a human; `why` is the invariant.
 *  Same shape as `FilterFinding` deliberately — the walk reports both the same way. */
export interface GeometryFinding {
  where: string;
  why: string;
}

export interface GeometryReport {
  findings: GeometryFinding[];
  /** How many text-rendering elements the sweep actually looked at. See the floor note above. */
  inspected: number;
}

/** Sub-pixel rounding on a scaled or bordered box lands within a pixel. Real truncation is a word
 *  wide, never one pixel — the same reasoning as `filterSurface.ts`'s `SLOP`, one axis down. */
const SLOP = 1;

/** The smallest cut worth reporting. Measured on this app: the string "granted" renders 46px for
 *  seven characters, so a character at `--font-sm` is about 6.6px — under that, either nothing
 *  legible is missing or the box merely ends a pixel past its container, which is rounding rather
 *  than truncation. */
const CUT_PX = 6;

/** How much of an element's text to quote in a finding. Long enough to identify the string on the
 *  screen, short enough that a failure message stays readable. */
const QUOTE = 40;

/** Below this, a box is not showing text to a sighted operator at all, so "is it truncated" is not
 *  a question about it. The visually-hidden idiom — `position: absolute; width: 1px; height: 1px;
 *  overflow: hidden; clip: rect(0,0,0,0)` — renders at exactly 1px and is *meant* to be read only
 *  by a screen reader; `RolesPage.css`'s `.roles-sr` is this app's copy of it, and it accounted for
 *  24 of the 39 findings on the first run. 8px is under every size in the type scale, so nothing
 *  that renders a glyph falls through here. */
const LEGIBLE_PX = 8;

/** The floor on `inspected`, measured on the first full run (2026-08-23, all 46 screens).
 *
 *  The sparsest screen is `/login` at 9 — a sign-in form outside the app shell, so it has no nav,
 *  no page header and no table. The two map screens follow at 22 and 26 (SVG, almost no text), and
 *  every other screen is 40 or more, up to 185. 5 sits below the sparsest by enough that a copy
 *  change cannot trip it, and far enough above zero that a traversal which stopped matching — the
 *  failure this exists for — cannot clear it. */
export const MIN_TEXT_ELEMENTS = 5;

export async function inspectScreenGeometry(page: Page): Promise<GeometryReport> {
  return page.evaluate(
    ({ slop, quote, legible, cutPx }) => {
      const findings: { where: string; why: string }[] = [];
      let inspected = 0;

      // ── 1. Nothing is laid out off the side of the page ───────────────────────────────────────
      // The only visible symptom the portalled-popover bug ever produced. `AnchoredPopover` exists
      // because a `position: fixed` panel resolves against the nearest ancestor that establishes a
      // containing block — a virtualized row's `transform` is exactly that — and the panel then
      // lays itself out somewhere off to the right. Nothing throws, nothing is missing, and the
      // page grows a horizontal scrollbar. `rowMenu.spec.ts` already uses this as a second witness
      // on one screen; here it costs nothing and covers every screen.
      //
      // The page body must never scroll horizontally: wide content (tables, diagrams) scrolls
      // inside its own container. So this needs no exemption list — a screen that trips it is
      // either laying something out off-screen or has lost the container that should be scrolling.
      const root = document.documentElement;
      if (root.scrollWidth > root.clientWidth + slop) {
        findings.push({
          where: 'document',
          why: `the page scrolls horizontally (${root.scrollWidth}px of content in ${root.clientWidth}px) — something is laid out off to the side, or a wide element lost the container that should scroll it`,
        });
      }

      // ── 2. Text that is cut off has some way to be read in full ───────────────────────────────
      // 🚨 The rule is NOT "clipped text is a defect" and NOT "an ellipsis means it was deliberate".
      // Both are wrong here, and the second was this check's first design:
      //
      //   - `.yt-entity-name` — the shared component that renders every entity reference — is
      //     `text-overflow: ellipsis` by default (`styles/table.css`). The Thresholds defect that
      //     motivated this work was *exactly* an ellipsis: the operator could not tell which Cisco
      //     profile the rule named. Excusing ellipses would have excused the defect.
      //   - And 62 `text-overflow` declarations across 31 stylesheets are correct design. Failing
      //     all of them would make this a check nobody keeps.
      //
      // What separates the two is whether the whole string is still reachable. `ThresholdsPage`
      // solved its own case by putting all three names in a `title`; that is the escape hatch, and
      // adding one is also the right fix for a screen reader. So: clipped, and no `title` on the
      // element or an ancestor that carries the text ⇒ finding.
      //
      // ⚠️ **The `title` must CONTAIN the text, not merely exist.** `EntityName` sets
      // `title={id}` — the raw UUID, deliberately, so the id stays reachable without being the
      // cell's primary text. A presence check would let a truncated name be excused by a tooltip
      // that answers a different question.
      const hasOwnText = (el: Element): boolean => {
        for (const node of Array.from(el.childNodes)) {
          if (node.nodeType === Node.TEXT_NODE && (node.textContent || '').trim() !== '') {
            return true;
          }
        }
        return false;
      };

      const fullText = (el: Element): string => (el.textContent || '').replace(/\s+/g, ' ').trim();

      const coveredByTitle = (el: Element, text: string): boolean => {
        for (let node: Element | null = el; node; node = node.parentElement) {
          const title = node.getAttribute('title');
          if (title && title.replace(/\s+/g, ' ').includes(text)) return true;
        }
        return false;
      };

      for (const el of Array.from(document.body.querySelectorAll('*'))) {
        // Only elements that render text themselves. A container whose children hold the text is
        // not the box doing the clipping, and counting it would report the same string many times
        // — and would drag in layout overflow that has nothing to do with readability.
        if (!hasOwnText(el)) continue;
        const box = el.getBoundingClientRect();
        // Not rendered, or rendered at the visually-hidden idiom's 1px — see `LEGIBLE_PX`. Skipped
        // BEFORE the count, because an element no sighted operator can read is not a subject of
        // this check and counting it would inflate the floor with things it does not guard.
        if (box.width < legible || box.height < legible) continue;

        inspected++;

        // Two ways a line gets cut, and this app produces both. `DataTable.css` gives `.dt-cell > *`
        // its own `overflow: hidden`, so a table cell's text element IS the clipping box — that is
        // the self-clipped case, and all fifteen findings on the first run were it. The other is a
        // container clipping a child that overflows it, which no `scrollWidth` on the child can see
        // because the child is laid out at its full width and simply extends past the edge.
        //
        // `auto`/`scroll` are not clipping: the rest can be scrolled to, which is a legitimate
        // design (a wide table inside its own scroller) rather than lost text.
        const clips = (e: Element): boolean => {
          const o = getComputedStyle(e).overflowX;
          return o === 'hidden' || o === 'clip';
        };
        let cutBy = clips(el) ? el.scrollWidth - el.clientWidth : 0;
        if (cutBy <= 0) {
          for (let p = el.parentElement; p && p !== document.body; p = p.parentElement) {
            if (!clips(p)) continue;
            const pb = p.getBoundingClientRect();
            cutBy = Math.max(box.right - pb.right, pb.left - box.left);
            break; // only the nearest clipping ancestor decides
          }
        }
        // Below one character nothing legible is lost, and a box that ends a pixel or two past its
        // container is rounding, not truncation. Measured on this app's own first run: "granted"
        // is 46px for 7 characters, so ~6.6px is a character at `--font-sm`.
        if (cutBy < cutPx) continue;

        const text = fullText(el);
        if (text === '') continue;
        if (coveredByTitle(el, text)) continue;

        const cls = el.getAttribute('class') || '';
        findings.push({
          where: `<${el.tagName.toLowerCase()}${cls ? ` class="${cls.slice(0, 60)}"` : ''}>`,
          why: `"${text.slice(0, quote)}${text.length > quote ? '…' : ''}" is cut off by ${Math.round(cutBy)}px (${Math.round(box.width)}px shown) and no title on it or an ancestor carries the whole string`,
        });
      }

      return { findings, inspected };
    },
    { slop: SLOP, quote: QUOTE, legible: LEGIBLE_PX, cutPx: CUT_PX },
  );
}
