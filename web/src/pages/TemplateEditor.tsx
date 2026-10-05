// SPDX-License-Identifier: AGPL-3.0-only
// The pieces of the visual notification-template editor (ADR-039 Inc.2): the editable field that
// holds text and variable tags, the list a tag is picked from, the panel that sets what a tag
// does when the alert lacks its value, and the note that says what a variable is.
//
// Layout and browser plumbing only. Every decision is in a `.ts` a test can reach: the template
// model (`templateModel.ts`), the DOM conversion (`templateDom.ts`), and the list's grouping and
// search (`templateVariables.ts`).

import { useEffect, useLayoutEffect, useRef, useState } from 'react';
import { createPortal } from 'react-dom';
import type { KeyboardEvent, MutableRefObject } from 'react';
import { useTranslation } from 'react-i18next';
import { AnchoredPopover } from '../components/ui/AnchoredPopover';
import { placeFromTrigger, type Placement, type Point } from '../components/ui/popoverPlacement';
import { Button } from '../components/ui/Button';
import { SearchField } from '../components/ui/SearchField';
import { isImeComposing } from '../lib/ime';
import type { Segment } from './templateModel';
import {
  chipNodes,
  chipSegment,
  insertNodes,
  isEmptiedField,
  paintChip,
  readSegments,
  renderSegments,
  textNodes,
  writeChip,
  CHIP_CLASS,
  type ChipLook,
} from './templateDom';
import { insertList, type TemplateVariableName } from './templateVariables';

/** What the dialog asks of a field it does not otherwise touch. */
export interface FieldHandle {
  insertVariable: (name: string) => void;
  /** Read the field again after a tag was edited in place. */
  reread: () => void;
}

export function TemplateField({
  id,
  segments,
  multiline,
  label,
  placeholder,
  look,
  handleRef,
  onChange,
  onOpenPicker,
  onChipClick,
}: {
  id: string;
  /** Drawn once, when the field mounts. The dialog remounts the field (a new `key`) when it
   *  replaces the row wholesale, because rewriting a field under the operator's caret loses it. */
  segments: readonly Segment[];
  multiline: boolean;
  label: string;
  placeholder: string;
  look: ChipLook;
  handleRef: MutableRefObject<FieldHandle | null>;
  onChange: (segments: Segment[]) => void;
  /** Open the variable list at the caret (`null`: below the field's insert button). */
  onOpenPicker: (at: Point | null) => void;
  onChipClick: (chip: HTMLElement) => void;
}) {
  const el = useRef<HTMLDivElement>(null);
  // The caret as it was last seen inside this field. Focus moves into the variable list's search
  // box before a tag is inserted, so the live selection is gone by then.
  const saved = useRef<Range | null>(null);

  useLayoutEffect(() => {
    if (el.current) renderSegments(el.current, segments, look);
    // Drawn on mount only; see `segments` above.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  useEffect(() => {
    const onSelection = () => {
      const sel = window.getSelection();
      if (!sel || sel.rangeCount === 0 || !el.current) return;
      const r = sel.getRangeAt(0);
      if (el.current.contains(r.startContainer)) saved.current = r.cloneRange();
    };
    document.addEventListener('selectionchange', onSelection);
    return () => document.removeEventListener('selectionchange', onSelection);
  }, []);

  const emit = () => {
    if (el.current) onChange(readSegments(el.current));
  };

  // The caret as it is right now when it is inside this field, else the last one seen. Read at the
  // moment of the key press: `selectionchange` arrives as a task of its own, so after fast typing
  // the remembered range can still sit before the text just typed - which put a tag in front of
  // the words it was meant to follow (found by the Tier1 walk).
  const caretNow = (): Range | null => {
    const sel = window.getSelection();
    if (sel && sel.rangeCount > 0 && el.current?.contains(sel.getRangeAt(0).startContainer)) {
      saved.current = sel.getRangeAt(0).cloneRange();
    }
    return saved.current;
  };

  const put = (nodes: Node[]) => {
    const field = el.current;
    if (!field) return;
    const caret = insertNodes(field, caretNow(), nodes);
    field.focus({ preventScroll: true });
    const sel = window.getSelection();
    sel?.removeAllRanges();
    sel?.addRange(caret);
    saved.current = caret.cloneRange();
    emit();
  };

  // Re-published after every render, so the dialog always calls the closures of the current one.
  useLayoutEffect(() => {
    handleRef.current = {
      insertVariable: (name) =>
        put(
          chipNodes(
            document,
            { kind: 'var', name, fallback: look.isOptional(name) ? '—' : '', hideLine: false },
            look,
          ),
        ),
      reread: emit,
    };
  });

  const onKeyDown = (e: KeyboardEvent<HTMLDivElement>) => {
    // Enter confirms a Japanese conversion, and "{" can arrive mid-composition too.
    if (isImeComposing(e)) return;
    if (e.key === '{') {
      e.preventDefault();
      const caret = caretNow()?.getBoundingClientRect() ?? null;
      // A collapsed range at a line start can measure as all zeros; fall back to the field.
      const box = caret && (caret.left || caret.top) ? caret : el.current?.getBoundingClientRect();
      onOpenPicker(box ? { x: box.left, y: box.bottom } : null);
      return;
    }
    if (e.key === 'Enter') {
      e.preventDefault();
      if (multiline) put([document.createElement('br')]);
    }
  };

  return (
    <div
      ref={el}
      id={id}
      className={multiline ? 'tpl-field is-multi' : 'tpl-field'}
      contentEditable
      suppressContentEditableWarning
      role="textbox"
      aria-multiline={multiline}
      aria-label={label}
      data-placeholder={placeholder}
      spellCheck={false}
      onInput={() => {
        const field = el.current;
        // A field emptied by deleting leaves a lone `<br>` that would hide the placeholder.
        if (field && isEmptiedField(field)) field.replaceChildren();
        emit();
      }}
      onKeyDown={onKeyDown}
      onPaste={(e) => {
        e.preventDefault();
        const raw = e.clipboardData.getData('text/plain');
        put(textNodes(document, multiline ? raw : raw.replace(/\r?\n/g, ' ')));
      }}
      onClick={(e) => {
        const chip = (e.target as HTMLElement).closest<HTMLElement>(`.${CHIP_CLASS}`);
        if (chip) {
          e.preventDefault();
          onChipClick(chip);
        }
      }}
    />
  );
}

