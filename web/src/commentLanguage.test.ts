// SPDX-License-Identifier: AGPL-3.0-only
// ADR-190: every comment in the repository is written in English — the WebUI's half.
//
// The Rust half is `crates/yagra-common/src/comment_language.rs`, which says why the rule exists
// and why it is an allow-list ("every character is one English prose uses") rather than a ban on
// one script. What this file adds is the reader: comments are found with the TypeScript parser,
// not a line filter, because a `//` inside a string or a regex is not a comment, and a JSX
// `{/* … */}` is one.
//
// ⚠️ `ALLOWED_RANGES` is written twice, here and in Rust; `the_allowed_ranges_are_the_webuis` on
// the Rust side parses this table (one `[lo, hi]` pair per line, in hex) and compares the two.
//
// Read: `web/src` (tests included, the generated `api/` excluded — its text comes from Rust doc
// comments, which the Rust half checks), `web/tests`, `web/scripts`, and the repository's own
// `scripts/*.{js,mjs}`. String literals are not read: a Japanese locale string is data.
import { readFileSync } from 'node:fs';
import { join, relative } from 'node:path';
import ts from 'typescript';
import { describe, expect, it } from 'vitest';
import { SRC, sourceFiles } from './testSupport/sources';

/** The characters an English comment may contain beyond ASCII, as inclusive code-point ranges. */
const ALLOWED_RANGES: readonly (readonly [number, number])[] = [
  [0x00A0, 0x017F], // Latin-1 Supplement + Latin Extended-A: é, ü, ×, °, ±, µ, §
  [0x0370, 0x03FF], // Greek: α, Δ, Σ in maths
  [0x2000, 0x2BFF], // punctuation, arrows, maths, technical, box drawing, shapes, symbols
  [0xFE00, 0xFE0F], // variation selectors (the emoji form of ⚠)
  [0x1F000, 0x1FAFF], // emoji
];

function isEnglish(text: string): boolean {
  for (const ch of text) {
    const cp = ch.codePointAt(0) ?? 0;
    if (cp < 0x80) continue;
    if (!ALLOWED_RANGES.some(([lo, hi]) => cp >= lo && cp <= hi)) return false;
  }
  return true;
}

/** `[1-based line, text]` for each line of each comment. */
type CommentLines = [number, string][];

/** Every comment in a TS/TSX/JS file, found by walking the parser's tokens. */
function scriptComments(fileName: string, text: string): CommentLines {
  const kind = fileName.endsWith('.tsx')
    ? ts.ScriptKind.TSX
    : /\.m?js$/.test(fileName)
      ? ts.ScriptKind.JS
      : ts.ScriptKind.TS;
  const sf = ts.createSourceFile(fileName, text, ts.ScriptTarget.Latest, false, kind);
  const seen = new Set<number>();
  const out: CommentLines = [];
  const take = (ranges: ts.CommentRange[] | undefined) => {
    for (const r of ranges ?? []) {
      if (seen.has(r.pos)) continue;
      seen.add(r.pos);
      const first = sf.getLineAndCharacterOfPosition(r.pos).line + 1;
      text
        .slice(r.pos, r.end)
        .split('\n')
        .forEach((l, i) => out.push([first + i, l]));
    }
  };
  // Both kinds of range are asked for at every node's *full start* — the end of the token before
  // it — and never at a node's end. TypeScript calls a comment on the previous token's line
  // "trailing" and one below it "leading", so both are needed; but asked for after a JSX tag,
  // trailing ranges would scan the text that follows it (`<p>// words</p>`) as trivia. Starting
  // from each node's full start and skipping JSX text itself never looks there.
  // JSX text is text, not trivia: scanning it would read `// …` in a paragraph as a comment. The
  // positions are collected first because the list wrapping a JSX element's children starts at
  // the same position as its first piece of text.
  const jsxText = new Set<number>();
  const findText = (node: ts.Node) => {
    if (node.kind === ts.SyntaxKind.JsxText) jsxText.add(node.pos);
    ts.forEachChild(node, findText);
  };
  findText(sf);
  const visit = (node: ts.Node) => {
    if (node.kind === ts.SyntaxKind.JsxText) return;
    if (!jsxText.has(node.pos)) {
      take(ts.getTrailingCommentRanges(text, node.pos));
      take(ts.getLeadingCommentRanges(text, node.pos));
    }
    // A JSDoc node *is* a comment (the one at the end of a file hangs off the end-of-file token):
    // its own range has just been taken, and its children are positions inside it.
    if (node.kind === ts.SyntaxKind.JSDoc) return;
    node.getChildren(sf).forEach(visit);
  };
  visit(sf);
  return out.sort((a, b) => a[0] - b[0]);
}

