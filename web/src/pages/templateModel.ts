// SPDX-License-Identifier: AGPL-3.0-only
// The visual notification-template editor's model (ADR-039 Inc.2): a template as rows of text and
// variable tags, one set per point in an alert's life, and the two-way conversion between that and
// the template text the server stores.
//
// The stored format does not change. A template saved here is the same minijinja text an operator
// could have typed into the code editor, so the server, the database and the delivery path are not
// involved. That makes this file the whole risk of the feature, and why it is a `.ts` with tests:
//
// - `serializeField` writes only a small set of shapes, and `parseField` reads exactly that set
//   back. Anything else in a stored template (a loop, a nested `if`, another filter) is refused
//   with a reason, and the editor opens the template as code instead of guessing.
// - A draft that is still Yagra's built-in subject saves as `null`, not as text, so the built-in
//   wording keeps applying - including to the pool and Meraki organization alerts, whose built-in
//   sentences are not the node one the draft shows.

import type { BuiltinSubjectTemplate, NotifyEvent } from '../types/api';
import { isTemplateVariable } from './templateVariables';

/** One piece of a template row: literal text, or a variable the server fills in. */
export type Segment =
  | { kind: 'text'; text: string }
  | {
      kind: 'var';
      name: string;
      /** Shown instead when the alert lacks the value (`| default("...")`). Ignored when
       *  `hideLine` is set. */
      fallback: string;
      /** Leave this whole line out when the alert lacks the value. */
      hideLine: boolean;
    };

export type TemplateField = 'subject' | 'body';

/** What fire sends. */
export interface Variant {
  subject: Segment[];
  body: Segment[];
}

/** What resolve or suppress sends, field by field. `null` is "the same as fire" for that field:
 *  the built-in subject differs at every point in the alert's life while the body does not, so
 *  the two fields have to be able to follow fire separately. */
export interface BranchVariant {
  subject: Segment[] | null;
  body: Segment[] | null;
}

/** A whole template. */
export interface VisualTemplate {
  fire: Variant;
  resolve: BranchVariant;
  suppress: BranchVariant;
}

/** The three points in an alert's life, in the order the editor's tabs show them. */
export const TEMPLATE_EVENTS = ['fire', 'resolve', 'suppress'] as const satisfies readonly NotifyEvent[];
export type BranchEvent = Exclude<NotifyEvent, 'fire'>;
const BRANCH_EVENTS: readonly BranchEvent[] = ['resolve', 'suppress'];

/** JSM cuts its alert title here. The Rust side truncates at the same number
 *  (`alerts/notify.rs::JSM_MESSAGE_MAX_CHARS`), and a test there reads this line. */
export const JSM_MESSAGE_MAX_CHARS = 130;

/** Why a stored template cannot be shown as tags. Each has an EN/JA sentence under
 *  `routing.template.unsupported.*`. */
export const UNSUPPORTED_REASONS = [
  'statement',
  'expression',
  'unknownVariable',
  'lineCondition',
  'eventBranch',
  'comment',
  'whitespaceControl',
  'unclosed',
] as const;
export type UnsupportedReason = (typeof UNSUPPORTED_REASONS)[number];

export interface Unsupported {
  reason: UnsupportedReason;
  /** The piece of the template that could not be read, for the operator to find. */
  snippet: string;
}

/** A field, read back: fire always, the other two only when the template branches on them. */
export type FieldBranches = { fire: Segment[] } & Partial<Record<BranchEvent, Segment[]>>;
export type ParseResult = { ok: true; branches: FieldBranches } | { ok: false; error: Unsupported };

// ── Segments ──────────────────────────────────────────────────────────────────────────────

/** Join neighbouring text pieces and drop empty ones, so two equal templates compare equal. */
export function normalize(segments: readonly Segment[]): Segment[] {
  const out: Segment[] = [];
  for (const s of segments) {
    if (s.kind === 'text') {
      if (s.text === '') continue;
      const last = out[out.length - 1];
      if (last && last.kind === 'text') {
        out[out.length - 1] = { kind: 'text', text: last.text + s.text };
        continue;
      }
      out.push({ kind: 'text', text: s.text });
    } else {
      out.push({ ...s, fallback: s.hideLine ? '' : s.fallback });
    }
  }
  return out;
}

/** Whether two rows send the same thing. A hidden line's fallback is never sent, so it is ignored. */
export function sameSegments(a: readonly Segment[], b: readonly Segment[]): boolean {
  return JSON.stringify(normalize(a)) === JSON.stringify(normalize(b));
}

