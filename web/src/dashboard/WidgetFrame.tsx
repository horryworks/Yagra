// SPDX-License-Identifier: AGPL-3.0-only
// One placed widget: a Card whose chrome the frame owns. In view mode the card shows the widget's
// own header actions (a selector, a "View all" link); in edit mode it shows a remove (×) and a
// dnd-kit move handle (⠿) in the header, plus a bottom-right **resize grip** that drags width +
// height together, snapping to the widget's allowed span/rowSpan steps. The grid size comes from
// the instance (or, mid-drag, the live `preview`) and maps to `.mydash-span-N` / `.mydash-rowspan-N`
// classes on the cell.

import { useState } from 'react';
import { useSortable } from '@dnd-kit/sortable';
import { CSS } from '@dnd-kit/utilities';
import { useTranslation } from 'react-i18next';
import { Card } from '../components/ui/Card';
import { useLayoutStoreContext } from './LayoutStoreContext';
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

  const actions = editing ? (
    <span className="widgetframe-edit">
      <button
        type="button"
        className="widgetframe-remove"
        onClick={() => removeWidget(instance.instanceId)}
        aria-label={t('widgetFrame.remove', { name: t(def.title) })}
        title={t('common:actions.remove')}
      >
        ×
      </button>
      <button
        type="button"
        className="widgetframe-handle"
        aria-label={t('widgetFrame.drag', { name: t(def.title) })}
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
      <Card title={t(def.title)} actions={actions}>
        <Body instance={instance} setSettings={setSettings} />
      </Card>

      {editing && !isDragging && resizable && (
        <button
          type="button"
          className="widgetframe-resize"
          aria-label={t('widgetFrame.resize', { name: t(def.title) })}
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
