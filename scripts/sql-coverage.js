// SPDX-License-Identifier: AGPL-3.0-only
//
// Map the statements a PostgreSQL server actually executed back to the source files that wrote
// them (ADR-116 decision 5). Run from the repo root, after `scripts/sql-coverage.sh` has produced
// the server log:
//
//   node scripts/sql-coverage.js /var/tmp/yagra-sql-coverage/pg.log
//
// Three buckets, and the third one is why this is a measurement and not a gate:
//
//   executed  — the file's SQL literal appears verbatim in the server log. Proof it ran.
//   unrun     — the literal is nowhere in the log. Almost certainly never executed.
//   unknown   — the argument to `sqlx::query…(` is not a string literal (built with `format!`,
//               concatenated, or held in a `const`). This reader cannot resolve it, so it says so
//               rather than guessing. `repo/listing.rs` is all four of its statements here, and
//               ADR-114's database tests certainly do run them.
//
// ⚠️ **`unrun` is an upper bound, not an exact count.** A false positive is near-impossible (the
// match is containment of the whole normalised statement), but a false negative is not: a literal
// the driver rewrote before sending would not be found. Read the number as "at most this many".
//
// 🔍 Sanity check when changing this file: `repo/seed.rs` must come out at 0 unrun (every fixture
// seeds), and the unrun statements of `repo/nodes.rs` / `repo/pools.rs` must each be a method no
// test calls. If either stops holding, the reader is broken, not the suite.

const cp = require('child_process');
const fs = require('fs');

const logPath = process.argv[2];
if (!logPath) {
  console.error('usage: node scripts/sql-coverage.js <pg.log from scripts/sql-coverage.sh>');
  process.exit(2);
}

// --- 1. every statement the server executed, normalised -----------------------------------------
// A logged statement can span lines; every record starts with a timestamp, so anything else is a
// continuation of the one before it.
const lines = fs.readFileSync(logPath, 'utf8').split('\n');
const records = [];
let cur = null;
for (const line of lines) {
  if (/^\d{4}-\d{2}-\d{2} \d{2}:\d{2}:\d{2}/.test(line)) {
    if (cur !== null) records.push(cur);
    cur = line;
  } else if (cur !== null) {
    cur += ' ' + line;
  }
}
if (cur !== null) records.push(cur);

const norm = (s) => s.replace(/\s+/g, ' ').trim();
const executed = new Set();
for (const r of records) {
  // sqlx uses prepared statements, so most records are `execute sqlx_s_N: <sql>`; the migrator and
  // the test harness issue a few directly as `statement: <sql>`.
  const m = r.match(/LOG:  (?:execute [^:]*: |statement: )([\s\S]*)$/);
  if (m) executed.add(norm(m[1]));
}
const haystack = ' ' + [...executed].join('  ') + ' ';

// --- 2. every SQL literal in the workspace's production source ----------------------------------
// `examples/` is excluded on purpose: the load-test rigs are not product code.
const files = cp
  .execSync('git ls-files crates', { maxBuffer: 1e8 })
  .toString()
  .split('\n')
  .filter((f) => f.endsWith('.rs') && f.includes('/src/'));

// Drop a top-level `#[cfg(test)] mod … { … }` before counting.
//
// ⚠️ The question is which SQL the *product* holds, and a test's own statement would otherwise be
// counted as production and then found in the log — inflating the executed side with the very
// thing being measured. (It did: adding one `query_scalar` to a test moved the totals.)
//
// Deliberately NOT the `srcread` rule, which removes **every** top-level test-only item. Cutting a
// stray test-only `use` would risk removing production text after it and *under*-report the gap,
// which is the direction that looks like success. This removes only the block it can see the
// braces of, so anything it fails to recognise stays counted.
function withoutTestModules(src) {
  let out = '';
  let i = 0;
  for (;;) {
    const at = src.indexOf('\n#[cfg(test)]', i);
    if (at < 0) return out + src.slice(i);
    const open = src.indexOf('{', at);
    if (open < 0) return out + src.slice(i);
    let depth = 0;
    let j = open;
    for (; j < src.length; j++) {
      if (src[j] === '{') depth++;
      else if (src[j] === '}' && --depth === 0) break;
    }
    out += src.slice(i, at);
    i = j + 1;
  }
}

const NEEDLE = 'sqlx::query';
const rows = [];
for (const f of files) {
  const s = withoutTestModules(fs.readFileSync(f, 'utf8'));
  let i = 0;
  let total = 0;
  let unknown = 0;
  const literals = [];
  while ((i = s.indexOf(NEEDLE, i)) >= 0) {
    total++;
    const open = s.indexOf('(', i);
    if (open < 0) { i += NEEDLE.length; continue; }
    let k = open + 1;
    while (k < s.length && /\s/.test(s[k])) k++;
    let lit = null;
    const rawStr = s.slice(k).match(/^r(#*)"/);
    if (rawStr) {
      const close = '"' + rawStr[1];
      const end = s.indexOf(close, k + rawStr[0].length);
      if (end > 0) lit = s.slice(k + rawStr[0].length, end);
    } else if (s[k] === '"') {
      let e = k + 1;
      let out = '';
      while (e < s.length && s[e] !== '"') {
        if (s[e] === '\\') { out += s[e + 1] === 'n' ? ' ' : s[e + 1]; e += 2; } else { out += s[e]; e++; }
      }
      lit = out;
    }
    if (lit === null) unknown++; else literals.push(norm(lit));
    i += NEEDLE.length;
  }
  if (total === 0) continue;
  // A very short literal could collide with an unrelated statement; require some substance.
  const ran = literals.filter((l) => l.length > 15 && haystack.includes(l));
  rows.push({ f, total, unknown, executed: ran.length, unrun: literals.filter((l) => !(l.length > 15 && haystack.includes(l))) });
}

rows.sort((a, b) => a.executed / a.total - b.executed / b.total || b.total - a.total);
const T = rows.reduce((n, r) => n + r.total, 0);
const E = rows.reduce((n, r) => n + r.executed, 0);
const U = rows.reduce((n, r) => n + r.unknown, 0);

console.log('distinct statements the server executed : ' + executed.size);
console.log('files under crates/*/src using sqlx     : ' + rows.length);
console.log('statements                              : ' + T);
console.log('  executed (proven)                     : ' + E);
console.log('  never executed (upper bound)          : ' + (T - E - U));
console.log('  unknown (not a string literal)        : ' + U);
console.log('');
console.log('run%  ran/total  unknown  file');
for (const r of rows) {
  console.log(
    String(Math.round((100 * r.executed) / r.total)).padStart(4) +
      '  ' + String(r.executed).padStart(3) + '/' + String(r.total).padEnd(5) +
      '  ' + String(r.unknown).padStart(7) +
      '  ' + r.f,
  );
}