/** The list a variable is picked from, with a search box. */
export function VariablePicker({
  anchorRef,
  at,
  isOptional,
  onPick,
  onClose,
}: {
  anchorRef?: MutableRefObject<HTMLElement | null>;
  at?: Point;
  isOptional: (name: string) => boolean;
  onPick: (name: TemplateVariableName) => void;
  onClose: () => void;
}) {
  const { t } = useTranslation('alertsConfig');
  const [query, setQuery] = useState('');
  const [active, setActive] = useState(0);
  const [placed, setPlaced] = useState(false);
  const input = useRef<HTMLInputElement>(null);
  const label = (n: TemplateVariableName) => t(`routing.template.vars.${n}.label`);
  const desc = (n: TemplateVariableName) => t(`routing.template.vars.${n}.desc`);
  const groups = insertList(query, label, desc);
  const flat = groups.flatMap((g) => g.names);

  useLayoutEffect(() => {
    if (placed) input.current?.focus({ preventScroll: true });
  }, [placed]);

  const onKeyDown = (e: KeyboardEvent<HTMLInputElement>) => {
    if (isImeComposing(e)) return;
    if (e.key === 'ArrowDown') {
      e.preventDefault();
      setActive((i) => Math.min(flat.length - 1, i + 1));
    } else if (e.key === 'ArrowUp') {
      e.preventDefault();
      setActive((i) => Math.max(0, i - 1));
    } else if (e.key === 'Enter') {
      e.preventDefault();
      const name = flat[active];
      if (name) onPick(name);
    }
  };

  return (
    <AnchoredPopover
      open
      anchorRef={anchorRef}
      at={at}
      role="listbox"
      label={t('routing.template.pickerLabel')}
      className="tpl-picker"
      onDismiss={onClose}
      onPlacedChange={setPlaced}
    >
      <SearchField
        inputRef={input}
        id="tpl-picker-search"
        boxClassName="tpl-picker-searchbox"
        className="tpl-picker-search"
        autoComplete="off"
        value={query}
        aria-label={t('routing.template.search')}
        placeholder={t('routing.template.search')}
        onChange={(e) => {
          setQuery(e.target.value);
          setActive(0);
        }}
        onClear={() => {
          setQuery('');
          setActive(0);
        }}
        onKeyDown={onKeyDown}
      />
      <div className="tpl-picker-list">
        {groups.length === 0 && <p className="tpl-picker-empty">{t('routing.template.noMatch')}</p>}
        {groups.map((g) => (
          <div key={g.group} role="group" aria-label={t(`routing.template.groups.${g.group}`)}>
            <div className="tpl-picker-group">{t(`routing.template.groups.${g.group}`)}</div>
            {g.names.map((n) => (
              <button
                key={n}
                type="button"
                role="option"
                aria-selected={flat[active] === n}
                className={flat[active] === n ? 'tpl-picker-item is-active' : 'tpl-picker-item'}
                // Keep focus in the search box: a mousedown on the button would move it there and
                // close the list from the popover's own outside-click handling.
                onMouseDown={(e) => e.preventDefault()}
                onClick={() => onPick(n)}
              >
                <span className="tpl-picker-label">
                  {label(n)}
                  {isOptional(n) && <span className="tpl-picker-opt">{t('routing.template.optionalMark')}</span>}
                </span>
                <span className="tpl-picker-desc">{desc(n)}</span>
              </button>
            ))}
          </div>
        ))}
      </div>
    </AnchoredPopover>
  );
}

