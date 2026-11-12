// SPDX-License-Identifier: AGPL-3.0-only
// The one way a list is put in order (ADR-255): the ⠿ grip at the left of each row is dragged up or
// down. Nothing else on the row starts a drag, so a checkbox or a link in it keeps working.
//
//   <SortableList ids={ids} onReorder={setIds} nameOf={(id) => label(id)}>
//     {ids.map((id) => (
//       <SortableItem key={id} id={id} as="li" className="my-row">
//         <DragGrip name={label(id)} />
//         …the row…
//       </SortableItem>
//     ))}
//   </SortableList>
//
// The mouse starts a drag after 4px, a finger after a 250ms press (so a swipe still scrolls), and
// the keyboard with Space on the grip, then ↑ / ↓, then Space to drop or Esc to go back. Each step
// is read out in the viewer's language. `DataTable` takes `sortable` and does this for its rows.
// 🚨 A list with ↑ / ↓ buttons instead is refused by `sortable.test.ts`, and `useSortable` is used
// only here, in `DataTable` and in My Dashboard's `WidgetFrame` (a 2-D grid). The inventory tree
// keeps its own whole-row drag, because one gesture there both reorders and moves into a folder.

import {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useMemo,
  useRef,
  type CSSProperties,
  type ElementType,
  type ReactNode,
  type Ref,
} from 'react';
import { useTranslation } from 'react-i18next';
import { DndContext, closestCenter } from '@dnd-kit/core';
import { SortableContext, useSortable, verticalListSortingStrategy } from '@dnd-kit/sortable';
import { moveOnto } from '../../lib/reorder';
import { announce, verticalOnly, type Announcement } from './sortableOrder';
import { useSortableSensors } from './useSortableSensors';
import './Sortable.css';

interface SortableListProps {
  /** The items' ids in their current order. */
  ids: string[];
  /** Called once per drop that changed the order, with the new order. */
  onReorder: (ids: string[]) => void;
  /** What a screen reader calls an item while it is moved. */
  nameOf: (id: string) => string;
  children: ReactNode;
}

export function SortableList({ ids, onReorder, nameOf, children }: SortableListProps) {
  const { t } = useTranslation();
  const sensors = useSortableSensors();
  // Callers build `ids` and `nameOf` inline, so they are new every render; the announcements read
  // the latest through a ref and are built once per language instead of on every render.
  const latest = useRef({ ids, nameOf });
  useEffect(() => {
    latest.current = { ids, nameOf };
  });
  const accessibility = useMemo(() => {
    const say = (key: Announcement['key'], active: string, over: string | null) => {
      const a = announce(key, latest.current.ids, active, over, latest.current.nameOf);
      return t(`sortable.${a.key}`, { name: a.name, n: a.place, total: a.total });
    };
    type Id = { id: string | number };
    return {
      screenReaderInstructions: { draggable: t('sortable.instructions') },
      announcements: {
        onDragStart: ({ active }: { active: Id }) => say('picked', String(active.id), null),
        onDragOver: ({ active, over }: { active: Id; over: Id | null }) =>
          say('over', String(active.id), over ? String(over.id) : null),
        onDragEnd: ({ active, over }: { active: Id; over: Id | null }) =>
          say('dropped', String(active.id), over ? String(over.id) : null),
        onDragCancel: ({ active }: { active: Id }) => say('cancelled', String(active.id), null),
      },
    };
  }, [t]);
  return (
    <DndContext
      sensors={sensors}
      collisionDetection={closestCenter}
      modifiers={[verticalOnly]}
      accessibility={accessibility}
      onDragEnd={({ active, over }) => {
        const next = moveOnto(ids, String(active.id), over ? String(over.id) : null);
        if (next) onReorder(next);
      }}
    >
      <SortableContext items={ids} strategy={verticalListSortingStrategy}>
        {children}
      </SortableContext>
    </DndContext>
  );
}

/** What a row hands its grip: the listeners that start a drag, and where the drag starts from. */
interface GripWiring {
  attributes: ReturnType<typeof useSortable>['attributes'];
  listeners: ReturnType<typeof useSortable>['listeners'];
  setActivatorNodeRef: (el: HTMLElement | null) => void;
  disabled: boolean;
}

const GripContext = createContext<GripWiring | null>(null);

interface SortableItemProps {
  id: string;
  /** The element the row is. */
  as?: ElementType;
  className?: string;
  style?: CSSProperties;
  /** Where the row already stands, for a row a virtualized list positions itself (`DataTable`):
   *  set as `top`, so the drag's transform does not replace it. The row must be absolutely placed. */
  offsetY?: number;
  /** A ref of the caller's own, kept alongside the one the drag needs (a virtualizer's measure). */
  extraRef?: Ref<HTMLElement>;
  disabled?: boolean;
  onClick?: () => void;
  children: ReactNode;
  [attr: `data-${string}`]: string | number | undefined;
}

/** One row of a `SortableList`. Its `DragGrip` child is what starts the drag. */
export function SortableItem({
  id,
  as: Tag = 'div',
  className,
  style,
  offsetY,
  extraRef,
  disabled = false,
  onClick,
  children,
  ...data
}: SortableItemProps) {
  const { attributes, listeners, setNodeRef, setActivatorNodeRef, transform, transition, isDragging } =
    useSortable({ id, disabled });
  // 🚨 A row a virtualized list positions is placed with `top`, never with a transform: dnd-kit
  // measures the rows with their transforms left out, so rows placed by `translateY` all measured at
  // the top and a keyboard move skipped a row or went the wrong way. The transform is the drag's own.
  const dragY = transform?.y ?? 0;
  const placed: CSSProperties = {
    ...style,
    top: offsetY !== undefined ? `${offsetY}px` : style?.top,
    transform: dragY !== 0 ? `translateY(${dragY}px)` : undefined,
    transition,
  };
  // Stable, so React does not detach and re-attach the row on every render: during a drag every row
  // re-renders each frame, and a fresh callback made dnd-kit and the virtualizer drop and re-register
  // each one every time.
  const ref = useCallback(
    (el: HTMLElement | null) => {
      setNodeRef(el);
      if (typeof extraRef === 'function') extraRef(el);
      else if (extraRef && 'current' in extraRef) (extraRef as { current: HTMLElement | null }).current = el;
    },
    [setNodeRef, extraRef],
  );
  const wiring = useMemo(
    () => ({ attributes, listeners, setActivatorNodeRef, disabled }),
    [attributes, listeners, setActivatorNodeRef, disabled],
  );
  return (
    <Tag
      ref={ref}
      style={placed}
      className={[className, isDragging ? 'is-dragging' : ''].filter(Boolean).join(' ') || undefined}
      onClick={onClick}
      {...data}
    >
      <GripContext.Provider value={wiring}>{children}</GripContext.Provider>
    </Tag>
  );
}

/** The ⠿ at the left of a row: the only place a drag starts. `name` is what it moves. */
export function DragGrip({ name }: { name: string }) {
  const { t } = useTranslation();
  const wiring = useContext(GripContext);
  if (!wiring) throw new Error('DragGrip must be inside a SortableItem');
  return (
    <button
      type="button"
      className="drag-grip"
      ref={wiring.setActivatorNodeRef}
      disabled={wiring.disabled}
      {...wiring.attributes}
      {...wiring.listeners}
      aria-label={t('sortable.grip', { name })}
    >
      <span aria-hidden="true">⠿</span>
    </button>
  );
}
