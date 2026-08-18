// SPDX-License-Identifier: AGPL-3.0-only
// One placed widget: a Card whose chrome the frame owns. In view mode the card shows the widget's
// own header actions (a selector, a "View all" link); in edit mode it shows a remove (×) and a
// dnd-kit move handle (⠿) in the header, plus a bottom-right **resize grip** that drags width +
// height together, snapping to the widget's allowed span/rowSpan steps. The grid size comes from
// the instance (or, mid-drag, the live `preview`) and maps to `.mydash-span-N` / `.mydash-rowspan-N`
// classes on the cell.

import { useEffect, useState } from 'react';
import { useSortable } from '@dnd-kit/sortable';
import { CSS } from '@dnd-kit/utilities';
import { useTranslation } from 'react-i18next';
import { Card } from '../components/ui/Card';
import { useLayoutStoreContext } from './LayoutStoreContext';
import { widgetHeading, widgetLabel, WIDGET_TITLE_MAX } from './layout';
import { getDefinition } from './registry';
import { useResizeHandle } from './useResizeHandle';
import type { WidgetDefinition, WidgetInstance, WidgetSettings } from './types';
import './WidgetFrame.css';

// Placeholder definition so `useResizeHandle` can be called before the (rare) unknown-type early
// return — keeps hook order stable. Empty allowed-lists mean it produces no resizing.
const EMPTY_DEF = { allowedSpans: [], allowedRowSpans: [] } as unknown as WidgetDefinition;

export function WidgetFrame({ instance, editing }: { instance: WidgetInstance; editing: boolean }) {
  const { t } = useTranslation('dashboard');
  const useStore = useLayoutStoreContext();
  const setSize = useStore((s) => s.setSize);
  const removeWidget = useStore((s) => s.removeWidget);
  const setSettingsAction = useStore((s) => s.setSettings);
  const renameWidget = useStore((s) => s.renameWidget);
  const { attributes, listeners, setNodeRef, transform, transition, isDragging } = useSortable({
    id: instance.instanceId,
    disabled: !editing,
  });
  const [gripFocused, setGripFocused] = useState(false);

  const def = getDefinition(instance.type);
  // Hooks must run unconditionally, so resolve the resize hook before the (rare) unknown-type bail.
  const { handleProps, preview } = useResizeHandle(instance, def ?? EMPTY_DEF, setSize);
  if (!def) return null;

  const Body = def.Component;
  const Actions = def.Actions;
  const setSettings = (patch: WidgetSettings) => setSettingsAction(instance.instanceId, patch);

  // Effective size = live drag preview if any, else the persisted instance.
  const span = preview?.span ?? instance.span;
  const rowSpan = preview?.rowSpan ?? instance.rowSpan ?? 1;
  const resizable = def.allowedSpans.length > 1 || (def.allowedRowSpans?.length ?? 0) > 1;

  // The heading, split for display and joined for the labels. Both come from one resolver so the
  // screen and the screen reader cannot describe this card differently (ADR-071 decisions 0 and 6).
  const heading = widgetHeading(instance, t(def.title));
  const name = widgetLabel(instance, t(def.title));

  const actions = editing ? (
    <span className="widgetframe-edit">
      <button
        type="button"
        className="widgetframe-remove"
        onClick={() => removeWidget(instance.instanceId)}
        aria-label={t('widgetFrame.remove', { name })}
        title={t('common:actions.remove')}
      >
        ×
      </button>
      <button
        type="button"
        className="widgetframe-handle"
        aria-label={t('widgetFrame.drag', { name })}
        title={t('widgetFrame.dragTitle')}
        {...attributes}
        {...listeners}
      >
        ⠿
      </button>
    </span>
  ) : Actions ? (
    <Actions instance={instance} setSettings={setSettings} />
  ) : undefined;

  return (
    <div
      ref={setNodeRef}
      style={{ transform: CSS.Translate.toString(transform), transition }}
      className={[
        'mydash-cell',
        `mydash-span-${span}`,
        `mydash-rowspan-${rowSpan}`,
        isDragging ? 'is-dragging' : '',
        editing ? 'is-editing' : '',
        preview ? 'is-resizing' : '',
      ]
        .filter(Boolean)
        .join(' ')}
    >
      <Card
        title={
          <span className="widgetframe-title">
            <span className="widgetframe-kind">{heading.base}</span>
            {/* The separator is an element rather than a `::before`, because in edit mode the thing
                it precedes is an <input> — a replaced element, which generates no pseudo-elements.
                Keeping it out of the name string also means an unnamed card carries no stray mark
                and nobody copies one out of the box. `aria-hidden` because `widgetLabel` already
                puts the same character into the labels that name this card. */}
            {(editing || heading.own) && (
              <span className="widgetframe-sep" aria-hidden="true">
                ·
              </span>
            )}
            {editing ? (
              <WidgetName
                value={instance.title ?? ''}
                aria={t('widgetFrame.nameAria', { name: heading.base })}
                placeholder={t('widgetFrame.namePlaceholder')}
                onCommit={(v) => renameWidget(instance.instanceId, v)}
              />
            ) : (
              heading.own && <span className="widgetframe-own">{heading.own}</span>
            )}
          </span>
        }
        actions={actions}
      >
        <Body instance={instance} setSettings={setSettings} />
      </Card>

      {editing && !isDragging && resizable && (
        <button
          type="button"
          className="widgetframe-resize"
          aria-label={t('widgetFrame.resize', { name })}
          title={t('widgetFrame.resizeHint')}
          onFocus={() => setGripFocused(true)}
          onBlur={() => setGripFocused(false)}
          {...handleProps}
        >
          <span aria-hidden="true">⤡</span>
        </button>
      )}
      {preview && (
        <span className="widgetframe-readout" aria-hidden="true">
          {t('widgetFrame.sizeReadout', { w: span, h: rowSpan })}
        </span>
      )}
      {editing && resizable && (
        <span className="widgetframe-sr" aria-live="polite">
          {preview || gripFocused ? t('widgetFrame.sizeAnnounce', { w: span, h: rowSpan }) : ''}
        </span>
      )}
    </div>
  );
}

