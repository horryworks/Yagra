// One placed widget: a Card whose chrome the frame owns. In view mode the card shows the
// widget's own header actions (a selector, a "View all" link); in edit mode it shows the
// customize controls — a width selector, a remove (×), and a drag handle (dnd-kit). The grid
// span comes from the instance and maps to a `.mydash-span-N` class.

import { useSortable } from '@dnd-kit/sortable';
import { CSS } from '@dnd-kit/utilities';
import { useTranslation } from 'react-i18next';
import { Card } from '../components/ui/Card';
import { Select } from '../components/ui/Field';
import { useLayoutStoreContext } from './LayoutStoreContext';
import { getDefinition } from './registry';
import type { WidgetInstance, WidgetSettings } from './types';
import './WidgetFrame.css';

export function WidgetFrame({ instance, editing }: { instance: WidgetInstance; editing: boolean }) {
  const { t } = useTranslation('dashboard');
  const useStore = useLayoutStoreContext();
  const setSpan = useStore((s) => s.setSpan);
  const removeWidget = useStore((s) => s.removeWidget);
  const setSettingsAction = useStore((s) => s.setSettings);
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
          aria-label={t('widgetFrame.width')}
          title={t('widgetFrame.width')}
        >
          {def.allowedSpans.map((s) => (
            <option key={s} value={s}>
              {t(`widgetFrame.span.${s}`)}
            </option>
          ))}
        </Select>
      )}
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
        `mydash-span-${instance.span}`,
        isDragging ? 'is-dragging' : '',
        editing ? 'is-editing' : '',
      ]
        .filter(Boolean)
        .join(' ')}
    >
      <Card title={t(def.title)} actions={actions}>
        <Body instance={instance} setSettings={setSettings} />
      </Card>
    </div>
  );
}
