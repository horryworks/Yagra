// SPDX-License-Identifier: AGPL-3.0-only
// An explanation shown only when pressed (ADR-200): the ⓘ beside a field (`InfoTip`) and a label or
// badge that is itself the trigger (`InfoPress`).
//
// Why pressed and not hovered: a hover cannot be reached on touch, and a sentence that is always on
// screen is the prose ADR-200 removes. So the text is behind a button, opened by a click, a tap,
// Enter or Space, and closed by a second press, Escape or a press elsewhere.
//
// The text is a **namespaced key** (`infoKey="system:pollers.anchor.info"`), never children: a
// component that took its sentence as children would take any sentence, and the `.info` keys are
// what `proseBudget.test.ts` (G8) counts, caps at two sentences and holds to one call site each.
//
// The popover is `AnchoredPopover` with `role="dialog"`. Inside a `Modal` that matters twice: the
// popover is a floating layer, so Escape closes it and not the dialog (`escapeDismiss.ts`), and it
// carries no `aria-modal`, so the dialog's Tab trap keeps trapping while it is open (`Modal.tsx`).

import { useCallback, useRef, useState, type MouseEvent, type ReactNode } from 'react';
import { useTranslation } from 'react-i18next';
import { AnchoredPopover } from './AnchoredPopover';
import { focusPopoverTrigger } from './focusPopoverTrigger';
import { InfoIcon } from './icons';
import './InfoTip.css';

/** Open/close state shared by both triggers. The click is kept from the row or label around it:
 *  `preventDefault` so a wrapping `<label>` does not move focus to its control, `stopPropagation`
 *  so a clickable row does not navigate away. */
function useInfoPopover() {
  const [open, setOpen] = useState(false);
  const wrapRef = useRef<HTMLSpanElement>(null);
  const dismiss = useCallback((restoreFocus: boolean) => {
    setOpen(false);
    if (restoreFocus) focusPopoverTrigger(wrapRef.current, 'dialog');
  }, []);
  const toggle = useCallback((e: MouseEvent) => {
    e.preventDefault();
    e.stopPropagation();
    setOpen((o) => !o);
  }, []);
  return { open, wrapRef, dismiss, toggle };
}

/** The ⓘ beside a field label or a heading. `label` names what it explains ("About Owner"). */
export function InfoTip({ infoKey, label }: { infoKey: string; label: string }) {
  const { t } = useTranslation();
  const { open, wrapRef, dismiss, toggle } = useInfoPopover();
  const name = t('common:info.about', { label });
  return (
    <span className="infotip" ref={wrapRef}>
      <button
        type="button"
        className="infotip-btn"
        aria-haspopup="dialog"
        aria-expanded={open}
        aria-label={name}
        onClick={toggle}
      >
        <InfoIcon />
      </button>
      <AnchoredPopover
        open={open}
        anchorRef={wrapRef}
        role="dialog"
        label={name}
        align="start"
        className="infotip-pop"
        onDismiss={dismiss}
      >
        {t(infoKey)}
      </AnchoredPopover>
    </span>
  );
}

/** A label or badge that opens its own explanation. Text is drawn with a dotted underline; pass the
 *  badge's classes (`badge badge-warning`) and it is drawn as that badge with a `▾`.
 *
 *  `text` instead of `infoKey` is for an explanation that is not a `.info` key of this WebUI's: a
 *  metric's meaning, whose key is built from the metric name and whose sentence is generated from
 *  `metric_meaning.rs` (ADR-200 Inc.18). A file that passes `text` is listed, with its reason, in
 *  `proseBudget.test.ts` (G8) — it is not a way to put free prose behind a press. A number the
 *  sentence names (a configured window, say) goes in `values`, so the key stays the key G8 checks. */
export function InfoPress({
  className,
  children,
  ...source
}: (
  | { infoKey: string; values?: Record<string, string | number>; text?: never }
  | { text: string; infoKey?: never; values?: never }
) & {
  className?: string;
  children: ReactNode;
}) {
  const { t } = useTranslation();
  const { open, wrapRef, dismiss, toggle } = useInfoPopover();
  return (
    <span className="infotip" ref={wrapRef}>
      <button
        type="button"
        className={['infopress', className].filter(Boolean).join(' ')}
        aria-haspopup="dialog"
        aria-expanded={open}
        onClick={toggle}
      >
        {children}
      </button>
      <AnchoredPopover
        open={open}
        anchorRef={wrapRef}
        role="dialog"
        label={t('common:info.title')}
        align="start"
        className="infotip-pop"
        onDismiss={dismiss}
      >
        {source.infoKey === undefined ? source.text : t(source.infoKey, source.values)}
      </AnchoredPopover>
    </span>
  );
}
