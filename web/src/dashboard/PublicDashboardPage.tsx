// SPDX-License-Identifier: AGPL-3.0-only
// Public Dashboard — the one board an anonymous visitor sees (/dashboard/public, ADR-123).
//
// It looks like SharedDashboardPage and is not the same thing. What this board carries decides
// which API routes an unauthenticated request may reach, so:
//
//  - editing takes `manage_system` (Admin), not the `manage_config` the shared board takes;
//  - the catalog offers only widgets an anonymous visitor can actually load;
//  - a banner says, permanently, that this is visible from outside — ADR-055 R6, a deliberate
//    property stated where the person doing the work is looking;
//  - and there is a **view as anonymous** toggle, which is the only way to find a widget whose
//    `reads` declaration is wrong. An admin's own session answers every call, so a missing
//    declaration is invisible from here until a stranger hits it.
//
// The same page serves the anonymous visitor, without the header, the banner or any control —
// `PublicShell` renders it with `viewerOnly`.

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
import { api, setAnonymousPreview } from '../services/api';
import { useCan } from '../store';
import { CatalogModal } from './CatalogModal';
import { LayoutStoreProvider } from './LayoutStoreContext';
import { usePublicLayoutStore } from './layoutStore';
import { WidgetFrame } from './WidgetFrame';
import './MyDashboardPage.css';
import './SharedDashboardPage.css';

/** Rendered without any chrome for the anonymous visitor (`PublicShell`). */
export interface PublicDashboardPageProps {
  /** No header, no banner, no controls — just the board. */
  viewerOnly?: boolean;
}

