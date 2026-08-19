// SPDX-License-Identifier: AGPL-3.0-only
import { describe, expect, it } from 'vitest';
import {
  ABOVE_DIALOG_SELECTOR, FLOATING_SELECTOR, OVERLAY_SELECTOR, shouldDismissOnEscape } from './escapeDismiss';

/** The plain case: Escape on a page with nothing open and focus nowhere in particular. */
const plain = { key: 'Escape', overlayOpen: false, tagName: 'BODY', isEditable: false };

describe('shouldDismissOnEscape', () => {
  // 🚨 The accepting case comes first on purpose. A predicate that refused everything would pass a
  // suite made only of rejections, and this repo has shipped exactly that mistake before.
  it('clears the selection on a bare Escape', () => {
    expect(shouldDismissOnEscape(plain)).toBe(true);
  });

  it('clears when focus is on a button — a row or a toolbar control is not a text field', () => {
    expect(shouldDismissOnEscape({ ...plain, tagName: 'BUTTON' })).toBe(true);
    expect(shouldDismissOnEscape({ ...plain, tagName: 'DIV' })).toBe(true);
  });

  it('clears when the caller cannot name the focused element', () => {
    // `document.activeElement` is null on a page that has never been focused.
    expect(shouldDismissOnEscape({ ...plain, tagName: undefined })).toBe(true);
  });

  it('ignores every key but Escape', () => {
    for (const key of ['Enter', 'Tab', 'Backspace', 'Delete', 'ArrowUp', 'escape', 'Esc', ' ']) {
      expect(shouldDismissOnEscape({ ...plain, key })).toBe(false);
    }
  });

  it('defers to whatever is open above the page', () => {
    // The layering rule (ADR-073 decision 4): a modal, a popover or the tree's context menu gets
    // the press, and the selection behind it is left alone.
    expect(shouldDismissOnEscape({ ...plain, overlayOpen: true })).toBe(false);
  });

  it('leaves Escape to the field while the operator is typing', () => {
    for (const tagName of ['INPUT', 'TEXTAREA', 'SELECT']) {
      expect(shouldDismissOnEscape({ ...plain, tagName })).toBe(false);
    }
    expect(shouldDismissOnEscape({ ...plain, isEditable: true })).toBe(false);
  });

  it('reads the tag name case-insensitively', () => {
    // The DOM reports upper-case, but a caller that lower-cases it must not silently start
    // clearing the selection out from under a text box.
    expect(shouldDismissOnEscape({ ...plain, tagName: 'input' })).toBe(false);
  });
});

describe('the two selectors', () => {
  const floating = ['[role="dialog"]', '.apop', '.ntree-menu', '.ovm-menu', '.ts-run-menu'];

  it('name every floating layer in both', () => {
    // Pinned by name because the list is hand-maintained: dropping one of these is how Escape
    // starts clearing a selection through an open panel, and nothing else would notice.
    for (const part of floating) {
      expect(FLOATING_SELECTOR).toContain(part);
      expect(OVERLAY_SELECTOR).toContain(part);
    }
  });

  it('put the in-page surfaces in the outer selector only', () => {
    // The layering that makes the Interfaces dock work on the Nodes split: the dock outranks the
    // page's selection, but must not outrank *itself*, or Escape could never close it.
    expect(OVERLAY_SELECTOR).toContain('.nd-if-dock');
    expect(FLOATING_SELECTOR).not.toContain('.nd-if-dock');
  });

  it('keep the outer selector a superset of the floating one', () => {
    for (const part of FLOATING_SELECTOR.split(', ')) {
      expect(OVERLAY_SELECTOR).toContain(part);
    }
  });
});

describe('ABOVE_DIALOG_SELECTOR', () => {
  it('names every floating layer except a dialog', () => {
    // A modal asks "is anything stacked above me?" and must not answer yes because of itself —
    // that would make Escape unable to close any dialog at all.
    expect(ABOVE_DIALOG_SELECTOR).not.toContain('role="dialog"');
    expect(ABOVE_DIALOG_SELECTOR).toContain('.apop');
    for (const part of ABOVE_DIALOG_SELECTOR.split(', ')) {
      expect(FLOATING_SELECTOR).toContain(part);
    }
  });

  it('is exactly the floating layers minus dialogs, not a hand-written second list', () => {
    // Derived, so a fourth popover primitive added to FLOATING_LAYERS reaches the dialog rule too.
    const floating = FLOATING_SELECTOR.split(', ').filter((s) => s !== '[role="dialog"]');
    expect(ABOVE_DIALOG_SELECTOR.split(', ')).toEqual(floating);
  });
});
