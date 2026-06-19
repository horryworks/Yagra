// Board tabs for My Dashboard: switch between a user's boards, and in edit ("Customize") mode
// add / rename / remove them. Reads the active layout store from context so it targets the right
// board set. A user always keeps ≥1 board (the last one can't be removed).

import { useState } from 'react';
import { Button } from '../components/ui/Button';
import { TextInput } from '../components/ui/Field';
import { Modal } from '../components/ui/Modal';
import { useLayoutStoreContext } from './LayoutStoreContext';
import './BoardTabs.css';

export function BoardTabs({ editing }: { editing: boolean }) {
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
                aria-label="Board name"
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
                  aria-label={`Rename ${b.name}`}
                  title="Rename board"
                  onClick={() => startRename(b.id, b.name)}
                >
                  ✎
                </button>
                <button
                  type="button"
                  className="board-tab-btn"
                  aria-label={`Remove ${b.name}`}
                  title={boards.length <= 1 ? 'A dashboard needs at least one board' : 'Remove board'}
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
          title="Add a board"
        >
          + Add board
        </button>
      )}

      {confirmRemove && (
        <Modal
          title="Remove board?"
          onClose={() => setConfirmRemove(null)}
          footer={
            <>
              <Button onClick={() => setConfirmRemove(null)}>Cancel</Button>
              <Button
                variant="danger"
                onClick={() => {
                  removeBoard(confirmRemove.id);
                  setConfirmRemove(null);
                }}
              >
                Remove board
              </Button>
            </>
          }
        >
          <p>
            Remove “{confirmRemove.name}” and its widgets? This can’t be undone.
          </p>
        </Modal>
      )}
    </div>
  );
}