export function PublicDashboardPage({ viewerOnly = false }: PublicDashboardPageProps) {
  const { t } = useTranslation('dashboard');
  useAlertStream();

  // `PUT /public-dashboard` is ManageSystem: composing this board widens what strangers can read.
  const canSystem = useCan('manage_system');

  const widgets = usePublicLayoutStore((s) => s.widgets);
  const status = usePublicLayoutStore((s) => s.status);
  const saveError = usePublicLayoutStore((s) => s.saveError);
  const dismissSaveError = usePublicLayoutStore((s) => s.dismissSaveError);
  const editing = usePublicLayoutStore((s) => s.editing);
  const setEditing = usePublicLayoutStore((s) => s.setEditing);
  const cancelEditing = usePublicLayoutStore((s) => s.cancelEditing);
  const isDirty = usePublicLayoutStore((s) => s.isDirty);
  const move = usePublicLayoutStore((s) => s.move);
  const load = usePublicLayoutStore((s) => s.load);

  const [catalogOpen, setCatalogOpen] = useState(false);
  const [confirmCancel, setConfirmCancel] = useState(false);
  const [reordering, setReordering] = useState(false);
  const [preview, setPreview] = useState(false);
  const [switchState, setSwitchState] = useState<{ enabled: boolean; routes: number } | null>(null);

  useEffect(() => {
    void load();
  }, [load]);

  // Whether the deployment is actually serving this board. The editor says so plainly: composing a
  // board that nobody can see is a common way to spend an afternoon.
  useEffect(() => {
    if (viewerOnly) return;
    api
      .getPublicDashboardSwitch()
      .then((s) => setSwitchState({ enabled: s.enabled, routes: s.route_count }))
      .catch(() => setSwitchState(null));
  }, [viewerOnly, widgets]);

  // 🚨 The preview is process-wide (every request loses its credential), so it must not survive
  // this screen. Clearing on unmount is unconditional — a navigation away while it is on would
  // otherwise leave the whole app signed out in effect, with no visible cause.
  useEffect(() => () => setAnonymousPreview(false), []);

  const togglePreview = (on: boolean) => {
    // Leaving edit mode first is not tidiness: a save issued without a credential is refused, and
    // the operator would see a failure the screen never explained.
    if (on) setEditing(false);
    setAnonymousPreview(on);
    setPreview(on);
    void load();
  };

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

  const onCancel = () => {
    if (isDirty()) setConfirmCancel(true);
    else cancelEditing();
  };

  const board = (
    <>
      {saveError && (
        <div className="mydash-save-error" role="alert">
          <span>{saveError}</span>
          <Button variant="ghost" onClick={dismissSaveError}>
            {t('actions.dismiss')}
          </Button>
        </div>
      )}

      {status === 'loading' && widgets.length === 0 ? (
        <p className="muted">{t('public.loading')}</p>
      ) : widgets.length === 0 ? (
        <div className="mydash-empty">
          <p className="muted">{viewerOnly ? t('public.emptyViewer') : t('public.empty')}</p>
          {!viewerOnly && canSystem && !editing && (
            <Button variant="primary" onClick={() => setEditing(true)}>
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
          <SortableContext items={widgets.map((w) => w.instanceId)} strategy={rectSortingStrategy}>
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
    </>
  );

  if (viewerOnly) {
    return <LayoutStoreProvider store={usePublicLayoutStore}>{board}</LayoutStoreProvider>;
  }

  const actions = editing ? (
    <>
      <Button onClick={() => setCatalogOpen(true)}>{t('actions.addWidget')}</Button>
      <Button variant="ghost" onClick={onCancel}>
        {t('common:actions.cancel')}
      </Button>
      <Button variant="primary" onClick={() => setEditing(false)}>
        {t('actions.done')}
      </Button>
    </>
  ) : canSystem ? (
    <>
      <Button variant={preview ? 'primary' : 'ghost'} onClick={() => togglePreview(!preview)}>
        {preview ? t('public.previewExit') : t('public.preview')}
      </Button>
      {!preview && <Button onClick={() => setEditing(true)}>{t('actions.customize')}</Button>}
    </>
  ) : null;

  return (
    <LayoutStoreProvider store={usePublicLayoutStore}>
      <div>
        <PageHeader
          title={t('nav:dashboard.public')}
          trail={[{ label: t('nav:sections.dashboard') }, { label: t('nav:dashboard.public') }]}
          note={t('public.pageNote')}
          actions={actions}
        />

        {/* ADR-055 R6: a deliberate property, said where the person acting on it is looking, and
            not in a parenthetical somewhere else. It stays on screen the whole time. */}
        <div className={`shared-dash-warning${preview ? ' is-preview' : ''}`} role="status">
          {preview
            ? t('public.previewBanner')
            : switchState == null
              ? t('public.banner')
              : switchState.enabled
                ? t('public.bannerLive', { count: switchState.routes })
                : t('public.bannerOff')}
        </div>

        {board}

        {catalogOpen && <CatalogModal onClose={() => setCatalogOpen(false)} publicOnly />}

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

/** What an anonymous visitor gets: the public board and nothing else — no sidebar, no other route.
 *
 *  🚨 This is the whole reason ADR-123 exists. Before it, a public deployment served every
 *  `RequireView` endpoint and the WebUI drew its full shell over them, so the node list, the event
 *  log and the internal shared board were all reachable. The visitor now has one page, and the API
 *  agrees: the routes open to them are derived from the widgets on this very board.
 */
export function PublicShell() {
  const { t } = useTranslation('dashboard');
  const [failed, setFailed] = useState<string | null>(null);
  const status = usePublicLayoutStore((s) => s.status);

  useEffect(() => {
    // Surface the one failure a visitor can act on: this deployment is public but nobody has
    // composed a board, so there is nothing to show. Anything else renders as an empty board.
    if (status === 'error') setFailed(t('public.viewerUnavailable'));
  }, [status, t]);

  return (
    <div className="public-shell">
      {/* 🚨 A way in, on the page. `appGate` makes `/login` always serve the form, but a URL nobody
          is told about is not an affordance (ADR-055 R6) — an operator arriving at a public
          deployment would have no reason to guess it. Deliberately small and out of the way: this
          page is for visitors, and the link is for the one person who needs to get past it. */}
      <div className="public-shell-bar">
        <a className="public-shell-signin" href="/login">
          {t('public.signIn')}
        </a>
      </div>
      {failed && (
        <p className="muted" role="status">
          {failed}
        </p>
      )}
      <PublicDashboardPage viewerOnly />
    </div>
  );
}
