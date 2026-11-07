// SPDX-License-Identifier: AGPL-3.0-only
// The rules ADR-246 holds every WebUI stylesheet and component to, counted one file at a time.
//
// Two readers share this module, and the point of sharing it is that they cannot disagree:
//   - `src/uiBudget.test.ts` counts the whole tree against `src/styles/uiBudget.json` (the gate
//     that `npm run test`, `/flashdeploy` and CI run);
//   - `.claude/hooks/ui-guard.js` counts the one file Claude just wrote, so a raw `gap: 10px` is
//     reported while the edit is still on screen rather than at release time.
//
// What is counted, per file:
//   spacing        a raw length in a padding / margin / gap / border-radius declaration (CSS)
//   color          a raw colour in a colour-bearing declaration (CSS)
//   fontSize       a raw length in `font-size` (CSS)
//   inlineSpacing  a literal number or px string for a spacing key inside `style={{ }}` (TSX)
//   legacy         a class name the shared layout parts replace (TSX outside components/ui)
//   uppercase      `text-transform: uppercase` (CSS) or `textTransform: 'uppercase'` (TSX):
//                  design-system.md §1.4 says sentence case everywhere, and a transform also
//                  rewrites whatever data lands in the text - a mount path, a pool name
//   lines          the length of a page-local stylesheet (outside styles/ and components/ui/)
//
// Each count has a ceiling in the budget that only goes down. A file with no entry has a ceiling
// of zero, except a page-local stylesheet, which must be listed with the reason it exists.
//
// A line that must keep a raw value carries `ui-allow: <reason>` in a comment on that line. An
// allow with no reason, or on a line where nothing needed allowing, is itself a problem, so an
// exemption cannot outlive the value it was written for.
//
//   node scripts/ui-rules.mjs src/pages/FooPage.css   # what is wrong in a file, with the fix
//   node scripts/ui-rules.mjs --lower                 # lower every ceiling to today's count
//   node scripts/ui-rules.mjs --init                  # write the first budget (ADR-246 Inc.1)

import { existsSync, readdirSync, readFileSync, statSync, writeFileSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

export const WEB = join(dirname(fileURLToPath(import.meta.url)), '..');
export const SRC = join(WEB, 'src');
export const BUDGET_PATH = join(SRC, 'styles', 'uiBudget.json');

/** The spacing scale, in px, with the token that names each step (ADR-246 decision 1). */
export const SPACE_STEPS = [
  [2, '--space-0-5'],
  [4, '--space-1'],
  [6, '--space-1-5'],
  [8, '--space-2'],
  [12, '--space-3'],
  [16, '--space-4'],
  [24, '--space-5'],
  [32, '--space-6'],
];

/** The counted metrics that have a per-file ceiling. `lines` is handled apart (it needs a why). */
export const METRICS = ['spacing', 'color', 'fontSize', 'inlineSpacing', 'legacy', 'uppercase'];

/** How far below its ceiling a count may sit before the ceiling must come down with it. */
export const SLACK = { count: 10, lines: 30 };

/** Class names the shared layout parts replace. Counted in TSX outside `components/ui/`, where
 *  the parts themselves still render some of them until ADR-246 Inc.8 deletes them. */
export const LEGACY_CLASSES = [
  'modal-field',
  'modal-field-label',
  'modal-field-row',
  'modal-field-value',
  'form-stack',
  'form-label',
  'form-row',
  'form-check',
  'form-warning',
  'page-fill',
  'table-section-title',
];

const SPACING_PROP =
  /^(?:(?:padding|margin)(?:-(?:top|right|bottom|left|inline|block|inline-start|inline-end|block-start|block-end))?|gap|row-gap|column-gap|grid-gap|grid-row-gap|grid-column-gap|border(?:-(?:top|bottom|start|end)-(?:left|right|start|end))?-radius)$/;
const RADIUS_PROP = /radius$/;
/** A page's own custom property that holds a spacing value (`--pane-gap: 6px`). Without this a
 *  raw value could hide one step away, in a variable the declaration then reads. Width-like names
 *  (`--col-w`) are not spacing and are left alone. */
const SPACING_VAR = /^--[\w-]*(?:gap|pad|space|spacing|margin|radius|inset)[\w-]*$/;
const COLOR_PROP =
  /^(?:color|background(?:-color|-image)?|border(?:-(?:top|right|bottom|left|inline|block))?(?:-color)?|outline(?:-color)?|fill|stroke|box-shadow|text-shadow|caret-color|accent-color|text-decoration(?:-color)?|column-rule(?:-color)?)$/;
const NAMED_COLORS =
  /(?<![\w-])(?:white|black|red|green|blue|gr[ae]y|orange|yellow|purple|pink|silver|navy|teal|maroon|olive|lime|aqua|fuchsia)(?![\w-])/;
const RAW_LENGTH = /(?<![\w.-])-?(?:\d+\.?\d*|\.\d+)(?:px|rem|em|pt)(?![\w-])/g;
const STYLE_KEY =
  /(?<![\w$])((?:padding|margin)(?:Top|Right|Bottom|Left|Inline|Block|InlineStart|InlineEnd|BlockStart|BlockEnd)?|gap|rowGap|columnGap|borderRadius|border(?:Top|Bottom)(?:Left|Right)Radius)\s*:\s*([^,}\n]+)/g;