/** Every `/* … *\/` comment in a stylesheet, outside quoted strings. */
function cssComments(text: string): CommentLines {
  const out: CommentLines = [];
  let line = 1;
  let quote: string | null = null;
  for (let i = 0; i < text.length; i++) {
    const c = text[i];
    if (c === '\n') line++;
    if (quote) {
      if (c === '\\') i++;
      else if (c === quote) quote = null;
    } else if (c === '"' || c === "'") {
      quote = c;
    } else if (c === '/' && text[i + 1] === '*') {
      const end = text.indexOf('*/', i + 2);
      const body = text.slice(i, end < 0 ? text.length : end + 2);
      body.split('\n').forEach((l, k) => out.push([line + k, l]));
      line += body.split('\n').length - 1;
      i += body.length - 1;
    }
  }
  return out;
}

const WEB = join(SRC, '..');
const REPO = join(WEB, '..');

describe('every comment is written in English (ADR-190)', () => {
  const files = [
    ...sourceFiles(SRC, {
      exts: ['.ts', '.tsx', '.css'],
      includeTests: true,
      declarations: true,
      skipDirs: ['api'],
    }),
    ...sourceFiles(join(WEB, 'tests'), { includeTests: true }),
    ...sourceFiles(join(WEB, 'scripts'), { exts: ['.mjs', '.js'] }),
    ...sourceFiles(join(REPO, 'scripts'), { exts: ['.mjs', '.js'] }),
  ];

  // Reading and lexing 900+ files takes 7-9 s on a warm machine, past Vitest's 5 s default;
  // the budget is for the walk, not a sign of a hang.
  it('in every source file the WebUI owns', () => {
    const offences: string[] = [];
    let commentLines = 0;
    for (const file of files) {
      const text = readFileSync(file, 'utf8');
      const comments = file.endsWith('.css') ? cssComments(text) : scriptComments(file, text);
      commentLines += comments.length;
      const where = relative(REPO, file).replace(/\\/g, '/');
      for (const [line, body] of comments) {
        if (!isEnglish(body)) offences.push(`${where}:${line}: ${body.trim()}`);
      }
    }
    // Floors on what was inspected: an empty walk or a parser that stopped yielding comments
    // would otherwise read as a clean tree.
    expect(files.length).toBeGreaterThan(900);
    expect(commentLines).toBeGreaterThan(30_000);
    expect(offences, 'rewrite these comments in English').toEqual([]);
  }, 60_000);
});

describe('the comment reader', () => {
  it('finds comments and nothing inside strings, templates, regexes or JSX text', () => {
    const src = [
      "const url = 'http://example.com'; // trailing",
      'const t = `a // not ${1} a comment`;',
      'const re = /\\/\\/not/g; /* block',
      'still block */',
      'const el = <p>// text, not a comment {/* jsx comment */}</p>;',
      '/** doc */',
    ].join('\n');
    expect(scriptComments('x.tsx', src)).toEqual([
      [1, '// trailing'],
      [3, '/* block'],
      [4, 'still block */'],
      [5, '/* jsx comment */'],
      [6, '/** doc */'],
    ]);
  });

  it('finds stylesheet comments outside strings', () => {
    const css = 'a::before { content: "/* no */"; } /* one\ntwo */\nb {}';
    expect(cssComments(css)).toEqual([
      [1, '/* one'],
      [2, 'two */'],
    ]);
  });

  it('passes English prose and refuses Japanese', () => {
    expect(isEnglish('// ADR-164 decision 18 — “menu” → ✓ ⚠️ 🚨 ×2 °C α ▸ ─ café')).toBe(true);
    for (const bad of [
      '// ADR-164 決定 18', // kanji
      '// です', // hiragana
      '// 「quoted」', // ideographic brackets
      '// width （fullwidth）', // fullwidth parentheses
      '// при', // Cyrillic
    ]) {
      expect(isEnglish(bad), bad).toBe(false);
    }
  });
});
