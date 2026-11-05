// SPDX-License-Identifier: AGPL-3.0-only
import { describe, expect, it } from 'vitest';
import { NOTES_MAX, isValidNotes, noteToSend, noteView, showNoteCount } from './notesRules';

describe('inline notes (ADR-239)', () => {
  it('counts code points and ignores the surrounding blanks, as the server does', () => {
    expect(isValidNotes('a'.repeat(NOTES_MAX))).toBe(true);
    expect(isValidNotes('a'.repeat(NOTES_MAX + 1))).toBe(false);
    expect(isValidNotes('🙂'.repeat(NOTES_MAX))).toBe(true);
    expect(isValidNotes(`  ${'a'.repeat(NOTES_MAX)}  `)).toBe(true);
  });

  it('shows the count only near the cap', () => {
    expect(showNoteCount('a'.repeat(NOTES_MAX * 0.9))).toBe(false);
    expect(showNoteCount('a'.repeat(NOTES_MAX * 0.9 + 1))).toBe(true);
    expect(showNoteCount('a'.repeat(NOTES_MAX + 5))).toBe(true);
  });

  it('offers "Add note" only to a writer, and hides an empty note from everyone else', () => {
    expect(noteView('needs a ladder', false)).toBe('text');
    expect(noteView('needs a ladder', true)).toBe('text');
    expect(noteView(null, true)).toBe('add');
    expect(noteView('   ', true)).toBe('add');
    expect(noteView(undefined, false)).toBe('hidden');
    expect(noteView('', false)).toBe('hidden');
  });

  it('sends the trimmed text, and an empty string to clear', () => {
    expect(noteToSend('  two\nlines  ')).toBe('two\nlines');
    expect(noteToSend('   ')).toBe('');
  });
});