/** Whether a row sends nothing at all. */
export function isBlank(segments: readonly Segment[]): boolean {
  return !segments.some((s) => s.kind === 'var' || s.text.trim() !== '');
}

function splitLines(segments: readonly Segment[]): Segment[][] {
  const lines: Segment[][] = [[]];
  for (const s of normalize(segments)) {
    if (s.kind === 'var') {
      lines[lines.length - 1].push(s);
      continue;
    }
    s.text.split('\n').forEach((part, i) => {
      if (i > 0) lines.push([]);
      if (part !== '') lines[lines.length - 1].push({ kind: 'text', text: part });
    });
  }
  return lines;
}

// ── Writing ───────────────────────────────────────────────────────────────────────────────

/** Literal text, with any `{{`, `{%` or `{#` made inert so the operator's braces stay text. */
function escapeText(text: string): string {
  // A lookahead, not a capture: `{{{` has two openers, and consuming the second brace as part of
  // the first match would leave it unescaped.
  return text.replace(/\{(?=[{%#])/g, "{{ '{' }}");
}

function variableExpr(s: Extract<Segment, { kind: 'var' }>): string {
  // `tags` is a list; printed bare it would read like `["JAPAN", "core"]`.
  if (s.name === 'tags') return '{{ tags | join(", ") }}';
  if (!s.hideLine && s.fallback !== '') return `{{ ${s.name} | default(${JSON.stringify(s.fallback)}) }}`;
  return `{{ ${s.name} }}`;
}

/** One row as template text. A line holding a variable marked `hideLine` is wrapped, newline
 *  included, so leaving it out leaves no blank line behind. */
export function serializeSegments(segments: readonly Segment[]): string {
  const lines = splitLines(segments);
  return lines
    .map((line, i) => {
      const text =
        line.map((s) => (s.kind === 'text' ? escapeText(s.text) : variableExpr(s))).join('') +
        (i < lines.length - 1 ? '\n' : '');
      const conds = [...new Set(line.flatMap((s) => (s.kind === 'var' && s.hideLine ? [s.name] : [])))];
      return conds.length > 0
        ? `{% if ${conds.map((c) => `${c} is defined`).join(' and ')} %}${text}{% endif %}`
        : text;
    })
    .join('');
}

/** The row a point in the alert's life actually sends for a field. */
export function effective(model: VisualTemplate, event: NotifyEvent, field: TemplateField): Segment[] {
  return event === 'fire' ? model.fire[field] : (model[event][field] ?? model.fire[field]);
}

/** Whether resolve or suppress sends exactly fire's text, in both fields. */
export function followsFire(model: VisualTemplate, event: NotifyEvent): boolean {
  return event !== 'fire' && model[event].subject === null && model[event].body === null;
}

/** Whether one field at resolve or suppress follows fire's. */
export function fieldFollowsFire(model: VisualTemplate, event: NotifyEvent, field: TemplateField): boolean {
  return event !== 'fire' && model[event][field] === null;
}

/**
 * One field as the template text to store, or `null` when it sends nothing (= the built-in text).
 *
 * A resolve / suppress row that is the same as fire's gets no branch, so a template that never
 * differed by event reads back with its tabs set to "the same".
 */
export function serializeField(model: VisualTemplate, field: TemplateField): string | null {
  const fire = model.fire[field];
  const branches = BRANCH_EVENTS.flatMap((e) => {
    const own = model[e][field];
    return own && !sameSegments(own, fire) ? [{ event: e, segments: own }] : [];
  });
  if (isBlank(fire) && branches.every((b) => isBlank(b.segments))) return null;
  if (branches.length === 0) return serializeSegments(fire);
  const arms = branches
    .map((b, i) => `{% ${i === 0 ? 'if' : 'elif'} event == "${b.event}" %}${serializeSegments(b.segments)}`)
    .join('');
  return `${arms}{% else %}${serializeSegments(fire)}{% endif %}`;
}

// ── Reading ───────────────────────────────────────────────────────────────────────────────

type Token =
  | { t: 'text'; text: string }
  | { t: 'expr'; inner: string; raw: string }
  | { t: 'stmt'; inner: string; raw: string };

function fail(reason: UnsupportedReason, snippet: string): { ok: false; error: Unsupported } {
  return { ok: false, error: { reason, snippet } };
}

function tokenize(src: string): Token[] | { ok: false; error: Unsupported } {
  const out: Token[] = [];
  let i = 0;
  while (i < src.length) {
    const open = src.slice(i).search(/\{[{%#]/);
    if (open < 0) {
      out.push({ t: 'text', text: src.slice(i) });
      break;
    }
    const start = i + open;
    if (start > i) out.push({ t: 'text', text: src.slice(i, start) });
    const kind = src[start + 1];
    if (kind === '#') return fail('comment', src.slice(start, start + 40));
    const close = src.indexOf(kind === '{' ? '}}' : '%}', start + 2);
    if (close < 0) return fail('unclosed', src.slice(start, start + 40));
    const raw = src.slice(start, close + 2);
    const inner = src.slice(start + 2, close);
    if (inner.startsWith('-') || inner.endsWith('-') || inner.startsWith('+') || inner.endsWith('+')) {
      return fail('whitespaceControl', raw);
    }
    out.push(kind === '{' ? { t: 'expr', inner: inner.trim(), raw } : { t: 'stmt', inner: inner.trim(), raw });
    i = close + 2;
  }
  return out;
}

function unquote(literal: string): string {
  if (literal.startsWith('"')) return JSON.parse(literal) as string;
  // A single-quoted literal: swap the quoting and let JSON do the unescaping.
  const body = literal.slice(1, -1).replace(/\\'/g, "'").replace(/"/g, '\\"');
  return JSON.parse(`"${body}"`) as string;
}

const STRING = String.raw`("(?:[^"\\]|\\.)*"|'(?:[^'\\]|\\.)*')`;
const RE_BRACE = /^(['"])\{\1$/;
const RE_NAME = /^([a-z_]+)$/;
const RE_DEFAULT = new RegExp(String.raw`^([a-z_]+)\s*\|\s*default\(\s*${STRING}\s*\)$`);
const RE_JOIN = /^tags\s*\|\s*join\(\s*(", "|', ')\s*\)$/;
const RE_IF_EVENT = /^(if|elif)\s+event\s*==\s*(["'])(resolve|suppress)\2$/;
const RE_IF_DEFINED = /^if\s+([a-z_]+\s+is\s+defined(?:\s+and\s+[a-z_]+\s+is\s+defined)*)$/;

function readExpr(tok: Extract<Token, { t: 'expr' }>): Segment | { ok: false; error: Unsupported } {
  const e = tok.inner;
  if (RE_BRACE.test(e)) return { kind: 'text', text: '{' };
  if (RE_JOIN.test(e)) return { kind: 'var', name: 'tags', fallback: '', hideLine: false };
  const named = RE_NAME.exec(e);
  const dflt = RE_DEFAULT.exec(e);
  const name = named?.[1] ?? dflt?.[1];
  if (!name) return fail('expression', tok.raw);
  if (!isTemplateVariable(name)) return fail('unknownVariable', tok.raw);
  // A bare list would print its brackets; the editor only writes it joined.
  if (name === 'tags') return fail('expression', tok.raw);
  return { kind: 'var', name, fallback: dflt ? unquote(dflt[2]) : '', hideLine: false };
}

/** Text and variables, with line conditions. No event branches: those are the field's own level. */
function readBody(tokens: readonly Token[]): { ok: true; segments: Segment[] } | { ok: false; error: Unsupported } {
  const out: Segment[] = [];
  const atLineStart = () => {
    const norm = normalize(out);
    const last = norm[norm.length - 1];
    return !last || (last.kind === 'text' && last.text.endsWith('\n'));
  };
  for (let i = 0; i < tokens.length; i++) {
    const tok = tokens[i];
    if (tok.t === 'text') {
      out.push({ kind: 'text', text: tok.text });
      continue;
    }
    if (tok.t === 'expr') {
      const seg = readExpr(tok);
      if ('ok' in seg) return seg;
      out.push(seg);
      continue;
    }
    if (RE_IF_EVENT.test(tok.inner)) return fail('eventBranch', tok.raw);
    const cond = RE_IF_DEFINED.exec(tok.inner);
    if (!cond) return fail('statement', tok.raw);
    // A line condition: exactly one line, starting at a line start, ending at a newline or at the
    // end of the row, holding every variable it names.
    const end = tokens.findIndex((t, j) => j > i && t.t === 'stmt');
    if (end < 0 || tokens[end].t !== 'stmt' || (tokens[end] as { inner: string }).inner !== 'endif') {
      return fail('lineCondition', tok.raw);
    }
    if (!atLineStart()) return fail('lineCondition', tok.raw);
    const inner: Segment[] = [];
    for (const t of tokens.slice(i + 1, end)) {
      if (t.t === 'text') inner.push({ kind: 'text', text: t.text });
      else if (t.t === 'expr') {
        const seg = readExpr(t);
        if ('ok' in seg) return seg;
        inner.push(seg);
      }
    }
    const text = normalize(inner)
      .flatMap((s) => (s.kind === 'text' ? [s.text] : ['x']))
      .join('');
    const isLast = end === tokens.length - 1;
    const oneLine = text.endsWith('\n') ? !text.slice(0, -1).includes('\n') : isLast && !text.includes('\n');
    if (!oneLine) return fail('lineCondition', tok.raw);
    const names = cond[1].split(/\s+and\s+/).map((c) => c.replace(/\s+is\s+defined$/, ''));
    for (const n of names) {
      if (!inner.some((s) => s.kind === 'var' && s.name === n)) return fail('lineCondition', tok.raw);
    }
    for (const s of inner) {
      out.push(s.kind === 'var' && names.includes(s.name) ? { ...s, fallback: '', hideLine: true } : s);
    }
    i = end;
  }
  return { ok: true, segments: normalize(out) };
}

/** Read one stored field back into rows, or say why it cannot be. */
export function parseField(src: string): ParseResult {
  const tokens = tokenize(src);
  if (!Array.isArray(tokens)) return tokens;
  const first = tokens[0];
  const headTok = first && first.t === 'stmt' ? first : null;
  const head = headTok ? RE_IF_EVENT.exec(headTok.inner) : null;
  if (!headTok || !head || head[1] !== 'if') {
    const body = readBody(tokens);
    return body.ok ? { ok: true, branches: { fire: body.segments } } : body;
  }
  // `{% if event == "a" %}...{% elif event == "b" %}...{% else %}...{% endif %}`, spanning the
  // whole field. Line conditions may sit inside the arms, so arms split at depth 0 only.
  const arms: { event: NotifyEvent; tokens: Token[] }[] = [];
  let current: { event: NotifyEvent; tokens: Token[] } = { event: head[3] as BranchEvent, tokens: [] };
  let depth = 0;
  let closed = false;
  for (let i = 1; i < tokens.length; i++) {
    const tok = tokens[i];
    if (closed) return fail('eventBranch', tok.t === 'text' ? tok.text : tok.raw);
    if (tok.t === 'stmt') {
      const arm = RE_IF_EVENT.exec(tok.inner);
      if (depth === 0 && arm && arm[1] === 'elif') {
        arms.push(current);
        current = { event: arm[3] as BranchEvent, tokens: [] };
        continue;
      }
      if (depth === 0 && tok.inner === 'else') {
        arms.push(current);
        current = { event: 'fire', tokens: [] };
        continue;
      }
      if (depth === 0 && tok.inner === 'endif') {
        arms.push(current);
        closed = true;
        continue;
      }
      if (tok.inner.startsWith('if ')) depth++;
      if (tok.inner === 'endif') depth--;
    }
    current.tokens.push(tok);
  }
  const events = arms.map((a) => a.event);
  if (!closed || events[events.length - 1] !== 'fire' || new Set(events).size !== events.length) {
    return fail('eventBranch', headTok.raw);
  }
  const branches: Partial<Record<NotifyEvent, Segment[]>> = {};
  for (const arm of arms) {
    const body = readBody(arm.tokens);
    if (!body.ok) return body;
    branches[arm.event] = body.segments;
  }
  return { ok: true, branches: branches as FieldBranches };
}

// ── Opening and saving ────────────────────────────────────────────────────────────────────

/** Yagra's built-in node subject as rows, per point in the alert's life; `null` when the server
 *  did not provide it (an older core) or it could not be read. */
export type BuiltinDraft = Record<NotifyEvent, Segment[]> | null;

export function builtinDraft(templates: readonly BuiltinSubjectTemplate[] | null): BuiltinDraft {
  if (!templates) return null;
  const out: Partial<Record<NotifyEvent, Segment[]>> = {};
  for (const t of templates) {
    const parsed = parseField(t.subject);
    if (!parsed.ok) return null;
    out[t.event] = parsed.branches.fire;
  }
  return TEMPLATE_EVENTS.every((e) => out[e]) ? (out as Record<NotifyEvent, Segment[]>) : null;
}

function clone(segments: readonly Segment[]): Segment[] {
  return segments.map((s) => ({ ...s }));
}

/** A template from its two fields, read back separately. A point in the alert's life that either
 *  field branches on gets a row of its own in both; the other field copies its fire row. */
export function templateFrom(subject: FieldBranches, body: FieldBranches): VisualTemplate {
  const variant = (e: BranchEvent): BranchVariant => ({
    subject: subject[e] ? clone(subject[e]) : null,
    body: body[e] ? clone(body[e]) : null,
  });
  return {
    fire: { subject: clone(subject.fire), body: clone(body.fire) },
    resolve: variant('resolve'),
    suppress: variant('suppress'),
  };
}

function draftBranches(draft: BuiltinDraft): FieldBranches {
  if (!draft) return { fire: [] };
  return { fire: draft.fire, resolve: draft.resolve, suppress: draft.suppress };
}

/** The template a channel opens on, or why it has to open as code. A field with no stored template
 *  starts from the built-in: the subject as the draft, the body empty (the built-in body is the
 *  alert as JSON, which has no rows to show). */
export function openTemplate(
  stored: { subject: string | null; body: string | null },
  draft: BuiltinDraft,
): { ok: true; model: VisualTemplate } | { ok: false; error: Unsupported } {
  const read = (src: string | null): ParseResult | null => (src && src.trim() !== '' ? parseField(src) : null);
  const subject = read(stored.subject);
  const body = read(stored.body);
  if (subject && !subject.ok) return subject;
  if (body && !body.ok) return body;
  return {
    ok: true,
    model: templateFrom(subject ? subject.branches : draftBranches(draft), body ? body.branches : { fire: [] }),
  };
}

/** The draft the "restore the built-in text" button puts back. */
export function builtinTemplate(draft: BuiltinDraft): VisualTemplate {
  return templateFrom(draftBranches(draft), { fire: [] });
}

/** Whether the subject still sends exactly the built-in subject at every point in the alert's life. */
export function subjectIsBuiltin(model: VisualTemplate, draft: BuiltinDraft): boolean {
  return draft !== null && TEMPLATE_EVENTS.every((e) => sameSegments(effective(model, e, 'subject'), draft[e]));
}

/** What saving sends. A subject left as the built-in draft is `null`, never its text. */
export function saveRequest(
  model: VisualTemplate,
  draft: BuiltinDraft,
): { subject: string | null; body: string | null } {
  return {
    subject: subjectIsBuiltin(model, draft) ? null : serializeField(model, 'subject'),
    body: serializeField(model, 'body'),
  };
}

/** Replace one field's row at one point in the alert's life. At resolve or suppress this gives
 *  the field text of its own, even if it followed fire until now. */
export function withField(
  model: VisualTemplate,
  event: NotifyEvent,
  field: TemplateField,
  segments: Segment[],
): VisualTemplate {
  if (event === 'fire') return { ...model, fire: { ...model.fire, [field]: segments } };
  return { ...model, [event]: { ...model[event], [field]: segments } };
}

/** Give resolve or suppress text of its own, starting from fire's. */
export function writeOwn(model: VisualTemplate, event: BranchEvent): VisualTemplate {
  return { ...model, [event]: { subject: clone(model.fire.subject), body: clone(model.fire.body) } };
}

/** Make resolve or suppress send fire's text again: one field, or (no field) both. */
export function backToFire(model: VisualTemplate, event: BranchEvent, field?: TemplateField): VisualTemplate {
  if (field) return { ...model, [event]: { ...model[event], [field]: null } };
  return { ...model, [event]: { subject: null, body: null } };
}

// ── Preview samples ───────────────────────────────────────────────────────────────────────

/** The sample alerts the preview offers. Each pairs a point in the alert's life with one of the
 *  server's two sample alerts (`PreviewSample`). */
export const PREVIEW_SAMPLES = [
  { id: 'nodeDown', event: 'fire', sample: 'liveness' },
  { id: 'threshold', event: 'fire', sample: 'threshold' },
  { id: 'recovered', event: 'resolve', sample: 'liveness' },
  { id: 'rolledUp', event: 'suppress', sample: 'liveness' },
] as const;
export type PreviewSampleId = (typeof PREVIEW_SAMPLES)[number]['id'];

export function previewSample(id: PreviewSampleId): (typeof PREVIEW_SAMPLES)[number] {
  return PREVIEW_SAMPLES.find((s) => s.id === id) ?? PREVIEW_SAMPLES[0];
}

/** The sample to show when the operator switches to a tab: the current one if it is about that
 *  point in the alert's life, otherwise the first that is. */
export function sampleForEvent(event: NotifyEvent, current: PreviewSampleId): PreviewSampleId {
  if (previewSample(current).event === event) return current;
  return (PREVIEW_SAMPLES.find((s) => s.event === event) ?? PREVIEW_SAMPLES[0]).id;
}

/** A JSM title split at the point JSM cuts it. Counted in characters, as the server counts. */
export function jsmTitle(title: string): { kept: string; cut: string; length: number } {
  const chars = Array.from(title);
  return {
    kept: chars.slice(0, JSM_MESSAGE_MAX_CHARS).join(''),
    cut: chars.slice(JSM_MESSAGE_MAX_CHARS).join(''),
    length: chars.length,
  };
}
