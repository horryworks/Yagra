// SPDX-License-Identifier: AGPL-3.0-only
// The geometry of a filter surface, inspected in a browser (ADR-053 × ADR-052, ADR-228).
//
// WHY THIS FILE EXISTS. ADR-053 shipped with 1,966 green unit tests and **six defects were still
// found by eye**. Re-read as a set, four of the six are one question nothing could ask: *is the
// control I added actually there, beside the right column, big enough to press?* That question is
// unanswerable without a layout engine — which is why `.tsx` tests are banned here and why this is
// a `page.evaluate` over real boxes rather than an assertion about the DOM.
//
// Since ADR-228 the controls are funnels: one beside each filterable header (`.hf-trigger`), or
// one in the toolbar of a list drawn without a header (`.mfilt-menu`). The questions are the same.
//
// It runs inside the route walk, on the visit the walk already makes, so covering every screen that
// has a filter surface costs no extra navigation. A screen that has none is simply not a subject.
//
// ⚠️ **These are floors, not opinions about the design.** Each one below names where its number
// comes from; a check with an invented threshold would be re-tuned the first time it inconvenienced
// someone, and then it would guard nothing.

import type { Page } from '@playwright/test';

/** Every desktop control that opens a filter: a header's funnel, or the toolbar's funnel of a list
 *  with no header. On a phone neither is drawn and `.mfilt-btn` opens the sheet instead. */
export const FILTER_TRIGGER = '.hf-trigger, .mfilt-menu > .mfilt-btn';

export interface FilterFinding {
  where: string;
  why: string;
}

/** `HeaderFilter.css` draws the funnel 22×22. Below that the header is squeezing it. */
const FUNNEL = 22;

/** The header label beside a funnel must keep some room: a funnel that pushes its label to 0px
 *  has made the column nameless. Twelve pixels is one glyph and its ellipsis. */
const LABEL_FLOOR = 12;

/** Sub-pixel rounding between two boxes laid out by one engine. A real defect is never this small. */
const SLOP = 2;

export async function inspectFilterSurface(page: Page): Promise<FilterFinding[]> {
  return page.evaluate(
    ({ funnel, labelFloor, slop, selector }) => {
      const out: { where: string; why: string }[] = [];
      const box = (el: Element) => el.getBoundingClientRect();
      const name = (el: Element) =>
        (el.getAttribute('aria-label') || el.textContent || '').trim().slice(0, 60) || '(unnamed)';

      for (const trigger of Array.from(document.querySelectorAll(selector))) {
        const b = box(trigger);
        const where = name(trigger);
        if (b.width === 0 || b.height === 0) {
          out.push({ where, why: 'the funnel has no box at all — it is in the DOM and invisible' });
          continue;
        }
        if (trigger.classList.contains('hf-trigger')) {
          if (b.width < funnel - slop || b.height < funnel - slop) {
            out.push({
              where,
              why: `${Math.round(b.width)}×${Math.round(b.height)}px; the component declares ${funnel}px, so the header is squeezing it`,
            });
          }
          // Beside its own header, inside it: a funnel laid out over the next column would read as
          // that column's.
          const cell = trigger.closest('.hcell');
          if (!cell) {
            out.push({ where, why: 'a header funnel outside a header cell' });
          } else {
            const c = box(cell);
            if (b.left < c.left - slop || b.right > c.right + slop) {
              out.push({
                where,
                why: `the funnel spans ${Math.round(b.left)}–${Math.round(b.right)} but its header spans ${Math.round(c.left)}–${Math.round(c.right)}`,
              });
            }
            const label = cell.querySelector('.hcell-label');
            if (label && box(label).width < labelFloor) {
              out.push({
                where,
                why: `the funnel left its header label ${Math.round(box(label).width)}px — the column has lost its name`,
              });
            }
          }
        }
        const cx = b.left + b.width / 2;
        const cy = b.top + b.height / 2;
        // Strictly less than: the last row of pixels is `innerHeight - 1`, and a point AT
        // `innerHeight` is outside the window, where `elementFromPoint` answers null. A funnel
        // whose centre landed exactly on the bottom edge read as covered (Pollers, when ADR-246
        // moved its table up 6px, from 726 to 720).
        const inView = cx >= 0 && cy >= 0 && cx < window.innerWidth && cy < window.innerHeight;
        if (inView) {
          const hit = document.elementFromPoint(cx, cy);
          if (!hit || !(trigger === hit || trigger.contains(hit))) {
            out.push({
              where,
              why: `nothing reaches the funnel at its own centre — ${hit ? `<${hit.tagName.toLowerCase()} class="${hit.className}">` : 'null'} is on top`,
            });
          }
        }
      }
      return out;
    },
    { funnel: FUNNEL, labelFloor: LABEL_FLOOR, slop: SLOP, selector: FILTER_TRIGGER },
  );
}

/** Screens that must have a filter control. Absence is the failure this catches — a screen whose
 *  filters stopped rendering looks *fine*, it just shows every row forever.
 *
 *  Deliberately a floor, not the full list: the walk asserts in the other direction too (any screen
 *  that renders a funnel is inspected, declared or not), so this only needs to name the ones whose
 *  disappearance would be silent. Each is a distinct shape of the work. */
export const MUST_FILTER: Record<string, string> = {
  // `/nodes` left this list with ADR-177: its State / Kind / Pool moved into the inventory's filter
  // popover, which is closed at rest, so there is no trigger for the walk to find. The popover is
  // pressed by `treeAttention.spec.ts` and `treeWithNodes.spec.ts` instead.
  '/nodes/discovery': 'the two hand-built discovery tables (where the 28px defect landed)',
  '/nodes/collection/mib': 'a server-side filter: typing has to reach the request',
  '/nodes/collection': 'a grouped `.ytable`, header funnels plus the toolbar funnel for category',
  '/alerts': 'a browser-held list over SSE, header funnels since ADR-243',
  '/events': 'URL-backed header funnels',
  '/alerts/history': 'server-side, and the one that shipped never reaching the request',
  '/settings/users': 'the identity list — no header, a toolbar funnel',
  '/settings/audit': 'server-side header funnels',
  '/troubleshoot/runs': 'a header-less list, a toolbar funnel',
  '/troubleshoot/findings': 'the number kind lives here (Score)',
};
