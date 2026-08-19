// SPDX-License-Identifier: AGPL-3.0-only
// My Dashboard — the customizable widget board (/dashboard/my). The fixed Overview
// (/dashboard) is unchanged; this is the per-user, add/remove/move board. Layout loads from and
// saves to the server (per account). The page owns the single alert SSE subscription so alert
// widgets stay live; each widget otherwise self-fetches. dnd-kit handles reordering (pointer +
// keyboard) in edit mode; widgets keep their declared 12-col span.

import { useEffect, useState } from 'react';
import { useTranslation } from 'react-i18next';
import {
  DndContext,
  KeyboardSensor,
  MouseSensor,
  TouchSensor,
  closestCenter,
  useSensor,
  useSensors,
  type DragEndEvent,
} from '@dnd-kit/core';
import {
  SortableContext,
  rectSortingStrategy,
  sortableKeyboardCoordinates,
} from '@dnd-kit/sortable';
import { Button } from '../components/ui/Button';
import { Modal } from '../components/ui/Modal';
import { PageHeader } from '../components/ui/PageHeader';
import { useAlertStream } from '../hooks/useAlertStream';
import { escapeClearsSelection } from '../lib/escapeDismiss';
import { BoardTabs } from './BoardTabs';
import { CatalogModal } from './CatalogModal';
import { LayoutStoreProvider } from './LayoutStoreContext';
import { useLayoutStore } from './layoutStore';
import { WidgetFrame } from './WidgetFrame';
import './MyDashboardPage.css';

