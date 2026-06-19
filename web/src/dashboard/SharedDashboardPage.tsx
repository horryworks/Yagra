// Shared Dashboard — the global widget board (/dashboard), the customizable replacement for the old
// fixed "Overview". One layout shown to all users; **only admins** may customize it (the change
// applies to everyone, so entering edit mode requires a confirmation). Non-admins see it read-only.
//
// Reuses the My Dashboard machinery: it provides the shared store via LayoutStoreContext so
// WidgetFrame/CatalogModal edit the global layout, and borrows MyDashboardPage.css for the grid.
// Single-board (no board tabs).

import { useEffect, useState } from 'react';
import {
  DndContext,
  KeyboardSensor,
  PointerSensor,
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
import { useAuthStore } from '../store';
import { CatalogModal } from './CatalogModal';
import { LayoutStoreProvider } from './LayoutStoreContext';
import { useSharedLayoutStore } from './layoutStore';
import { WidgetFrame } from './WidgetFrame';
import './MyDashboardPage.css';
import './SharedDashboardPage.css';

export function SharedDashboardPage() {
  // One SSE subscription for the whole board (alert widgets read the shared store).
  useAlertStream();

  const isAdmin = useAuthStore((s) => s.role) === 'admin';

  const widgets = useSharedLayoutStore((s) => s.widgets);
  const status = useSharedLayoutStore((s) => s.status);
  const saveError = useSharedLayoutStore((s) => s.saveError);
  const dismissSaveError = useSharedLayoutStore((s) => s.dismissSaveError);
  const editing = useSharedLayoutStore((s) => s.editing);
  const setEditing = useSharedLayoutStore((s) => s.setEditing);
  const move = useSharedLayoutStore((s) => s.move);
  const load = useSharedLayoutStore((s) => s.load);
  const resetToDefault = useSharedLayoutStore((s) => s.resetToDefault);

  const [catalogOpen, setCatalogOpen] = useState(false);
  const [confirmReset, setConfirmReset] = useState(false);
  // Warn before entering edit mode: changes here are global (apply to every user).
  const [confirmEdit, setConfirmEdit] = useState(false);
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

  const sensors = useSensors(
    useSensor(PointerSensor, { activationConstraint: { distance: 4 } }),
    useSensor(KeyboardSensor, { coordinateGetter: sortableKeyboardCoordinates }),
  );

  const onDragEnd = (e: DragEndEvent) => {
    const { active, over } = e;
    if (!over || active.id === over.id) return;
    const ids = widgets.map((w) => w.instanceId);
    const from = ids.indexOf(String(active.id));
    const to = ids.indexOf(String(over.id));
    if (from >= 0 && to >= 0) move(from, to);
  };

  const actions = editing ? (
    <>
      <Button onClick={() => setCatalogOpen(true)}>Add widget</Button>
      <Button variant="ghost" onClick={() => setConfirmReset(true)}>
        Reset
      </Button>
      <Button variant="primary" onClick={() => setEditing(false)}>
        Done
      </Button>
    </>
  ) : (
    <Button
      onClick={() => setConfirmEdit(true)}
      disabled={!isAdmin}
      title={isAdmin ? undefined : 'Only admins can customize the shared dashboard'}
    >
      Customize
    </Button>
  );

  return (
    <LayoutStoreProvider store={useSharedLayoutStore}>
      <div>
        <PageHeader
          title="Shared dashboard"
          trail={[{ label: 'Dashboard' }, { label: 'Shared dashboard' }]}
          actions={actions}
        />

        {editing && (
          <div className="shared-dash-warning" role="status">
            Editing the shared dashboard — changes are visible to all users.
          </div>
        )}

        {saveError && (
          <div className="mydash-save-error" role="alert">
            <span>{saveError}</span>
            <Button variant="ghost" onClick={dismissSaveError}>
              Dismiss
            </Button>
          </div>
        )}

        {status === 'loading' && widgets.length === 0 ? (
          loadSlow ? (
            <div className="mydash-empty">
              <p className="muted">Loading the shared dashboard is taking longer than expected.</p>
              <Button variant="primary" onClick={() => void load()}>
                Retry
              </Button>
            </div>
          ) : (
            <p className="muted">Loading the shared dashboard…</p>
          )
        ) : widgets.length === 0 ? (
          <div className="mydash-empty">
            <p className="muted">The shared dashboard has no widgets yet.</p>
            {isAdmin && (
              <Button variant="primary" onClick={() => setConfirmEdit(true)}>
                Customize
              </Button>
            )}
          </div>
        ) : (
          <DndContext sensors={sensors} collisionDetection={closestCenter} onDragEnd={onDragEnd}>
            <SortableContext
              items={widgets.map((w) => w.instanceId)}
              strategy={rectSortingStrategy}
            >
              <div className="mydash-grid">
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
            title="Customize the shared dashboard?"
            onClose={() => setConfirmEdit(false)}
            footer={
              <>
                <Button onClick={() => setConfirmEdit(false)}>Cancel</Button>
                <Button
                  variant="primary"
                  onClick={() => {
                    setConfirmEdit(false);
                    setEditing(true);
                  }}
                >
                  Customize for everyone
                </Button>
              </>
            }
          >
            <p>
              Changes you make here apply to <strong>every user</strong> — everyone’s Shared
              Dashboard updates. Continue?
            </p>
          </Modal>
        )}

        {confirmReset && (
          <Modal
            title="Reset shared dashboard?"
            onClose={() => setConfirmReset(false)}
            footer={
              <>
                <Button onClick={() => setConfirmReset(false)}>Cancel</Button>
                <Button
                  variant="danger"
                  onClick={() => {
                    resetToDefault();
                    setConfirmReset(false);
                  }}
                >
                  Reset to default
                </Button>
              </>
            }
          >
            <p>
              This replaces the shared dashboard’s widgets with the default set — for all users. It
              can’t be undone.
            </p>
          </Modal>
        )}
      </div>
    </LayoutStoreProvider>
  );
}
