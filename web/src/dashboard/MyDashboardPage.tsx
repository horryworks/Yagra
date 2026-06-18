// My Dashboard — the customizable widget board (/dashboard/my). The fixed Overview
// (/dashboard) is unchanged; this is the per-user, add/remove/move board. Layout loads from and
// saves to the server (per account). The page owns the single alert SSE subscription so alert
// widgets stay live; each widget otherwise self-fetches. dnd-kit handles reordering (pointer +
// keyboard) in edit mode; widgets keep their declared 12-col span.

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
import { CatalogModal } from './CatalogModal';
import { useLayoutStore } from './layoutStore';
import { WidgetFrame } from './WidgetFrame';
import './MyDashboardPage.css';

export function MyDashboardPage() {
  // One SSE subscription for the whole board (alert widgets read the shared store).
  useAlertStream();

  const widgets = useLayoutStore((s) => s.widgets);
  const status = useLayoutStore((s) => s.status);
  const saveError = useLayoutStore((s) => s.saveError);
  const dismissSaveError = useLayoutStore((s) => s.dismissSaveError);
  const editing = useLayoutStore((s) => s.editing);
  const setEditing = useLayoutStore((s) => s.setEditing);
  const move = useLayoutStore((s) => s.move);
  const load = useLayoutStore((s) => s.load);
  const resetToDefault = useLayoutStore((s) => s.resetToDefault);

  const [catalogOpen, setCatalogOpen] = useState(false);
  const [confirmReset, setConfirmReset] = useState(false);
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
    <Button onClick={() => setEditing(true)}>Customize</Button>
  );

  return (
    <div>
      <PageHeader
        title="My dashboard"
        trail={[{ label: 'Dashboard' }, { label: 'My dashboard' }]}
        actions={actions}
      />

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
            <p className="muted">Loading your dashboard is taking longer than expected.</p>
            <Button variant="primary" onClick={() => void load()}>
              Retry
            </Button>
          </div>
        ) : (
          <p className="muted">Loading your dashboard…</p>
        )
      ) : widgets.length === 0 ? (
        <div className="mydash-empty">
          <p className="muted">Your dashboard is empty.</p>
          <Button
            variant="primary"
            onClick={() => {
              setEditing(true);
              setCatalogOpen(true);
            }}
          >
            Add your first widget
          </Button>
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

      {confirmReset && (
        <Modal
          title="Reset dashboard?"
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
          <p>This replaces your current widgets with the default set. It can’t be undone.</p>
        </Modal>
      )}
    </div>
  );
}
