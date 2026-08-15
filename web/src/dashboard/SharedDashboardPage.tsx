// SPDX-License-Identifier: AGPL-3.0-only
// Shared Dashboard — the global widget board (/dashboard), the customizable replacement for the old
// fixed "Overview". One layout shown to all users; **only admins** may customize it (the change
// applies to everyone, so entering edit mode requires a confirmation). Non-admins see it read-only.
//
// Reuses the My Dashboard machinery: it provides the shared store via LayoutStoreContext so
// WidgetFrame/CatalogModal edit the global layout, and borrows MyDashboardPage.css for the grid.
// Single-board (no board tabs).

import { useEffect, useState } from 'react';
import { Trans, useTranslation } from 'react-i18next';
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
import { useCan } from '../store';
import { CatalogModal } from './CatalogModal';
import { LayoutStoreProvider } from './LayoutStoreContext';
import { useSharedLayoutStore } from './layoutStore';
import { WidgetFrame } from './WidgetFrame';
import './MyDashboardPage.css';
import './SharedDashboardPage.css';

export function SharedDashboardPage() {
  const { t } = useTranslation('dashboard');
  // One SSE subscription for the whole board (alert widgets read the shared store).
  useAlertStream();

  // `PUT /shared-dashboard` is ManageConfig — the board everyone sees is configuration.
  const canConfig = useCan('manage_config');

  const widgets = useSharedLayoutStore((s) => s.widgets);
  const status = useSharedLayoutStore((s) => s.status);
  const saveError = useSharedLayoutStore((s) => s.saveError);
  const dismissSaveError = useSharedLayoutStore((s) => s.dismissSaveError);
  const editing = useSharedLayoutStore((s) => s.editing);
  const setEditing = useSharedLayoutStore((s) => s.setEditing);
  const cancelEditing = useSharedLayoutStore((s) => s.cancelEditing);
  const isDirty = useSharedLayoutStore((s) => s.isDirty);
  const move = useSharedLayoutStore((s) => s.move);
  const load = useSharedLayoutStore((s) => s.load);
  const resetToDefault = useSharedLayoutStore((s) => s.resetToDefault);

  const [catalogOpen, setCatalogOpen] = useState(false);
  const [confirmReset, setConfirmReset] = useState(false);
  const [confirmCancel, setConfirmCancel] = useState(false);
  // Warn before entering edit mode: changes here are global (apply to every user).
  const [confirmEdit, setConfirmEdit] = useState(false);
  // True only while a reorder drag is in flight — disables dense packing so the sort stays linear.
  const [reordering, setReordering] = useState(false);
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

  // Mouse: 4px drag starts a reorder (unchanged). Touch: 250ms press-and-hold arms it so a normal
  // swipe still scrolls the board rather than dragging a tile. Keyboard reordering is unchanged.
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
  ) : canConfig ? (
    // Not `disabled` with an explanatory tooltip, which is what this was: a control nobody may use
    // is not drawn (ADR-056 Inc.2). A tooltip is hover-only, so on a phone the button read as
    // broken rather than as forbidden.
    <Button onClick={() => setConfirmEdit(true)}>{t('actions.customize')}</Button>
  ) : null;

  return (
    <LayoutStoreProvider store={useSharedLayoutStore}>
      <div>
        <PageHeader
          title={t('nav:dashboard.shared')}
          trail={[{ label: t('nav:sections.dashboard') }, { label: t('nav:dashboard.shared') }]}
          note={t('shared.pageNote')}
          actions={actions}
        />

        {editing && (
          <div className="shared-dash-warning" role="status">
            {t('shared.editingWarning')}
          </div>
        )}

        {saveError && (
          <div className="mydash-save-error" role="alert">
            <span>{saveError}</span>
            <Button variant="ghost" onClick={dismissSaveError}>
              {t('actions.dismiss')}
            </Button>
          </div>
        )}

        {status === 'loading' && widgets.length === 0 ? (
          loadSlow ? (
            <div className="mydash-empty">
              <p className="muted">{t('shared.loadSlow')}</p>
              <Button variant="primary" onClick={() => void load()}>
                {t('common:actions.retry')}
              </Button>
            </div>
          ) : (
            <p className="muted">{t('shared.loading')}</p>
          )
        ) : widgets.length === 0 ? (
          <div className="mydash-empty">
            <p className="muted">{t('shared.empty')}</p>
            {canConfig && (
              <Button variant="primary" onClick={() => setConfirmEdit(true)}>
                {t('actions.customize')}
              </Button>
            )}
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
              <div className={`mydash-grid${reordering ? ' is-reordering' : ''}`}>
                {widgets.map((w) => (
                  <WidgetFrame key={w.instanceId} instance={w} editing={editing} />
                ))}
              </div>
            </SortableContext>
          </DndContext>
        )}

        {catalogOpen && <CatalogModal onClose={() => setCatalogOpen(false)} />}

        {confirmEdit && (
          <Modal
            title={t('shared.confirmEditTitle')}
            onClose={() => setConfirmEdit(false)}
            footer={
              <>
                <Button onClick={() => setConfirmEdit(false)}>{t('common:actions.cancel')}</Button>
                <Button
                  variant="primary"
                  onClick={() => {
                    setConfirmEdit(false);
                    setEditing(true);
                  }}
                >
                  {t('shared.confirmEditConfirm')}
                </Button>
              </>
            }
          >
            <p className="modal-confirm-text">
              <Trans t={t} i18nKey="shared.confirmEditBody" components={{ strong: <strong /> }} />
            </p>
          </Modal>
        )}

        {confirmReset && (
          <Modal
            title={t('shared.resetTitle')}
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
            <p className="modal-confirm-text">{t('shared.resetBody')}</p>
          </Modal>
        )}

        {confirmCancel && (
          <Modal
            title={t('shared.cancelTitle')}
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
            <p className="modal-confirm-text">{t('shared.cancelBody')}</p>
          </Modal>
        )}
      </div>
    </LayoutStoreProvider>
  );
}
