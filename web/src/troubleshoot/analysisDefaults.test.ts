// SPDX-License-Identifier: AGPL-3.0-only
import { readdirSync, readFileSync } from 'node:fs';
import { join, relative, sep } from 'node:path';
import { describe, expect, it } from 'vitest';
import i18n from '../i18n';
import {
  ANALYSIS_WINDOWS,
  analysisWindowLabelKey,
  BASELINE_SECS,
  DEFAULT_SIGMA,
  DEFAULT_WINDOW_SECS,
  defaultAnalysisInput,
  isAnalysisWindow,
} from './analysisDefaults';

// The builder takes the caller's `t`; the global instance (English bundled, default lng) is
// already a standalone bound translator, so pass it straight through for the pure-function test.
const t = i18n.t;

describe('the analysis windows', () => {
  it('offer the default window', () => {
    // The type already says so; this is what fails if the list is edited without the default.
    expect(ANALYSIS_WINDOWS.map((w) => w.secs)).toContain(DEFAULT_WINDOW_SECS);
    expect(isAnalysisWindow(DEFAULT_WINDOW_SECS)).toBe(true);
  });

  it('default to seven days over a fourteen-day baseline', () => {
    // Pinned as numbers on purpose. The drawer, the schedule form, the quick run and the re-run of
    // an old job all inherit these, so moving one is a decision to take here, not a side effect.
    expect(DEFAULT_WINDOW_SECS).toBe(7 * 24 * 3600);
    expect(BASELINE_SECS).toBe(14 * 24 * 3600);
  });

  it('run shortest first, each once, each under its own label', () => {
    const secs = ANALYSIS_WINDOWS.map((w) => w.secs);
    expect([...secs].sort((a, b) => a - b)).toEqual(secs);
    expect(new Set(secs).size).toBe(secs.length);
    const keys = ANALYSIS_WINDOWS.map((w) => w.labelKey);
    expect(new Set(keys).size).toBe(keys.length);
  });

  it('label a known window by its own key and an unknown one as the default', () => {
    for (const w of ANALYSIS_WINDOWS) expect(analysisWindowLabelKey(w.secs)).toBe(w.labelKey);
    // A schedule written through the API can carry any number; the editor labels it as the
    // default rather than rendering a raw key. What that hides is reported, not fixed, here.
    expect(isAnalysisWindow(3_600)).toBe(false);
    expect(analysisWindowLabelKey(3_600)).toBe(analysisWindowLabelKey(DEFAULT_WINDOW_SECS));
  });
});

describe('defaultAnalysisInput', () => {
  it('targets all nodes with the standard defaults', () => {
    const input = defaultAnalysisInput('anomaly', t);
    expect(input.tool).toBe('anomaly');
    expect(input.scope_kind).toBe('all');
    expect(input.scope_id).toBeNull();
    expect(input.depth).toBe('standard');
    expect(input.window_secs).toBe(DEFAULT_WINDOW_SECS);
    expect(input.baseline_secs).toBe(BASELINE_SECS);
    expect(input.sensitivity).toBe(DEFAULT_SIGMA);
  });

  it('labels the run in the active language, resolving both namespaces', () => {
    // The label crosses namespaces — the scope half moved to `common` with the picker while the
    // window half stayed in `troubleshoot`. A key that fails to resolve renders as the raw key,
    // which the i18n parity gate cannot catch (it compares locales, not resolution).
    const label = defaultAnalysisInput('anomaly', t).scope_label;
    expect(label).toBe('All nodes · 7 d');
    expect(label).not.toContain('scope.');
    expect(label).not.toContain('launch.');
  });
});

/**
 * ADR-184 increment 6's guard. The windows were spelled in four files and had drifted: the drawer,
 * the schedule form and the quick run said 7 days while the re-run of an old job said 24 hours.
 * `analysisDefaults.ts` owns the numbers now; this stops the next file from writing one of them
 * out again — as a literal, as a string, or as a product such as twenty-four times an hour, which
 * is how the drifted copy was spelled.
 *
 * ⚠️ **The needles are assembled at run time from the list itself**, and test files are never
 * scanned, so this file cannot match its own source — the trap
 * `reports/guards.rs::the_run_state_sql_is_built_from_the_enum` records on the Rust side.
 */