export function MyDashboardPage() {
  const { t } = useTranslation('dashboard');
  // One SSE subscription for the whole board (alert widgets read the shared store).
  useAlertStream();

  const widgets = useLayoutStore((s) => s.widgets);
  const status = useLayoutStore((s) => s.status);
  const saveError = useLayoutStore((s) => s.saveError);
  const dismissSaveError = useLayoutStore((s) => s.dismissSaveError);
  const editing = useLayoutStore((s) => s.editing);
  const setEditing = useLayoutStore((s) => s.setEditing);
  const cancelEditing = useLayoutStore((s) => s.cancelEditing);
  const isDirty = useLayoutStore((s) => s.isDirty);
  const move = useLayoutStore((s) => s.move);
  const load = useLayoutStore((s) => s.load);
  const resetToDefault = useLayoutStore((s) => s.resetToDefault);

  const [catalogOpen, setCatalogOpen] = useState(false);
  const [confirmReset, setConfirmReset] = useState(false);
  const [confirmCancel, setConfirmCancel] = useState(false);
  // True only while a reorder drag is in flight — disables dense packing so the sort stays linear.
  const [reordering, setReordering] = useState(false);
  // If the load hangs (no response), don't spin forever — surface a retry after a grace period.
  const [loadSlow, setLoadSlow] = useState(false);

  useEffect(() => {
    void load();
  }, [load]);

  useEffect(() => {
    if (status !== 'loading') {
      setLoadSlow(false);
      return;
    }
    const t = setTimeout(() => setLoadSlow(true), 12_000);
    return () => clearTimeout(t);
  }, [status]);

  // Split the pointer into per-input sensors so touch and mouse can activate differently. Mouse:
  // a 4px drag starts a reorder (unchanged desktop feel). Touch: a 250ms press-and-hold arms the
  // drag (a `distance` constraint would hijack vertical scroll on a phone) — a normal swipe still
  // scrolls the board. Keyboard reordering is unchanged.
  const sensors = useSensors(
    useSensor(MouseSensor, { activationConstraint: { distance: 4 } }),
    useSensor(TouchSensor, { activationConstraint: { delay: 250, tolerance: 5 } }),
    useSensor(KeyboardSensor, { coordinateGetter: sortableKeyboardCoordinates }),
  );

  const onDragEnd = (e: DragEndEvent) => {
    setReordering(false);
    const { active, over } = e;
    if (!over || active.id === over.id) return;
    const ids = widgets.map((w) => w.instanceId);
    const from = ids.indexOf(String(active.id));
    const to = ids.indexOf(String(over.id));
    if (from >= 0 && to >= 0) move(from, to);
  };

  // Cancel = discard this session's edits. Confirm only when something actually changed.
  const onCancel = () => {
    if (isDirty()) setConfirmCancel(true);
    else cancelEditing();
  };

  // Escape leaves edit mode, through that same handler so a dirty board still asks before it
  // discards anything (ADR-073). `WidgetFrame`'s Escape branch has always carried a comment saying
  // it stops "the board's own Escape handling" from taking an abandoned title draft with it — the
  // board had no such handling, so that `stopPropagation()` guarded nothing. It does now.
  // The body is inlined rather than calling `onCancel`, which is a fresh closure every render and
  // would re-subscribe the listener on each one.
  useEffect(() => {
    if (!editing) return;
    const onKey = (e: KeyboardEvent) => {
      if (!escapeClearsSelection(e)) return;
      if (isDirty()) setConfirmCancel(true);
      else cancelEditing();
    };
    document.addEventListener('keydown', onKey);
    return () => document.removeEventListener('keydown', onKey);
  }, [editing, isDirty, cancelEditing]);

  const actions = editing ? (
    <>
      <Button onClick={() => setCatalogOpen(true)}>{t('actions.addWidget')}</Button>
      <Button variant="ghost" onClick={() => setConfirmReset(true)}>
        {t('common:actions.reset')}
      </Button>
      <Button variant="ghost" onClick={onCancel}>
        {t('common:actions.cancel')}
      </Button>
      <Button variant="primary" onClick={() => setEditing(false)}>
        {t('actions.done')}
      </Button>
    </>
  ) : (
    <Button onClick={() => setEditing(true)}>{t('actions.customize')}</Button>
  );

  return (
    <LayoutStoreProvider store={useLayoutStore}>
      <div>
        <PageHeader
          title={t('nav:dashboard.my')}
          trail={[{ label: t('nav:sections.dashboard') }, { label: t('nav:dashboard.my') }]}
          note={t('my.pageNote')}
          actions={actions}
        />

        {saveError && (
          <div className="mydash-save-error" role="alert">
            <span>{saveError}</span>
            <Button variant="ghost" onClick={dismissSaveError}>
              {t('actions.dismiss')}
            </Button>
          </div>
        )}

        {status !== 'loading' && <BoardTabs editing={editing} />}

        {status === 'loading' && widgets.length === 0 ? (
          loadSlow ? (
            <div className="mydash-empty">
              <p className="muted">{t('my.loadSlow')}</p>
              <Button variant="primary" onClick={() => void load()}>
                {t('common:actions.retry')}
              </Button>
            </div>
          ) : (
            <p className="muted">{t('my.loading')}</p>
          )
        ) : widgets.length === 0 ? (
          <div className="mydash-empty">
            <p className="muted">{t('my.empty')}</p>
            <Button
              variant="primary"
              onClick={() => {
                setEditing(true);
                setCatalogOpen(true);
              }}
            >
              {t('my.addFirst')}
            </Button>
          </div>
        ) : (
          <DndContext
            sensors={sensors}
            collisionDetection={closestCenter}
            onDragStart={() => setReordering(true)}
            onDragEnd={onDragEnd}
            onDragCancel={() => setReordering(false)}
          >
            <SortableContext
              items={widgets.map((w) => w.instanceId)}
              strategy={rectSortingStrategy}
            >
              <div
                className={`mydash-grid${editing ? ' is-editing' : ''}${
                  reordering ? ' is-reordering' : ''
                }`}
              >
                {widgets.map((w) => (
                  <WidgetFrame key={w.instanceId} instance={w} editing={editing} />
                ))}
              </div>
            </SortableContext>
          </DndContext>
        )}

        {catalogOpen && <CatalogModal onClose={() => setCatalogOpen(false)} />}

        {confirmReset && (
          <Modal
            title={t('my.resetTitle')}
            onClose={() => setConfirmReset(false)}
            footer={
              <>
                <Button onClick={() => setConfirmReset(false)}>{t('common:actions.cancel')}</Button>
                <Button
                  variant="danger"
                  onClick={() => {
                    resetToDefault();
                    setConfirmReset(false);
                  }}
                >
                  {t('actions.resetToDefault')}
                </Button>
              </>
            }
          >
            <p className="modal-confirm-text">{t('my.resetBody')}</p>
          </Modal>
        )}

        {confirmCancel && (
          <Modal
            title={t('my.cancelTitle')}
            onClose={() => setConfirmCancel(false)}
            footer={
              <>
                <Button onClick={() => setConfirmCancel(false)}>{t('actions.keepEditing')}</Button>
                <Button
                  variant="danger"
                  onClick={() => {
                    cancelEditing();
                    setConfirmCancel(false);
                  }}
                >
                  {t('actions.discard')}
                </Button>
              </>
            }
          >
            <p className="modal-confirm-text">{t('my.cancelBody')}</p>
          </Modal>
        )}
      </div>
    </LayoutStoreProvider>
  );
}
