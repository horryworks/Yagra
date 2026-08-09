// SPDX-License-Identifier: AGPL-3.0-only
// Board tabs for My Dashboard: switch between a user's boards, and in edit ("Customize") mode
// add / rename / remove them. Reads the active layout store from context so it targets the right
// board set. A user always keeps ≥1 board (the last one can't be removed).

import { useState } from 'react';
import { useTranslation } from 'react-i18next';
import { Button } from '../components/ui/Button';
import { TextInput } from '../components/ui/Field';
import { Modal } from '../components/ui/Modal';
import { useLayoutStoreContext } from './LayoutStoreContext';
import './BoardTabs.css';

export function BoardTabs({ editing }: { editing: boolean }) {
  const { t } = useTranslation('dashboard');
  const useStore = useLayoutStoreContext();
  const boards = useStore((s) => s.boards);
  const activeBoardId = useStore((s) => s.activeBoardId);
  const setActiveBoard = useStore((s) => s.setActiveBoard);
  const addBoard = useStore((s) => s.addBoard);
  const removeBoard = useStore((s) => s.removeBoard);
  const renameBoard = useStore((s) => s.renameBoard);

  const [renamingId, setRenamingId] = useState<string | null>(null);
  const [draft, setDraft] = useState('');
  const [confirmRemove, setConfirmRemove] = useState<{ id: string; name: string } | null>(null);

  const startRename = (id: string, name: string) => {
    setRenamingId(id);
    setDraft(name);
  };
  const commitRename = () => {
    if (renamingId) {
      const name = draft.trim();
      if (name) renameBoard(renamingId, name);
    }
    setRenamingId(null);
  };

  return (
    <div className="board-tabs" role="tablist">
      {boards.map((b) => {
        const active = b.id === activeBoardId;
        if (editing && renamingId === b.id) {
          return (
            <span className="board-tab is-active board-tab-renaming" key={b.id}>
              <TextInput
                value={draft}
                autoFocus
                onChange={(e) => setDraft(e.target.value)}
                onBlur={commitRename}
                onKeyDown={(e) => {
                  if (e.key === 'Enter') commitRename();
                  if (e.key === 'Escape') setRenamingId(null);
                }}
                aria-label={t('boardTabs.nameAria')}
              />
            </span>
          );
        }
        return (
          <span
            className={['board-tab', active ? 'is-active' : ''].filter(Boolean).join(' ')}
            key={b.id}
          >
            <button
              type="button"
              role="tab"
              aria-selected={active}
              className="board-tab-label"
              onClick={() => setActiveBoard(b.id)}
              onDoubleClick={() => editing && startRename(b.id, b.name)}
            >
              {b.name}
            </button>
            {editing && (
              <>
                <button
                  type="button"
                  className="board-tab-btn"
                  aria-label={t('boardTabs.rename', { name: b.name })}
                  title={t('boardTabs.renameTitle')}
                  onClick={() => startRename(b.id, b.name)}
                >
                  ✎
                </button>
                <button
                  type="button"
                  className="board-tab-btn"
                  aria-label={t('boardTabs.remove', { name: b.name })}
                  title={boards.length <= 1 ? t('boardTabs.removeLastTitle') : t('boardTabs.removeTitle')}
                  disabled={boards.length <= 1}
                  onClick={() =>
                    b.widgets.length === 0
                      ? removeBoard(b.id)
                      : setConfirmRemove({ id: b.id, name: b.name })
                  }
                >
                  ×
                </button>
              </>
            )}
          </span>
        );
      })}

      {editing && (
        <button
          type="button"
          className="board-tab-add"
          onClick={() => addBoard()}
          title={t('boardTabs.addBoardTitle')}
        >
          + {t('boardTabs.addBoard')}
        </button>
      )}

      {confirmRemove && (
        <Modal
          title={t('boardTabs.removeConfirmTitle')}
          onClose={() => setConfirmRemove(null)}
          footer={
            <>
              <Button onClick={() => setConfirmRemove(null)}>{t('common:actions.cancel')}</Button>
              <Button
                variant="danger"
                onClick={() => {
                  removeBoard(confirmRemove.id);
                  setConfirmRemove(null);
                }}
              >
                {t('boardTabs.removeConfirm')}
              </Button>
            </>
          }
        >
          <p className="modal-confirm-text">
            {t('boardTabs.removeBody', { name: confirmRemove.name })}
          </p>
        </Modal>
      )}
    </div>
  );
}
