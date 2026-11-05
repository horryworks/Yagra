// The rules behind a free-text note written in place (ADR-239): on a node's Overview tab and on a
// folder's pane. Kept apart from the component so Vitest (node environment) can hold them.

/** The longest note the API accepts. Mirrors `api/nodes.rs::NOTES_MAX`, which both the node and
 *  the folder endpoint use. */
export const NOTES_MAX = 2000;

/** Counted in code points, as the backend counts it — `.length` counts UTF-16 units, so a note of
 *  emoji or CJK would be refused by the server after passing here, the worst place to disagree. */
export function noteLength(notes: string): number {
  return [...notes.trim()].length;
}

/** Whether the note is short enough to send. */
export function isValidNotes(notes: string): boolean {
  return noteLength(notes) <= NOTES_MAX;
}

/** Whether to show the "n / 2000" count. Only near the cap: below it the number says nothing a
 *  person needs (ADR-200 — the count is the screen showing a result, not a sentence). */
export function showNoteCount(notes: string): boolean {
  return noteLength(notes) > NOTES_MAX * 0.9;
}

/** What the section shows when it is not being edited.
 *
 *  - `text`: the note, with the pencil when the viewer may write.
 *  - `add`: no note yet, and the viewer may write one — only the "Add note" button.
 *  - `hidden`: no note and no way to write one, so nothing at all (an empty heading would say
 *    "there is something here" when there is not). */
export type NoteView = 'text' | 'add' | 'hidden';

export function noteView(notes: string | null | undefined, canEdit: boolean): NoteView {
  if (notes && notes.trim() !== '') return 'text';
  return canEdit ? 'add' : 'hidden';
}

/** What a save sends: trimmed, and `''` to clear. The endpoint takes the whole value and reads
 *  `''` as "no note", so there is no separate delete. */
export function noteToSend(draft: string): string {
  return draft.trim();
}