/**
 * The card heading while the board is being edited: a text box carrying this card's own name.
 *
 * Committed on blur or Enter rather than per keystroke, for the reason `MetricTopActions` gives
 * about its own field — every commit rewrites the layout document and re-renders the board, so
 * `up` and `upl` would each cost one. Escape abandons the edit, which is the only way back to the
 * previous name once a character has been typed.
 *
 * Empty is a real state, not a broken one: the type's name is drawn to its left either way, so a
 * card with no name of its own reads exactly as it did before ADR-071.
 */
function WidgetName({
  value,
  aria,
  placeholder,
  onCommit,
}: {
  value: string;
  aria: string;
  placeholder: string;
  onCommit: (v: string) => void;
}) {
  const [draft, setDraft] = useState(value);
  useEffect(() => setDraft(value), [value]);
  return (
    <input
      className="widgetframe-name"
      type="text"
      value={draft}
      maxLength={WIDGET_TITLE_MAX}
      placeholder={placeholder}
      aria-label={aria}
      title={aria}
      onChange={(e) => setDraft(e.target.value)}
      onBlur={() => onCommit(draft)}
      onKeyDown={(e) => {
        if (e.key === 'Enter') {
          e.preventDefault();
          onCommit(draft);
          e.currentTarget.blur();
        } else if (e.key === 'Escape') {
          // Stop here: the board's own Escape handling would otherwise read this as "leave edit
          // mode" and take the abandoned draft with it.
          e.stopPropagation();
          setDraft(value);
          e.currentTarget.blur();
        }
      }}
    />
  );
}
