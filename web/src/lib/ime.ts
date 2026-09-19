// SPDX-License-Identifier: AGPL-3.0-only
// Is this key press part of an IME composition?
//
// WHY THIS EXISTS. The WebUI ships in Japanese, and Japanese is typed through an input method: the
// operator types kana, the IME offers conversions, and **Enter confirms the conversion**. That Enter
// is addressed to the IME, not to the page — but the page receives a `keydown` for it all the same.
// A handler that reads `e.key === 'Enter'` as "commit" therefore fires in the middle of typing: the
// node picker selected whatever row was highlighted for the half-typed text and closed; the label
// field stored the unconverted kana as a label. Arrow keys have the same problem — during a
// composition they move through the IME's candidates, not through the page's list.
//
// ⚠️ **One definition, because the two halves are easy to get half right.**
//   - `isComposing` is the standard answer, and Chromium and Firefox set it on the confirming Enter.
//   - Safari fires `compositionend` BEFORE that `keydown`, so `isComposing` is already false there.
//     What it still carries is the legacy `keyCode` 229, which every engine uses for "this key was
//     handled by the input method". Checking only the first half is a fix that does not work on a Mac.
//
// `lib/ime.test.ts` also fails the build for a file that reads `'Enter'` without calling this and
// without saying why it may (a handler that is not on a text field has no composition to respect).

/** The slice of a key event this needs — satisfied by React's synthetic event and by the DOM's. */
export interface ImeKeyEvent {
  /** DOM `KeyboardEvent`. */
  isComposing?: boolean;
  /** Legacy, deprecated, and the only signal Safari gives. */
  keyCode?: number;
  /** React's synthetic event keeps the DOM event here. */
  nativeEvent?: { isComposing?: boolean; keyCode?: number };
}

/** The `keyCode` every engine reports for a key the input method consumed. */
const IME_KEYCODE = 229;

/** True while the key press belongs to the input method rather than to the page. */
export function isImeComposing(e: ImeKeyEvent): boolean {
  const native = e.nativeEvent ?? e;
  return (
    native.isComposing === true ||
    e.isComposing === true ||
    native.keyCode === IME_KEYCODE ||
    e.keyCode === IME_KEYCODE
  );
}
