// SPDX-License-Identifier: AGPL-3.0-only
// Yagra's built-in notification text, read for SHOWING (ADR-197 decision 3).
//
// A channel with no template of its own sends the built-in text, and the editor shows it: variables
// as tags, and each part that is only sent when a condition holds inside a dashed box saying when.
// The built-in uses shapes the visual editor's reader refuses on purpose (`node_address !=
// node_name`, an `if` inside an `if`, `| number`), so this is a second reader, and a deliberately
// lenient one: it only ever draws, it never saves, and a piece it cannot name is shown as the raw
// text rather than refused. `templateModel.ts::parseField` stays the one reader whose answer is
// written back to the server.
//
// Also here: the copy "Edit a copy of this text" puts in the code editor (decision 4), and whether
// a channel has a template of its own at all (decision 5).

import type { BuiltinSubjectTemplate, NotifyEvent } from '../types/api';
import { TEMPLATE_EVENTS } from './templateModel';
import { isTemplateVariable } from './templateVariables';

/** One clause of a condition. */
export type Term =
  | { kind: 'present'; name: string }
  | { kind: 'differs'; name: string; other: string };

/** What a branch is taken on: clauses that all hold, or text this reader could not name. */
export type Condition = { terms: Term[] } | { raw: string };

export interface Branch {
  /** `null` for an `{% else %}` arm. */
  when: Condition | null;
  /** Whether an earlier arm of the same `if` comes first (`elif` / `else`). */
  otherwise: boolean;
  pieces: DisplayPiece[];
}

export type DisplayPiece =
  | { kind: 'text'; text: string }
  | { kind: 'var'; name: string }
  /** An expression or statement this reader does not draw as a tag, shown as written. */
  | { kind: 'raw'; text: string }
  | { kind: 'cond'; branches: Branch[] };

type Token =
  | { t: 'text'; text: string }
  | { t: 'expr'; inner: string; raw: string }
  | { t: 'stmt'; inner: string; raw: string };

function tokenize(src: string): Token[] {
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
    const closer = kind === '{' ? '}}' : kind === '%' ? '%}' : '#}';
    const close = src.indexOf(closer, start + 2);
    if (close < 0) {
      out.push({ t: 'text', text: src.slice(start) });
      break;
    }
    const raw = src.slice(start, close + 2);
    const inner = src
      .slice(start + 2, close)
      .replace(/^[-+]|[-+]$/g, '')
      .trim();
    if (kind === '{') out.push({ t: 'expr', inner, raw });
    else if (kind === '%') out.push({ t: 'stmt', inner, raw });
    i = close + 2;
  }
  return out;
}

const RE_PRESENT = /^([a-z_]+)(?:\s+is\s+defined)?$/;
const RE_DIFFERS = /^([a-z_]+)\s*!=\s*([a-z_]+)$/;

/** A condition as clauses. A name that a `!=` clause already compares is not said twice:
 *  "address differs from name" covers "address is known". */
export function readCondition(src: string): Condition {
  const terms: Term[] = [];
  for (const part of src.trim().split(/\s+and\s+/)) {
    const present = RE_PRESENT.exec(part);
    const differs = RE_DIFFERS.exec(part);
    if (present && isTemplateVariable(present[1])) terms.push({ kind: 'present', name: present[1] });
    else if (differs && isTemplateVariable(differs[1]) && isTemplateVariable(differs[2])) {
      terms.push({ kind: 'differs', name: differs[1], other: differs[2] });
    } else return { raw: src.trim() };
  }
  const compared = new Set(terms.flatMap((t) => (t.kind === 'differs' ? [t.name] : [])));
  return { terms: terms.filter((t) => t.kind !== 'present' || !compared.has(t.name)) };
}