const ALLOW = /ui-allow:([^*\n]*?)(?:\*\/|$)/;

/** `var(...)` (one level of nested parentheses) and `url(...)` removed: a token is not a raw value,
 *  and neither is a fallback inside one or the contents of an inline SVG. */
function withoutTokensAndUrls(value) {
  return value
    .replace(/var\((?:[^()]|\([^()]*\))*\)/g, ' ')
    .replace(/url\((?:[^()]|\([^()]*\))*\)/g, ' ');
}

function isZero(token) {
  return Number.parseFloat(token) === 0;
}

function lineStarts(text) {
  const starts = [0];
  for (let i = 0; i < text.length; i++) if (text[i] === '\n') starts.push(i + 1);
  return starts;
}

function lineOf(starts, index) {
  let lo = 0;
  let hi = starts.length - 1;
  while (lo < hi) {
    const mid = (lo + hi + 1) >> 1;
    if (starts[mid] <= index) lo = mid;
    else hi = mid - 1;
  }
  return lo + 1;
}

/** `ui-allow:` markers by line number, read before comments are blanked. */
function allowsByLine(text) {
  const out = new Map();
  text.split('\n').forEach((l, i) => {
    const m = ALLOW.exec(l);
    if (m) out.set(i + 1, m[1].trim());
  });
  return out;
}

/** A page-local stylesheet: one that belongs to a screen rather than to the shared layer. */
export function isPageCss(rel) {
  return (
    rel.endsWith('.css') &&
    rel !== 'index.css' &&
    !rel.startsWith('styles/') &&
    !rel.startsWith('components/ui/')
  );
}

function emptyResult() {
  return {
    spacing: [],
    color: [],
    fontSize: [],
    inlineSpacing: [],
    legacy: [],
    uppercase: [],
    lines: null,
    allowProblems: [],
    inspected: 0,
  };
}

/** Applies the `ui-allow:` markers: drops allowed findings, reports empty and unused markers. */
function applyAllows(result, allows) {
  const used = new Set();
  for (const m of METRICS) {
    result[m] = result[m].filter((v) => {
      if (!allows.has(v.line)) return true;
      used.add(v.line);
      return false;
    });
  }
  for (const [line, why] of allows) {
    if (!why) result.allowProblems.push({ line, problem: 'a ui-allow marker gives no reason' });
    else if (!used.has(line))
      result.allowProblems.push({ line, problem: 'a ui-allow marker on a line with nothing to allow' });
  }
}

