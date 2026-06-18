// One placed widget: a Card whose chrome the frame owns. In view mode the card shows the
// widget's own header actions (a selector, a "View all" link); in edit mode it shows the
// customize controls — a width selector, a remove (×), and a drag handle (dnd-kit). The grid
// span comes from the instance and maps to a `.mydash-span-N` class.

import { useSortable } from '@dnd-kit/sortable';
import { CSS } from '@dnd-kit/utilities';
import { Card } from '../components/ui/Card';
import { Select } from '../components/ui/Field';
import { useLayoutStore } from './layoutStore';
import { getDefinition } from './registry';
import type { Span, WidgetInstance, WidgetSettings } from './types';
import './WidgetFrame.css';

/** Friendly width labels for the span selector. */
const SPAN_LABEL: Record<Span, string> = { 4: '⅓ width', 6: '½ width', 8: '⅔ width', 12: 'Full width' };

export function WidgetFrame({ instance, editing }: { instance: WidgetInstance; editing: boolean }) {
  const setSpan = useLayoutStore((s) => s.setSpan);
  const removeWidget = useLayoutStore((s) => s.removeWidget);
  const setSettingsAction = useLayoutStore((s) => s.setSettings);
  const { attributes, listeners, setNodeRef, transform, transition, isDragging } = useSortable({
    id: instance.instanceId,
    disabled: !editing,
  });

  const def = getDefinition(instance.type);
  if (!def) return null;
  const Body = def.Component;
  const Actions = def.Actions;
  const setSettings = (patch: WidgetSettings) => setSettingsAction(instance.instanceId, patch);

  const actions = editing ? (
    <span className="widgetframe-edit">
      {def.allowedSpans.length > 1 && (
        <Select
          value={String(instance.span)}
          onChange={(e) => setSpan(instance.instanceId, Number(e.target.value))}
          aria-label="Widget width"
          title="Widget width"
        >
          {def.allowedSpans.map((s) => (
            <option key={s} value={s}>
              {SPAN_LABEL[s]}
            </option>
          ))}
        </Select>
      )}
      <button
        type="button"
        className="widgetframe-remove"
        onClick={() => removeWidget(instance.instanceId)}
        aria-label={`Remove ${def.title}`}
        title="Remove"
      >
        ×
      </button>
      <button
        type="button"
        className="widgetframe-handle"
        aria-label={`Drag ${def.title} to reorder`}
        title="Drag to reorder"
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
        `mydash-span-${instance.span}`,
        isDragging ? 'is-dragging' : '',
        editing ? 'is-editing' : '',
      ]
        .filter(Boolean)
        .join(' ')}
    >
      <Card title={def.title} actions={actions}>
        <Body instance={instance} setSettings={setSettings} />
      </Card>
    </div>
  );
}