function readExpr(tok: Extract<Token, { t: 'expr' }>): DisplayPiece {
  if (/^(['"])\{\1$/.test(tok.inner)) return { kind: 'text', text: '{' };
  const m = /^([a-z_]+)(?:\s*\|.*)?$/s.exec(tok.inner);
  return m && isTemplateVariable(m[1]) ? { kind: 'var', name: m[1] } : { kind: 'raw', text: tok.raw };
}

/** Join neighbouring text, so the same text always draws the same way. */
function tidy(pieces: DisplayPiece[]): DisplayPiece[] {
  const out: DisplayPiece[] = [];
  for (const p of pieces) {
    const last = out[out.length - 1];
    if (p.kind === 'text' && p.text === '') continue;
    if (p.kind === 'text' && last?.kind === 'text') out[out.length - 1] = { kind: 'text', text: last.text + p.text };
    else out.push(p);
  }
  return out;
}

/** A template as pieces to draw. Never fails: what it cannot read is drawn as written. */
export function readForDisplay(src: string): DisplayPiece[] {
  const root: DisplayPiece[] = [];
  // The open `if`s, innermost last; each knows where its pieces go back to.
  const stack: { cond: Extract<DisplayPiece, { kind: 'cond' }>; parent: DisplayPiece[] }[] = [];
  let into = root;
  for (const tok of tokenize(src)) {
    if (tok.t === 'text') {
      into.push({ kind: 'text', text: tok.text });
      continue;
    }
    if (tok.t === 'expr') {
      into.push(readExpr(tok));
      continue;
    }
    const s = tok.inner;
    if (s.startsWith('if ')) {
      const branch: Branch = { when: readCondition(s.slice(3)), otherwise: false, pieces: [] };
      const cond: Extract<DisplayPiece, { kind: 'cond' }> = { kind: 'cond', branches: [branch] };
      into.push(cond);
      stack.push({ cond, parent: into });
      into = branch.pieces;
    } else if ((s.startsWith('elif ') || s === 'else') && stack.length > 0) {
      const top = stack[stack.length - 1];
      const branch: Branch = {
        when: s === 'else' ? null : readCondition(s.slice(5)),
        otherwise: true,
        pieces: [],
      };
      top.cond.branches.push(branch);
      into = branch.pieces;
    } else if (s === 'endif' && stack.length > 0) {
      into = stack.pop()!.parent;
    } else {
      into.push({ kind: 'raw', text: tok.raw });
    }
  }
  const settle = (pieces: DisplayPiece[]): DisplayPiece[] =>
    tidy(pieces).map((p) =>
      p.kind === 'cond' ? { ...p, branches: p.branches.map((b) => ({ ...b, pieces: settle(b.pieces) })) } : p,
    );
  return settle(root);
}

/**
 * Pieces laid out for reading (Inc.2 decision 10). The sentence stays one line: a part sent only
 * under a condition is underlined and numbered, and its condition is said once, below, in the
 * legend. A condition that keeps or drops a whole line is said at the end of that line instead,
 * and takes no number.
 */
export type Shown =
  | { kind: 'text'; text: string }
  | { kind: 'var'; name: string }
  | { kind: 'raw'; text: string }
  | { kind: 'part'; n: number; pieces: Shown[] }
  | { kind: 'line'; note: Note; pieces: Shown[] };

/** When one numbered part, or one line, is sent. */
export interface Note {
  branch: Branch;
  /** The number of the branch before this one in the same `if`, for "only when ③ is not sent". */
  prev: number | null;
  /** The numbered part this one sits inside, if any. */
  parent: number | null;
}

/** The text a branch would send, with each variable as one character: enough to tell where lines end. */
function flat(pieces: readonly DisplayPiece[]): string {
  return pieces
    .map((p) => (p.kind === 'text' ? p.text : p.kind === 'cond' ? flat(p.branches[0].pieces) : 'x'))
    .join('');
}

export function layoutForDisplay(pieces: readonly DisplayPiece[]): {
  shown: Shown[];
  legend: { n: number; note: Note }[];
} {
  const legend: { n: number; note: Note }[] = [];
  let next = 1;
  const walk = (list: readonly DisplayPiece[], parent: number | null, startsLine: boolean): Shown[] => {
    const out: Shown[] = [];
    let atLineStart = startsLine;
    for (const p of list) {
      if (p.kind !== 'cond') {
        out.push(p);
        atLineStart = p.kind === 'text' ? p.text.endsWith('\n') : false;
        continue;
      }
      const only = p.branches.length === 1 ? p.branches[0] : null;
      const body = only ? flat(only.pieces) : '';
      if (only && atLineStart && body.endsWith('\n') && !body.slice(0, -1).includes('\n')) {
        out.push({ kind: 'line', note: { branch: only, prev: null, parent }, pieces: walk(only.pieces, parent, true) });
        atLineStart = true;
        continue;
      }
      let prev: number | null = null;
      for (const branch of p.branches) {
        const n = next++;
        legend.push({ n, note: { branch, prev, parent } });
        out.push({ kind: 'part', n, pieces: walk(branch.pieces, n, atLineStart) });
        prev = n;
      }
      atLineStart = false;
    }
    return out;
  };
  return { shown: walk(pieces, null, true), legend };
}

/** The words a condition is said with, in the operator's language. */
export interface ConditionWords {
  labelOf: (name: string) => string;
  /** A part's number as it is drawn: ③. */
  numberOf: (n: number) => string;
  present: (name: string) => string;
  differs: (name: string, other: string) => string;
  /** Joins two or more clauses. */
  and: string;
  when: (cond: string) => string;
  otherwiseWhen: (prev: string, cond: string) => string;
  otherwise: (prev: string) => string;
  inside: (text: string, parent: string) => string;
}

/** "only when Address differs from Node name", "only when ③ is not sent (inside ②)". */
export function describeNote(note: Note, words: ConditionWords): string {
  const { branch, prev, parent } = note;
  const cond =
    branch.when === null
      ? null
      : 'raw' in branch.when
        ? branch.when.raw
        : branch.when.terms
            .map((t) =>
              t.kind === 'present'
                ? words.present(words.labelOf(t.name))
                : words.differs(words.labelOf(t.name), words.labelOf(t.other)),
            )
            .join(words.and);
  const prevText = prev === null ? '' : words.numberOf(prev);
  const text =
    cond === null
      ? words.otherwise(prevText)
      : branch.otherwise && prev !== null
        ? words.otherwiseWhen(prevText, cond)
        : words.when(cond);
  return parent === null ? text : words.inside(text, words.numberOf(parent));
}

/** ① … ⑳, then (21) — the numbers a legend can run to. */
export function circled(n: number): string {
  return n >= 1 && n <= 20 ? String.fromCodePoint(0x2460 + n - 1) : `(${n})`;
}

/** A built-in body without the title sentence it starts with, which the view draws as one tag
 *  (Inc.2 decision 10). A body that does not start with it is returned whole. */
export function bodyAfterTitle(body: string, title: string): string {
  return body.startsWith(title) ? body.slice(title.length) : body;
}

/** Whether a channel sends a template of its own. Blank text is not one: it sends the built-in. */
export function hasOwnTemplate(channel: { subject_template?: string | null; body_template?: string | null }): boolean {
  const own = (s: string | null | undefined) => s != null && s.trim() !== '';
  return own(channel.subject_template) || own(channel.body_template);
}

/** The built-in text of one field at each point in the alert's life; `null` when there is none. */
function perEvent(
  templates: readonly BuiltinSubjectTemplate[],
  field: 'subject' | 'body',
): Record<NotifyEvent, string> | null {
  const out: Partial<Record<NotifyEvent, string>> = {};
  for (const e of TEMPLATE_EVENTS) {
    const t = templates.find((x) => x.event === e);
    const text = t ? (field === 'subject' ? t.subject : t.body) : null;
    if (text == null) return null;
    out[e] = text;
  }
  return out as Record<NotifyEvent, string>;
}

/** The server renders a template with one trailing newline dropped, so a piece that is not at the
 *  end of the template has to drop it itself to send the same text. */
function inner(text: string): string {
  return text.endsWith('\n') ? text.slice(0, -1) : text;
}

/** One template sending each event's own text: the event branch the editor itself writes. */
function branched(by: Record<NotifyEvent, string>): string {
  const arms = (['resolve', 'suppress'] as const).filter((e) => by[e] !== by.fire);
  if (arms.length === 0) return by.fire;
  return (
    arms.map((e, i) => `{% ${i === 0 ? 'if' : 'elif'} event == "${e}" %}${inner(by[e])}`).join('') +
    `{% else %}${inner(by.fire)}{% endif %}`
  );
}

/**
 * The built-in text as one subject and one body, for the code editor (decision 4). Rendering it
 * sends what the channel sends today, at every point in the alert's life.
 *
 * The body repeats the subject's sentence on its first line; when every event's body is its own
 * subject followed by the same lines, only that first line branches on the event, so the copy an
 * operator edits is not the same fifteen lines three times.
 */
export function builtinSource(templates: readonly BuiltinSubjectTemplate[]): { subject: string; body: string } | null {
  const subject = perEvent(templates, 'subject');
  if (!subject) return null;
  const body = perEvent(templates, 'body');
  if (!body) return { subject: branched(subject), body: '' };
  const rests = TEMPLATE_EVENTS.map((e) => (body[e].startsWith(subject[e]) ? body[e].slice(subject[e].length) : null));
  const shared = rests[0] !== null && rests.every((r) => r === rests[0]);
  return {
    subject: branched(subject),
    body: shared ? branched(subject) + rests[0] : branched(body),
  };
}