/** Counts one stylesheet. `rel` is the path relative to `web/src`, with forward slashes. */
export function measureCss(rel, text) {
  const result = emptyResult();
  const allows = allowsByLine(text);
  // Comments blanked to spaces so every index keeps its line.
  const code = text.replace(/\/\*[\s\S]*?\*\//g, (c) => c.replace(/[^\n]/g, ' '));
  const starts = lineStarts(code);
  const definesTokens = rel === 'styles/tokens.css';
  const decl = /(?<![\w.#:-])(-{0,2}[a-z][a-z0-9-]*)\s*:\s*([^;{}]*)/g;
  for (const m of code.matchAll(decl)) {
    const prop = m[1];
    const value = m[2].trim();
    const line = lineOf(starts, m.index);
    const bare = withoutTokensAndUrls(value);
    if (SPACING_PROP.test(prop) || SPACING_VAR.test(prop)) {
      result.inspected++;
      if (definesTokens) continue;
      for (const t of bare.match(RAW_LENGTH) ?? []) {
        if (!isZero(t)) result.spacing.push({ line, prop, value, raw: t });
      }
    } else if (prop === 'text-transform') {
      result.inspected++;
      if (/uppercase/.test(bare)) result.uppercase.push({ line, prop, value, raw: 'uppercase' });
    } else if (prop === 'font-size') {
      result.inspected++;
      if (definesTokens) continue;
      for (const t of bare.match(RAW_LENGTH) ?? []) {
        if (!isZero(t)) result.fontSize.push({ line, prop, value, raw: t });
      }
    } else if (COLOR_PROP.test(prop)) {
      result.inspected++;
      if (definesTokens) continue;
      const raw =
        /#[0-9a-fA-F]{3,8}(?![\w-])/.exec(bare)?.[0] ??
        /(?<![\w-])(?:rgba?|hsla?)\(/.exec(bare)?.[0] ??
        NAMED_COLORS.exec(bare)?.[0];
      if (raw) result.color.push({ line, prop, value, raw });
    }
  }
  if (isPageCss(rel)) result.lines = text.split('\n').length - (text.endsWith('\n') ? 1 : 0);
  applyAllows(result, allows);
  return result;
}

/** Counts one component. `rel` is the path relative to `web/src`, with forward slashes. */
export function measureTsx(rel, text) {
  const result = emptyResult();
  const allows = allowsByLine(text);
  // Comment lines blanked, the same rule as `src/testSupport/sources.ts::codeOnly`.
  const code = text
    .split('\n')
    .map((l) => {
      const t = l.trimStart();
      return t.startsWith('//') || t.startsWith('*') || t.startsWith('/*') ? '' : l;
    })
    .join('\n');
  const starts = lineStarts(code);

  for (const m of code.matchAll(/style=\{\{/g)) {
    // Walk to the brace that closes the object literal.
    let depth = 1;
    let i = m.index + m[0].length;
    while (i < code.length && depth > 0) {
      if (code[i] === '{') depth++;
      else if (code[i] === '}') depth--;
      i++;
    }
    const body = code.slice(m.index + m[0].length, i - 1);
    const base = m.index + m[0].length;
    for (const k of body.matchAll(STYLE_KEY)) {
      result.inspected++;
      const value = k[2].trim();
      // `'var(--space-2)'` is the sanctioned spelling; its digit is part of a name, not a length.
      const bare = withoutTokensAndUrls(value);
      const literal = /^-?(?:\d+\.?\d*|\.\d+)$/.test(bare.trim())
        ? isZero(bare)
          ? null
          : bare.trim()
        : (/['"`][^'"`]*?(-?(?:\d+\.?\d*|\.\d+)(?:px|rem|em))[^'"`]*['"`]/.exec(bare)?.[1] ??
          /(?<![\w.$-])(?:\d+\.?\d*|\.\d+)(?![\w.])/.exec(bare)?.[0] ??
          null);
      if (literal && !isZero(literal))
        result.inlineSpacing.push({
          line: lineOf(starts, base + k.index),
          prop: k[1],
          value,
          raw: literal,
        });
    }
  }

  for (const m of code.matchAll(/textTransform\s*:\s*['"`]uppercase['"`]/g)) {
    result.inspected++;
    result.uppercase.push({
      line: lineOf(starts, m.index),
      prop: 'textTransform',
      value: 'uppercase',
      raw: 'uppercase',
    });
  }

  if (!rel.startsWith('components/ui/')) {
    const legacy = new RegExp(
      `(?<=["'\`\\s{])(${LEGACY_CLASSES.map((c) => c.replace(/-/g, '\\-')).join('|')})(?![\\w-])`,
      'g',
    );
    for (const m of code.matchAll(legacy)) {
      result.inspected++;
      result.legacy.push({ line: lineOf(starts, m.index), prop: 'className', value: m[1], raw: m[1] });
    }
  }
  applyAllows(result, allows);
  return result;
}

/** Counts one file of either kind; `null` for a file this module has no rules for. */
export function measure(rel, text) {
  if (rel.endsWith('.css')) return measureCss(rel, text);
  if (rel.endsWith('.tsx')) return measureTsx(rel, text);
  return null;
}

/** What to write instead of a raw value. */
export function suggest(v) {
  const px = /^(-?(?:\d+\.?\d*|\.\d+))px$/.exec(v.raw);
  if (RADIUS_PROP.test(v.prop) || /Radius$/.test(v.prop)) {
    if (!px) return 'var(--radius-sm) / var(--radius) / var(--radius-pill)';
    const n = Math.abs(Number(px[1]));
    if (n >= 100) return 'var(--radius-pill)';
    if (n <= 4) return 'var(--radius-sm)';
    // A chip or a track rounded to half its height is a pill whatever the number says.
    return n >= 8 ? 'var(--radius-pill) if round at its ends, else var(--radius)' : 'var(--radius)';
  }
  if (v.prop === 'font-size') return 'a --font-* token, or `ui-allow:` for SVG user units';
  if (v.prop === 'text-transform' || v.prop === 'textTransform')
    return 'nothing - write the text in sentence case (design-system.md §1.4)';
  if (v.prop === 'className') return 'the shared part that replaces it (ui-conventions.md, "画面の型")';
  if (!px && !/^-?(?:\d+\.?\d*|\.\d+)$/.test(v.raw)) return 'a --space-* step (convert to px first)';
  const n = Math.abs(Number(px ? px[1] : v.raw));
  const sign = (px ? px[1] : v.raw).startsWith('-') ? 'calc(-1 * ' : '';
  const wrap = (t) => (sign ? `${sign}var(${t}))` : `var(${t})`);
  const exact = SPACE_STEPS.find(([s]) => s === n);
  if (exact) return wrap(exact[1]);
  const below = [...SPACE_STEPS].reverse().find(([s]) => s < n);
  const above = SPACE_STEPS.find(([s]) => s > n);
  if (!below) return wrap(SPACE_STEPS[0][1]);
  if (!above) return `${wrap(SPACE_STEPS.at(-1)[1])}, or a layout size if this is not spacing`;
  return `${wrap(below[1])} inside a component, ${wrap(above[1])} between blocks`;
}

/** The budget's ceiling for one metric of one file. */
export function ceilingOf(budget, metric, rel) {
  return budget?.[metric]?.[rel] ?? 0;
}

/** Compares one measured file with the budget. Each problem says what to do about it. */
export function compare(rel, result, budget) {
  const problems = [];
  for (const metric of METRICS) {
    const count = result[metric].length;
    const ceiling = ceilingOf(budget, metric, rel);
    if (count > ceiling) problems.push({ kind: 'over', metric, count, ceiling, items: result[metric] });
    else if (ceiling - count >= SLACK.count) problems.push({ kind: 'lower', metric, count, ceiling });
  }
  if (result.lines !== null) {
    const entry = budget?.pageCss?.[rel];
    if (!entry) problems.push({ kind: 'unlisted', metric: 'lines', count: result.lines });
    else if (!String(entry.why ?? '').trim()) problems.push({ kind: 'noWhy', metric: 'lines' });
    else if (result.lines > entry.lines)
      problems.push({ kind: 'over', metric: 'lines', count: result.lines, ceiling: entry.lines, items: [] });
    else if (entry.lines - result.lines >= SLACK.lines)
      problems.push({ kind: 'lower', metric: 'lines', count: result.lines, ceiling: entry.lines });
  }
  for (const a of result.allowProblems) problems.push({ kind: 'allow', metric: 'allow', ...a });
  return problems;
}

/** A problem as one paragraph a person (or Claude) can act on. */
export function describe(rel, p) {
  switch (p.kind) {
    case 'over': {
      if (p.metric === 'lines')
        return `${rel}: ${p.count} lines, over its ceiling of ${p.ceiling}. Build the frame from the shared parts in components/ui instead of growing this stylesheet. Only if the new lines are this screen's own visuals (a map, a chart, an editor) raise its "pageCss" ceiling in src/styles/uiBudget.json, and say why in the commit message.`;
      const lines = p.items
        .slice(0, 12)
        .map((v) => `  line ${v.line}: ${v.prop}: ${v.value}  ->  ${suggest(v)}`)
        .join('\n');
      const more = p.items.length > 12 ? `\n  ... and ${p.items.length - 12} more` : '';
      return `${rel}: ${p.count} ${p.metric} finding(s), over its ceiling of ${p.ceiling} (ADR-246).\n${lines}${more}`;
    }
    case 'lower':
      return `${rel}: ${p.metric} is ${p.count}, ceiling ${p.ceiling}. Lower the ceiling in src/styles/uiBudget.json to ${p.count} in this change (\`node scripts/ui-rules.mjs --lower\`).`;
    case 'unlisted':
      return `${rel}: a new page-local stylesheet (${p.count} lines). Prefer the shared parts in components/ui; if this screen really has its own visuals, add it to "pageCss" in src/styles/uiBudget.json with {"lines": ${p.count}, "why": "<what is unique about this screen>"}.`;
    case 'noWhy':
      return `${rel}: its "pageCss" entry in src/styles/uiBudget.json has no "why".`;
    case 'allow':
      return `${rel}: line ${p.line}: ${p.problem}.`;
    default:
      return `${rel}: ${p.kind}`;
  }
}

/** Problems that must stop a change (everything but "lower the ceiling"). */
export function isBlocking(p) {
  return p.kind !== 'lower';
}

export function readBudget(path = BUDGET_PATH) {
  return existsSync(path) ? JSON.parse(readFileSync(path, 'utf8')) : {};
}

/** Every `.css` and `.tsx` under `web/src` except the generated API, as `[rel, abs]`. For the
 *  command line and the hook only — the test walks through `src/testSupport/sources.ts`. */
export function listFiles(root = SRC) {
  const out = [];
  const walk = (dir) => {
    for (const e of readdirSync(dir)) {
      const p = join(dir, e);
      const r = p.slice(root.length + 1).replace(/\\/g, '/');
      if (statSync(p).isDirectory()) {
        if (r !== 'api') walk(p);
      } else if ((e.endsWith('.css') || e.endsWith('.tsx')) && !/\.test\.tsx$/.test(e)) {
        out.push([r, p]);
      }
    }
  };
  walk(root);
  return out.sort(([a], [b]) => a.localeCompare(b));
}

function sortedObject(o) {
  return Object.fromEntries(Object.entries(o).sort(([a], [b]) => a.localeCompare(b)));
}

/** Writes the budget with its keys in a stable order, so a diff shows only what moved. */
export function writeBudget(budget, path = BUDGET_PATH) {
  const out = { _doc: budget._doc };
  for (const m of METRICS) out[m] = sortedObject(budget[m] ?? {});
  out.pageCss = sortedObject(budget.pageCss ?? {});
  writeFileSync(path, `${JSON.stringify(out, null, 2)}\n`);
}

const DOC =
  'ADR-246. Per-file ceilings that only go down: a count over its ceiling fails src/uiBudget.test.ts, and so does one 10 or more below it (30 for lines) - lower the ceiling in the same change. A file with no entry has a ceiling of 0; a page-local stylesheet must be listed in pageCss with the reason it exists. `node scripts/ui-rules.mjs --lower` lowers every ceiling to today\'s count and never raises one.';

function main(args) {
  const files = listFiles();
  // A metric added to METRICS starts from today's counts; every other ceiling is left alone.
  // `--lower` cannot do this - it never raises, so a new metric would start at zero everywhere.
  const add = args.indexOf('--add-metric');
  if (add >= 0) {
    const metric = args[add + 1];
    if (!METRICS.includes(metric)) {
      console.error(`--add-metric needs one of: ${METRICS.join(', ')}`);
      process.exit(1);
    }
    const budget = readBudget();
    if (budget[metric] && Object.keys(budget[metric]).length) {
      console.error(`"${metric}" already has ceilings; use --lower.`);
      process.exit(1);
    }
    budget[metric] = {};
    for (const [rel, abs] of files) {
      const n = measure(rel, readFileSync(abs, 'utf8'))[metric].length;
      if (n > 0) budget[metric][rel] = n;
    }
    budget._doc = DOC;
    writeBudget(budget);
    console.log(`Added "${metric}": ${Object.keys(budget[metric]).length} files`);
    return;
  }
  if (args.includes('--init') || args.includes('--lower')) {
    const init = args.includes('--init');
    if (init && existsSync(BUDGET_PATH) && !args.includes('--force')) {
      console.error('The budget already exists; use --lower, or --init --force to start over.');
      process.exit(1);
    }
    const old = init ? {} : readBudget();
    const next = { _doc: DOC, pageCss: {} };
    for (const m of METRICS) next[m] = {};
    for (const [rel, abs] of files) {
      const r = measure(rel, readFileSync(abs, 'utf8'));
      for (const m of METRICS) {
        const n = r[m].length;
        const ceiling = init ? n : Math.min(n, ceilingOf(old, m, rel));
        if (ceiling > 0) next[m][rel] = ceiling;
      }
      if (r.lines !== null) {
        const prev = old.pageCss?.[rel];
        if (init) next.pageCss[rel] = { lines: r.lines, why: 'before ADR-246' };
        else if (prev) next.pageCss[rel] = { lines: Math.min(r.lines, prev.lines), why: prev.why };
      }
    }
    writeBudget(next);
    console.log(`${init ? 'Wrote' : 'Lowered'} ${BUDGET_PATH}`);
    return;
  }
  const budget = readBudget();
  const targets = args.length
    ? args.map((a) => {
        const abs = join(process.cwd(), a);
        return [abs.slice(SRC.length + 1).replace(/\\/g, '/'), abs];
      })
    : files;
  let blocking = 0;
  for (const [rel, abs] of targets) {
    const r = measure(rel, readFileSync(abs, 'utf8'));
    if (!r) continue;
    for (const p of compare(rel, r, budget)) {
      if (isBlocking(p)) blocking++;
      console.log(describe(rel, p));
    }
  }
  process.exit(blocking ? 1 : 0);
}

if (
  process.argv[1] &&
  fileURLToPath(import.meta.url).toLowerCase() === resolve(process.argv[1]).toLowerCase()
) {
  main(process.argv.slice(2));
}
