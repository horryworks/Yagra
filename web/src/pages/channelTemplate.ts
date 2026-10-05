// SPDX-License-Identifier: AGPL-3.0-only
// Judgement behind the notification-template editor (ADR-039).
//
// It lives in a `.ts` and not in the modal because Vitest runs with `include: ['src/**/*.test.ts']`
// — a test written in a `.tsx` is a file nothing runs (testing.md). The modal keeps layout; the
// decisions that are easy to get quietly wrong (what "blank" means, whether the operator has
// unsaved work, what a stale preview is) live here where they can be tested.
//
// What is deliberately NOT here: whether a channel kind needs a JSON body, and whether a rendered
// body is valid JSON. The server answers both — `json_valid` on the preview — because writing the
// rule here would make it a mirror of the Rust `body_must_be_json` match with nothing to keep the
// two in step (extensibility.md §2).

import type { NotificationChannel, TemplatePreview } from '../types/api';

/** The editor's two fields, as typed. */
export interface TemplateDraft {
  subject: string;
  body: string;
  /** Line breaks and indentation around the tags are layout, not text (ADR-199). Absent is off. */
  freeLayout?: boolean;
}

/** The draft a channel opens with. */
export function draftFor(channel: NotificationChannel): TemplateDraft {
  return {
    subject: channel.subject_template ?? '',
    body: channel.body_template ?? '',
    ...(channel.template_free_layout ? { freeLayout: true } : {}),
  };
}

/**
 * The request body for a save.
 *
 * Blank collapses to `null`, which is how an override is cleared. An empty string would be a
 * template that renders to nothing — a subject line an operator could set by clearing the field
 * and then wonder where their notifications went. The server applies the same rule; this one
 * exists so the UI can tell "cleared" from "unchanged" before it asks.
 */
export function saveBody(draft: TemplateDraft): { subject: string | null; body: string | null; free_layout?: boolean } {
  const blank = (s: string) => (s.trim() === '' ? null : s);
  const subject = blank(draft.subject);
  const body = blank(draft.body);
  // Nothing to lay out is the built-in, which has one spelling; the server applies the same rule.
  const free = draft.freeLayout === true && (subject !== null || body !== null);
  return { subject, body, ...(free ? { free_layout: true } : {}) };
}

/** Whether the draft differs from what the channel currently has stored. */
export function isDirty(channel: NotificationChannel, draft: TemplateDraft): boolean {
  const saved = saveBody(draftFor(channel));
  const next = saveBody(draft);
  return saved.subject !== next.subject || saved.body !== next.body || saved.free_layout !== next.free_layout;
}

/** Whether the draft overrides nothing, i.e. saving it restores the built-in wording. */
export function isBuiltin(draft: TemplateDraft): boolean {
  const { subject, body } = saveBody(draft);
  return subject === null && body === null;
}

/** Whether a channel currently sends anything other than the built-in wording. */
export function hasTemplate(channel: NotificationChannel): boolean {
  return !isBuiltin(draftFor(channel));
}

/** How the preview panel should read. */
export type PreviewTone = 'ok' | 'problem';

/** A preview reduced to what the panel renders. */
export interface PreviewView {
  tone: PreviewTone;
  subject: string;
  body: string;
  /** One line per field that fell back, already prefixed with the field name. */
  problems: string[];
  /** Set only when the channel sends the body as JSON — `true`/`false` from the server, never
   *  decided here. */
  jsonValid: boolean | null;
}

/**
 * Turn a preview response into what the panel shows.
 *
 * A preview with problems is still a *successful* render in the sense that matters: it shows the
 * text that would actually be sent, because delivery falls back the same way. So the panel shows
 * the output either way and marks it, rather than replacing it with an error.
 */
export function previewView(preview: TemplatePreview): PreviewView {
  const problems = (preview.problems ?? []).map((p) => `${p.field}: ${p.message}`);
  return {
    tone: problems.length > 0 ? 'problem' : 'ok',
    subject: preview.subject,
    body: preview.body,
    problems,
    jsonValid: preview.json_valid ?? null,
  };
}

/**
 * Whether a preview still describes the draft on screen.
 *
 * Editing invalidates the preview: a stale one showing output for text the operator has since
 * changed is worse than no preview, because it reads as confirmation.
 */
export function previewMatches(shownFor: TemplateDraft | null, draft: TemplateDraft): boolean {
  return (
    shownFor !== null && shownFor.subject === draft.subject && shownFor.body === draft.body
  );
}

/** The snippet inserted when an operator picks a variable from the palette. */
export function variableSnippet(name: string, alwaysPresent: boolean): string {
  // An optional variable gets a `default` so a template written from the palette does not render a
  // blank where the operator expected something. The value is a placeholder they can edit.
  return alwaysPresent ? `{{ ${name} }}` : `{{ ${name} | default("—") }}`;
}

/**
 * Put `snippet` where the caret is in `text`, replacing a selection when there is one. Returns the
 * new text and where the caret goes: straight after what was put, so typing carries on from there.
 * Offsets are the input's own `selectionStart`/`selectionEnd` (UTF-16 units), clamped so a caret
 * remembered from an older value cannot land past the end.
 */
export function insertAtCaret(
  text: string,
  start: number,
  end: number,
  snippet: string,
): { text: string; caret: number } {
  const from = Math.max(0, Math.min(start, text.length));
  const to = Math.max(from, Math.min(end, text.length));
  return { text: text.slice(0, from) + snippet + text.slice(to), caret: from + snippet.length };
}

/**
 * Whether the subject is edited in a multi-line field. Laid out it spans lines (ADR-199); and a
 * subject that still holds a line break after free layout is turned off must show it, because a
 * one-line input hides line breaks on screen while the value it holds keeps them and sends them.
 */
export function subjectSpansLines(draft: TemplateDraft): boolean {
  return draft.freeLayout === true || /[\r\n]/.test(draft.subject);
}

/**
 * The draft with free layout turned on or off (ADR-199). The text is kept as typed, except that
 * the built-in copy is swapped for its other spelling — one line off, laid out on — because the
 * two send the same thing and the switch is what the operator reached for to read it.
 */
export function withFreeLayout(
  draft: TemplateDraft,
  on: boolean,
  copy: { subject: string; body: string } | null,
  laidCopy: { subject: string; body: string } | null,
): TemplateDraft {
  const same = (a: { subject: string; body: string } | null) =>
    a !== null && a.subject === draft.subject && a.body === draft.body;
  const text = on && same(copy) && laidCopy ? laidCopy : !on && same(laidCopy) && copy ? copy : draft;
  return { subject: text.subject, body: text.body, ...(on ? { freeLayout: true } : {}) };
}