/** What a tag does when the alert lacks its value, and removing it. Edits the tag in place. */
export function ChipSettings({
  chip,
  at,
  inSubject,
  look,
  onChanged,
  onClose,
}: {
  chip: HTMLElement;
  at: Point;
  inSubject: boolean;
  look: ChipLook;
  onChanged: () => void;
  onClose: () => void;
}) {
  const { t } = useTranslation('alertsConfig');
  const [seg, setSeg] = useState(() => chipSegment(chip));
  const optional = look.isOptional(seg.name);

  const apply = (next: typeof seg) => {
    setSeg(next);
    writeChip(chip, next);
    paintChip(chip, look);
    onChanged();
  };

  return (
    <AnchoredPopover open at={at} role="dialog" label={t('routing.template.chip.dialog')} className="tpl-chip-settings" onDismiss={onClose}>
      <div className="tpl-chip-head">{look.labelOf(seg.name)}</div>
      <code className="tpl-chip-code">{`{{ ${seg.name} }}`}</code>
      <p className="tpl-chip-desc">{t(`routing.template.vars.${seg.name}.desc`)}</p>
      <p className="tpl-chip-desc">{t(optional ? 'routing.template.chip.optional' : 'routing.template.chip.always')}</p>
      {optional && (
        <fieldset className="tpl-chip-missing">
          <legend>{t('routing.template.chip.missing')}</legend>
          <label className="tpl-chip-opt">
            <input
              type="radio"
              name="tpl-missing"
              id="tpl-missing-text"
              checked={!seg.hideLine && seg.prefix === undefined}
              onChange={() => apply({ ...seg, hideLine: false, prefix: undefined })}
            />
            {t('routing.template.chip.missText')}
            <input
              type="text"
              id="tpl-missing-fallback"
              className="tpl-chip-fallback"
              value={seg.fallback}
              onChange={(e) => apply({ ...seg, hideLine: false, prefix: undefined, fallback: e.target.value })}
            />
          </label>
          <label className={inSubject ? 'tpl-chip-opt is-disabled' : 'tpl-chip-opt'}>
            <input
              type="radio"
              name="tpl-missing"
              id="tpl-missing-hide"
              checked={seg.hideLine}
              disabled={inSubject}
              onChange={() => apply({ ...seg, hideLine: true, prefix: undefined })}
            />
            {t(inSubject ? 'routing.template.chip.missHideSubject' : 'routing.template.chip.missHide')}
          </label>
          <label className="tpl-chip-opt">
            <input
              type="radio"
              name="tpl-missing"
              id="tpl-missing-prefix"
              checked={!seg.hideLine && seg.prefix !== undefined}
              onChange={() => apply({ ...seg, hideLine: false, prefix: seg.prefix ?? '' })}
            />
            {t('routing.template.chip.missPrefix')}
            <input
              type="text"
              id="tpl-missing-prefix-text"
              className="tpl-chip-fallback"
              value={seg.prefix ?? ''}
              // One line: a newline here would turn the prefix into a line of its own.
              onChange={(e) => apply({ ...seg, hideLine: false, prefix: e.target.value.replace(/[\r\n]/g, '') })}
            />
          </label>
        </fieldset>
      )}
      <div className="tpl-chip-actions">
        <Button
          variant="outline"
          onClick={() => {
            chip.remove();
            onChanged();
            onClose();
          }}
        >
          {t('routing.template.chip.remove')}
        </Button>
      </div>
    </AnchoredPopover>
  );
}

/**
 * What a variable is, shown while the pointer rests on its button or the button has focus. Same
 * facts as the tag settings panel: the name in the operator's language, what it holds, the text
 * that will be inserted, and whether every alert carries it.
 *
 * Not an `AnchoredPopover`: that one is for panels the operator works in (it takes focus, closes on
 * an outside press). This one is read-only and ignores the pointer, so it can never get between the
 * operator and the button under it. It shares the placement arithmetic instead.
 */
export function VariableTooltip({
  id,
  anchor,
  name,
  snippet,
  optional,
}: {
  id: string;
  anchor: HTMLElement;
  name: string;
  /** The text a click inserts, which is what the operator will see in the field. */
  snippet: string;
  optional: boolean;
}) {
  const { t } = useTranslation('alertsConfig');
  const el = useRef<HTMLDivElement>(null);
  const [pos, setPos] = useState<Placement | null>(null);

  useLayoutEffect(() => {
    const tip = el.current;
    if (!tip) return;
    const r = tip.getBoundingClientRect();
    setPos(
      placeFromTrigger(
        anchor.getBoundingClientRect(),
        { width: r.width, height: r.height },
        { width: window.innerWidth, height: window.innerHeight },
        'start',
      ),
    );
  }, [anchor, name]);

  return createPortal(
    <div
      ref={el}
      id={id}
      role="tooltip"
      className="tpl-tip"
      style={pos ? { top: pos.top, left: pos.left } : { top: 0, left: 0, visibility: 'hidden' }}
    >
      <div className="tpl-chip-head">{t(`routing.template.vars.${name}.label`)}</div>
      <p className="tpl-chip-desc">{t(`routing.template.vars.${name}.desc`)}</p>
      <code className="tpl-chip-code">{snippet}</code>
      <p className="tpl-chip-desc">{t(optional ? 'routing.template.chip.optional' : 'routing.template.chip.always')}</p>
    </div>,
    document.body,
  );
}
