// SPDX-License-Identifier: AGPL-3.0-only
// The geometry of a filter surface, inspected in a browser (ADR-053 × ADR-052).
//
// WHY THIS FILE EXISTS. ADR-053 shipped with 1,966 green unit tests and **six defects were still
// found by eye**. Re-read as a set, four of the six are one question nothing could ask: *is the
// control I added actually there, under the right column, big enough to press?* That question is
// unanswerable without a layout engine — which is why `.tsx` tests are banned here and why this is
// a `page.evaluate` over real boxes rather than an assertion about the DOM.
//
// It runs inside the route walk, on the visit the walk already makes, so covering every screen that
// has a filter surface costs no extra navigation. A screen that has none is simply not a subject.
//
// ⚠️ **These are floors, not opinions about the design.** Each one below names where its number
// comes from; a check with an invented threshold would be re-tuned the first time it inconvenienced
// someone, and then it would guard nothing.

import type { Page } from '@playwright/test';

/** One thing wrong with one control. `where` locates it for a human; `why` is the invariant. */
export interface FilterFinding {
  where: string;
  why: string;
}

/** `ColumnFilterCell.css` sets `.dt-f-trigger { height: 28px }`. A trigger shorter than its own
 *  declared height has been squeezed by whatever it was put inside. */
const TRIGGER_H = 28;

/** `ui-conventions.md`: "40px floor for secondary in-row controls". A filter trigger is one. Height
 *  is fixed by the component, so the axis this actually constrains is the width — which is the
 *  axis the Discovery defect collapsed (a filter cell placed in a 28px checkbox column). */
const TRIGGER_W = 40;

/** Sub-pixel grid resolution and border rounding both land here. Anything the shared
 *  `gridTemplateColumns` gets wrong is a whole track wide, never one pixel. */
const SLOP = 2;

