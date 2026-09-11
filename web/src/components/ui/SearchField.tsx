// SPDX-License-Identifier: AGPL-3.0-only
// The one search box (ADR-132). A controlled text entry that can empty itself: while it holds
// anything, a ✕ sits at its right edge and clears it.
//
// **It replaced `SearchInput.tsx` rather than sitting beside it.** That file was the picker box and
// this is every box, so keeping both would be two names for one thing (`extensibility.md` §5). What
// the old file said still holds and is worth repeating: **a list is narrowed by `ColumnFilterCell`
// under its header, or by `FilterBar` when it has none** — a bare text box cannot carry the mode
// toggle, NOT, the multi-select or the URL codec (ADR-053 Inc.7). This component is for a set of
// *choices* (the five pickers), for a surface with **no header row to hang a filter row under** (the
// node tree, the top bar's global search), and for the free-text cells the filter row itself draws.
//
// Three things in here are load-bearing:
//
//   - 🚨 **The ✕ prevents default on `mousedown` and acts on `click`.** Anything floating above the
//     page dismisses on *mousedown* (`AnchoredPopover`), so a plain `onClick` is a click that can
//     arrive after its own surface has gone. Holding the caret in the input means nothing blurs,
//     nothing dismisses, and there is no focus to restore afterwards.
//   - **`type="search"` everywhere, and the browser's own ✕ suppressed in CSS.** Blink and WebKit
//     draw `::-webkit-search-cancel-button` and Gecko draws nothing, so before this the answer to
//     "can I empty this box" depended on the browser. One drawn ✕ is the whole point; two is worse
//     than none.
//   - **The caller keeps the state.** `onClear` says the ✕ was pressed — it does not say what empty
//     means. `TextConditionEditor` commits immediately rather than after its 250ms debounce, because
//     clearing is a decision and not a keystroke.
//
// Where CSS goes, when a call site needs its box to look different (there are five shipped looks and
// this component deliberately does not unify them): **width and outer margins on the wrapper**
// (`boxClassName`), **border, background, height and inner padding on the input** (`className`).
// The ✕, the magnifier and the room reserved for the ✕ are this file's own.

import type { InputHTMLAttributes, Ref } from 'react';
import { useTranslation } from 'react-i18next';
import { SearchIcon } from './icons';
import './SearchField.css';

type Props = Omit<InputHTMLAttributes<HTMLInputElement>, 'type' | 'value'> & {
  value: string;
  /** Empty the box. The ✕ reports the press; the caller owns what "empty" writes. */
  onClear: () => void;
  /** Leading magnifier — and with it the 240px standalone-box width, since the five call sites that
   *  want one are exactly the five standalone boxes. Off by default. */
  icon?: boolean;
  /** Class for the wrapper. Width and outer margins belong here, not on the input. */
  boxClassName?: string;
  inputRef?: Ref<HTMLInputElement>;
};

export function SearchField({
  value,
  onClear,
  icon = false,
  boxClassName,
  className,
  inputRef,
  ...rest
}: Props) {
  const { t } = useTranslation('common');
  return (
    <div className={['sfield', icon ? 'sfield-icon' : '', boxClassName].filter(Boolean).join(' ')}>
      {icon && <SearchIcon />}
      <input
        ref={inputRef}
        type="search"
        className={['sfield-input', className].filter(Boolean).join(' ')}
        value={value}
        {...rest}
        aria-label={rest['aria-label'] ?? t('actions.search')}
      />
      {/* Outside the input, never nested in it, and mounted only while there is something to clear —
          a ✕ that is always there is a control that does nothing half the time. */}
      {value !== '' && (
        <button
          type="button"
          className="sfield-clear"
          // Out of the tab order on purpose: Ctrl+A then Delete already empties the box from the
          // keyboard, and a stop here would be one more in every box — five of them in popovers
          // whose tab order is the operator's way through a list of options. Still announced.
          tabIndex={-1}
          aria-label={t('actions.clearSearch')}
          onMouseDown={(e) => e.preventDefault()}
          onClick={onClear}
        >
          ✕
        </button>
      )}
    </div>
  );
}