describe('no other Troubleshoot file spells an analysis window', () => {
  const ROOT = __dirname;
  const HOME = 'analysisDefaults.ts';

  /**
   * Files that keep numbers of their own, each with the reason it is not this vocabulary. An entry
   * whose file has stopped spelling one is refused below, so the list cannot outlive its reason.
   */
  const OWN_VOCABULARY: Record<string, string> = {
    'report/registry.tsx':
      'the report config bar offers per-report windows under report.common.windows — 1 h and 6 h ' +
      'exist there and 90 d does not — and every report picks its own default, so it is a ' +
      'different list, kept apart on purpose (ADR-184)',
  };

  /** The seconds only the home module may spell: every window, and the baseline. */
  const GUARDED = new Set<number>([...ANALYSIS_WINDOWS.map((w) => w.secs), BASELINE_SECS]);

  /** A number literal: digits with optional `_` groups, not part of a name or of a decimal. */
  const NUMBER = String.raw`(?<![\w.])\d[\d_]*(?![\w.])`;
  /** One number, or several joined by `*` — `24 * 3600` is a spelling of a day too. */
  const CHAIN = new RegExp(`${NUMBER}(?:\\s*\\*\\s*${NUMBER})*`, 'g');

  function sourceFiles(dir: string): string[] {
    return readdirSync(dir, { withFileTypes: true }).flatMap((e) => {
      const p = join(dir, e.name);
      if (e.isDirectory()) return sourceFiles(p);
      return /\.tsx?$/.test(e.name) && !e.name.endsWith('.test.ts') ? [p] : [];
    });
  }

  const rel = (p: string) => relative(ROOT, p).split(sep).join('/');

  interface Spelling {
    line: number;
    text: string;
    value: number;
  }

  /** Every number, or product of numbers, in one source text that equals a guarded second count. */
  function guardedSpellings(src: string): Spelling[] {
    const out: Spelling[] = [];
    for (const m of src.matchAll(CHAIN)) {
      const factors = m[0].split('*').map((f) => Number(f.trim().split('_').join('')));
      const product = factors.reduce((a, b) => a * b, 1);
      const hit = [product, ...factors].find((v) => GUARDED.has(v));
      if (hit === undefined) continue;
      const line = src.slice(0, m.index).split('\n').length;
      out.push({ line, text: m[0], value: hit });
    }
    return out;
  }

  const files = sourceFiles(ROOT).filter((p) => rel(p) !== HOME);
  const findings = files.flatMap((p) =>
    guardedSpellings(readFileSync(p, 'utf8')).map((s) => ({ file: rel(p), ...s })),
  );

  it('every launcher reads the windows and the baseline from analysisDefaults', () => {
    const offenders = findings
      .filter((f) => !(f.file in OWN_VOCABULARY))
      .map((f) => `${f.file}:${f.line}  ${f.text}  (= ${f.value} s)`);
    expect(
      offenders,
      `these files spell an analysis window or the baseline themselves, which is how the re-run ` +
        `fallback drifted to 24 hours (ADR-184). Import ANALYSIS_WINDOWS / DEFAULT_WINDOW_SECS / ` +
        `BASELINE_SECS from troubleshoot/analysisDefaults.ts instead:\n  ${offenders.join('\n  ')}`,
    ).toEqual([]);
  });

  it('keeps an exemption only while its file still spells one of the numbers', () => {
    for (const [file, reason] of Object.entries(OWN_VOCABULARY)) {
      expect(reason.length, `${file}: an exemption needs its reason`).toBeGreaterThan(40);
      expect(
        findings.some((f) => f.file === file),
        `${file} no longer spells a window or the baseline; drop its entry`,
      ).toBe(true);
    }
  });

  it('recognises the spellings it is meant to catch, and only those', () => {
    // The detector proven on one sample per shape, positive and negative, so "found nothing"
    // above can be told apart from "looked at nothing". The sample is built from the list.
    const [first] = ANALYSIS_WINDOWS;
    const grouped = first.secs.toLocaleString('en-US').split(',').join('_');
    const hours = first.secs / 3600;
    const sample = [
      `const a = ${first.secs};`, // plain
      `const b = ${grouped};`, // underscore-grouped
      `const c = '${first.secs}';`, // as a string, the drawer's old shape
      `const d = ${hours} * 3600;`, // as a product, the re-run fallback's old shape
      `const e = ${hours} * 60 * 60;`, // a longer product
      `const f = ${grouped}_000;`, // milliseconds: a different quantity
      `const g = 1${first.secs};`, // part of a longer number
      `const h = d${hours};`, // part of a name
      `const i = 3.${first.secs};`, // a decimal's fraction
      `const j = ${first.secs / 24};`, // an hour: not guarded
    ].join('\n');
    const found = guardedSpellings(sample);
    expect(found.map((s) => s.line)).toEqual([1, 2, 3, 4, 5]);
    expect(found.every((s) => s.value === first.secs)).toBe(true);
  });

  it('finds the sources it is supposed to be reading', () => {
    // Without this, a renamed directory would turn the check above into one that scans nothing
    // and passes — the failure mode that makes a guard worse than no guard. The floor is well
    // under the measured count (49 files at v0.3.34) and counts something the work does not shrink.
    expect(files.length).toBeGreaterThan(30);
    for (const must of ['LaunchDrawer.tsx', 'ScheduleModal.tsx', 'scheduleForm.ts', 'format.ts']) {
      expect(files.map(rel), must).toContain(must);
    }
    // The accept side: the launchers do read the shared module, so the ban above is not satisfied
    // by a screen that stopped offering a window at all.
    const home = `'./${HOME.slice(0, -'.ts'.length)}'`;
    const readers = files.filter((p) => readFileSync(p, 'utf8').includes(home));
    expect(readers.length).toBeGreaterThanOrEqual(5);
  });
});