export async function inspectFilterSurface(page: Page): Promise<FilterFinding[]> {
  return page.evaluate(
    ({ triggerH, triggerW, slop }) => {
      const out: { where: string; why: string }[] = [];
      const box = (el: Element) => el.getBoundingClientRect();
      const name = (el: Element) =>
        (el.getAttribute('aria-label') || el.textContent || '').trim().slice(0, 60) || '(unnamed)';

      // ── 1. The filter row is positional ──────────────────────────────────────────────────────
      // `DataTable.tsx` builds ONE `gridTemplateColumns` string and hands it to `.dt-head`,
      // `.dt-filters` and every `.dt-row`, with a comment saying that shared binding IS the guard.
      // It guards the string, not the result: a column whose `filter` is omitted still renders a
      // `.dt-f.empty` placeholder *precisely because* skipping it would shift every later control
      // under the wrong header. That is a claim about geometry, and this is where it is checked.
      for (const table of Array.from(document.querySelectorAll('.dt'))) {
        const filters = table.querySelector('.dt-filters');
        const head = table.querySelector('.dt-head');
        if (!filters) continue;
        if (!head) {
          out.push({
            where: '.dt-filters',
            why: 'a filter row with no header row above it',
          });
          continue;
        }
        const cells = Array.from(filters.children);
        const heads = Array.from(head.children);
        if (cells.length !== heads.length) {
          out.push({
            where: '.dt-filters',
            why: `${cells.length} filter cells under ${heads.length} column headers — the grid is positional, so every cell after the gap is under the wrong column`,
          });
          continue;
        }
        if (box(filters).top <= box(head).top) {
          out.push({
            where: '.dt-filters',
            why: 'the filter row is not below the header row',
          });
        }
        cells.forEach((cell, i) => {
          const c = box(cell);
          const h = box(heads[i]);
          if (c.width === 0 || h.width === 0) return; // an unrendered track, reported below
          if (c.left < h.left - slop || c.right > h.right + slop) {
            out.push({
              where: `column ${i} (${name(heads[i])})`,
              why: `the filter cell spans ${Math.round(c.left)}–${Math.round(c.right)} but its header spans ${Math.round(h.left)}–${Math.round(h.right)}`,
            });
          }
        });
      }

      // ── 2. Every trigger can be read and pressed ─────────────────────────────────────────────
      // Both halves matter and they fail separately. The node-tree bar rendered *as nothing* (a
      // wrapping flex row inside a fixed 38px header); the Discovery cell rendered as a visible
      // but useless sliver. A test that only asked "is it in the DOM" passed for both.
      for (const trigger of Array.from(document.querySelectorAll('.dt-f-trigger'))) {
        const b = box(trigger);
        const where = name(trigger);
        if (b.width === 0 || b.height === 0) {
          out.push({
            where,
            why: 'the trigger has no box at all — it is in the DOM and invisible',
          });
          continue;
        }
        if (b.height < triggerH - slop) {
          out.push({
            where,
            why: `${Math.round(b.height)}px tall; the component declares ${triggerH}px, so its container is squeezing it`,
          });
        }
        if (b.width < triggerW) {
          out.push({
            where,
            why: `${Math.round(b.width)}px wide — under the ${triggerW}px floor for an in-row control`,
          });
        }
        // `ColumnFilterCell.tsx` opens by saying what the trigger is *for*: with the controls
        // collapsed into a popover, the filter state is invisible unless the closed trigger keeps
        // saying it. `.dt-f-text` is `flex: 1; min-width: 0`, so a squeezed trigger loses the text
        // first and keeps the caret — it still looks like a control while having stopped being one.
        const text = trigger.querySelector('.dt-f-text');
        if (text && text.getBoundingClientRect().width === 0) {
          out.push({
            where,
            why: 'the trigger cannot show its own summary — the label is 0px wide',
          });
        }
        // The generic catch: covered, clipped by an `overflow: hidden` ancestor, or pushed under
        // another element. Skipped when the control is simply scrolled out of view, which is not a
        // defect — a long page legitimately has its filter row above the fold.
        const cx = b.left + b.width / 2;
        const cy = b.top + b.height / 2;
        const inView = cx >= 0 && cy >= 0 && cx <= window.innerWidth && cy <= window.innerHeight;
        if (inView) {
          const hit = document.elementFromPoint(cx, cy);
          if (!hit || !(trigger === hit || trigger.contains(hit))) {
            out.push({
              where,
              why: `nothing reaches the trigger at its own centre — ${hit ? `<${hit.tagName.toLowerCase()} class="${hit.className}">` : 'null'} is on top`,
            });
          }
        }
      }

      // ── 3. A bar that wraps out of its container ─────────────────────────────────────────────
      // `FilterBar.css` is `flex-wrap: wrap` on purpose, so the bar grows a second line rather than
      // overflowing sideways. Put inside a container with a fixed height, it grows *behind* it and
      // the operator sees nothing. The CSS comment had said "on its own row under the pane head"
      // from the day it was written, and the markup had never once been that.
      for (const bar of Array.from(document.querySelectorAll('.fbar'))) {
        const b = box(bar);
        const parent = bar.parentElement;
        if (!parent) continue;
        const p = box(parent);
        if (b.height > p.height + slop) {
          out.push({
            where: '.fbar',
            why: `the filter bar is ${Math.round(b.height)}px tall inside a ${Math.round(p.height)}px container — it is wrapping out of sight`,
          });
        }
      }

      return out;
    },
    { triggerH: TRIGGER_H, triggerW: TRIGGER_W, slop: SLOP },
  );
}

/** Screens that must have a filter surface. Absence is the failure this catches — a screen whose
 *  filter row stopped rendering looks *fine*, it just shows every row forever.
 *
 *  Deliberately a floor, not the full list: the walk asserts in the other direction too (any screen
 *  that renders a trigger is inspected, declared or not), so this only needs to name the ones whose
 *  disappearance would be silent. Each is a distinct shape of the ADR-053 work. */
export const MUST_FILTER: Record<string, string> = {
  '/nodes': 'the tree bar (Inc.6 decision E — a list with no header row)',
  '/nodes/discovery': 'the two discovery tables (Inc.6, and where the 28px defect landed)',
  '/nodes/mib': 'a server-side filter row: typing has to reach the request',
  '/alerts': 'FilterBar over an SSE-fed list (Inc.6)',
  '/alerts/events': 'the URL-backed filter row (Inc.2)',
  '/alerts/history': 'server-side, and the one that shipped never reaching the request',
  '/settings/users': 'the identity list — FilterBar, not a row',
  '/settings/audit': 'server-side filter row',
  '/troubleshoot/runs': 'FilterBar (Inc.6)',
  '/troubleshoot/findings': 'the number kind lives here (Score)',
};
