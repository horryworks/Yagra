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
}

/** The draft a channel opens with. */
export function draftFor(channel: NotificationChannel): TemplateDraft {
  return {
    subject: channel.subject_template ?? '',
    body: channel.body_template ?? '',
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
export function saveBody(draft: TemplateDraft): { subject: string | null; body: string | null } {
  const blank = (s: string) => (s.trim() === '' ? null : s);
  return { subject: blank(draft.subject), body: blank(draft.body) };
}

/** Whether the draft differs from what the channel currently has stored. */
export function isDirty(channel: NotificationChannel, draft: TemplateDraft): boolean {
  const saved = saveBody(draftFor(channel));
  const next = saveBody(draft);
  return saved.subject !== next.subject || saved.body !== next.body;
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
