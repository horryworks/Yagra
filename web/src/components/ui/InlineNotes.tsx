// SPDX-License-Identifier: AGPL-3.0-only
// A free-text note written where it is shown (ADR-239): a node's Overview tab and a folder's pane.
// Pressing the text or the pencil turns the section into a text box with Save / Cancel; there is
// no dialog to open. The rules (cap, when to show the count, what to draw) are in `notesRules.ts`.

import { useEffect, useRef, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { isImeComposing } from '../../lib/ime';
import { done } from '../../lib/submitState';
import { useSubmit } from '../../lib/useSubmit';
import { IconButton } from './IconButton';
import { FieldError, TextArea } from './Field';
import { FormError, FormFooter } from './FormFooter';
import { EditIcon } from './icons';
import {
  NOTES_MAX,
  isValidNotes,
  noteLength,
  noteToSend,
  noteView,
  showNoteCount,
} from './notesRules';
import './InlineNotes.css';

interface Props {
  /** The stored note, or null for none. */
  value: string | null | undefined;
  /** Whether the viewer may write it — `useCan('manage_config')`, never a role (ADR-056). */
  canEdit: boolean;
  /** Placeholder for the text box: an example of what this kind of note holds. */
  placeholder: string;
  /** Write the whole note; `''` clears it. */
  onSave: (notes: string) => Promise<void>;
  /** After a save landed: re-read whatever shows the note elsewhere. */
  onSaved?: () => void;
  /** What a failed save says when the server gave no message of its own. */
  errorFallback: string;
}

export function InlineNotes({
  value,
  canEdit,
  placeholder,
  onSave,
  onSaved,
  errorFallback,
}: Props) {
  const { t } = useTranslation('nodes');
  // What this section shows until the parent's next read arrives, so a save does not flash back
  // to the old text in between.
  const [shown, setShown] = useState(value ?? '');
  const [editing, setEditing] = useState(false);

  // A newer read from the parent wins, except while the person is typing. Keyed on the value
  // actually changing: re-running because `editing` went false after a save would copy the parent's
  // still-stale value over the text just saved, and the old note would flash back until the re-read.
  const seenValue = useRef(value);
  useEffect(() => {
    if (editing || value === seenValue.current) return;
    seenValue.current = value;
    setShown(value ?? '');
  }, [value, editing]);

  const view = noteView(shown, canEdit);
  if (view === 'hidden') return null;

  const start = () => {
    if (canEdit) setEditing(true);
  };

  if (editing) {
    return (
      <NoteEditor
        initial={shown}
        placeholder={placeholder}
        errorFallback={errorFallback}
        onSave={onSave}
        onCancel={() => setEditing(false)}
        onDone={(next) => {
          setShown(next);
          setEditing(false);
          onSaved?.();
        }}
      />
    );
  }

  if (view === 'add') {
    return (
      <section className="inline-notes">
        <button type="button" className="inline-notes-add" onClick={start}>
          + {t('notesAdd')}
        </button>
      </section>
    );
  }

  return (
    <section className="inline-notes">
      <div className="nd-section-t inline-notes-head">
        {t('field.notes')}
        {canEdit && (
          <IconButton title={t('notesEdit')} onClick={start} className="inline-notes-pen">
            <EditIcon />
          </IconButton>
        )}
      </div>
      {canEdit ? (
        <button type="button" className="nd-notes inline-notes-text" onClick={start}>
          {shown}
        </button>
      ) : (
        // Plain text. Operator-supplied and rendered as written — never as markup.
        <p className="nd-notes">{shown}</p>
      )}
    </section>
  );
}

/** The text box. Its own component so it unmounts when the edit ends — `useSubmit` stays busy
 *  after a success until its owner goes away, which is what a closing dialog does. */
function NoteEditor({
  initial,
  placeholder,
  errorFallback,
  onSave,
  onCancel,
  onDone,
}: {
  initial: string;
  placeholder: string;
  errorFallback: string;
  onSave: (notes: string) => Promise<void>;
  onCancel: () => void;
  onDone: (next: string) => void;
}) {
  const { t } = useTranslation(['nodes', 'common']);
  const [draft, setDraft] = useState(initial);
  const boxRef = useRef<HTMLTextAreaElement>(null);
  const form = useSubmit<string>({ errorFallback, onDone });
  const tooLong = !isValidNotes(draft);

  useEffect(() => {
    const box = boxRef.current;
    if (!box) return;
    box.focus();
    box.setSelectionRange(box.value.length, box.value.length);
  }, []);

  const save = () => {
    if (tooLong) return;
    const next = noteToSend(draft);
    form.submit(async () => {
      await onSave(next);
      return done(next);
    });
  };
  const cancel = () => {
    if (!form.busy) onCancel();
  };

  return (
    <section className="inline-notes">
      <div className="nd-section-t">{t('field.notes')}</div>
      <TextArea
        inputRef={boxRef}
        rows={4}
        value={draft}
        placeholder={placeholder}
        aria-label={t('field.notes')}
        onChange={(e) => setDraft(e.target.value)}
        onKeyDown={(e) => {
          if (isImeComposing(e)) return;
          if (e.key === 'Escape') {
            e.preventDefault();
            e.stopPropagation();
            cancel();
          } else if (e.key === 'Enter' && (e.ctrlKey || e.metaKey)) {
            e.preventDefault();
            save();
          }
        }}
      />
      {tooLong && <FieldError>{t('field.notesTooLong', { max: NOTES_MAX })}</FieldError>}
      <FormError form={form} />
      <div className="inline-notes-actions">
        <FormFooter
          form={form}
          onClose={cancel}
          onSubmit={save}
          submitLabel={t('common:actions.save')}
          canSubmit={!tooLong}
          extra={
            showNoteCount(draft) ? (
              <span className={`inline-notes-count${tooLong ? ' over' : ''}`}>
                {noteLength(draft)} / {NOTES_MAX}
              </span>
            ) : undefined
          }
        />
      </div>
    </section>
  );
}
