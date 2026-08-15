// SPDX-License-Identifier: AGPL-3.0-only
import { readdirSync, readFileSync, statSync } from 'node:fs';
import { join } from 'node:path';
import { describe, expect, it } from 'vitest';
import { ApiError } from '../services/api';
import { classifyLoadError, LOAD_BLOCKS } from './loadState';

describe('classifyLoadError', () => {
  it('names skeleton mode', () => {
    expect(classifyLoadError(new ApiError('admin_unavailable', 'no admin state', 503))).toBe(
      'unavailable',
    );
  });

  it('names a permission refusal, by status and by code', () => {
    // The guard extractors return `ApiError::forbidden()` — code `forbidden`, status 403.
    expect(classifyLoadError(new ApiError('forbidden', 'your role does not permit this', 403))).toBe(
      'forbidden',
    );
    // `ApiError::forbidden_code` keeps the status and changes the code. Still a refusal.
    expect(classifyLoadError(new ApiError('session_required', 'tokens cannot use this', 403))).toBe(
      'forbidden',
    );
  });

  it('is silent on 401, because the app handles it globally', () => {
    // The regression this pins: a page that drew "you lack permission" on the way to the sign-in
    // screen. 401 is "who are you", not "you may not".
    expect(classifyLoadError(new ApiError('unauthorized', 'sign in', 401))).toBeNull();
  });

  it('is silent on the errors a screen should not editorialize about', () => {
    expect(classifyLoadError(new ApiError('internal', 'boom', 500))).toBeNull();
    expect(classifyLoadError(new ApiError('not_found', 'gone', 404))).toBeNull();
    expect(classifyLoadError(new TypeError('network down'))).toBeNull();
    expect(classifyLoadError('a string nobody should throw')).toBeNull();
    expect(classifyLoadError(undefined)).toBeNull();
  });

  it('returns only declared blocks', () => {
    const seen = [
      classifyLoadError(new ApiError('admin_unavailable', '', 503)),
      classifyLoadError(new ApiError('forbidden', '', 403)),
    ];
    for (const s of seen) expect(LOAD_BLOCKS).toContain(s);
  });
});

/**
 * ADR-056's guard. Fourteen pages held a byte-identical `.catch` that handled `admin_unavailable`
 * and dropped the `403`, so a Viewer was told "No credentials yet" about two credentials. The
 * classifier above removes the duplication; this stops the next page from reintroducing it.
 *
 * ⚠️ **The needle is assembled at runtime.** A literal would match this file's own source and the
 * test would fail forever — the trap `reports.rs::the_run_state_sql_is_built_from_the_enum` records
 * on the Rust side.
 */
describe('no screen classifies a load failure by hand', () => {
  const SRC = join(__dirname, '..');
  const NEEDLE = `'${'admin'}_${'unavailable'}'`;

  function tsxFiles(dir: string): string[] {
    return readdirSync(dir).flatMap((e) => {
      const p = join(dir, e);
      if (statSync(p).isDirectory()) return tsxFiles(p);
      return p.endsWith('.tsx') ? [p] : [];
    });
  }

  it('every screen that inspects the skeleton-mode code goes through classifyLoadError', () => {
    const offenders = tsxFiles(SRC)
      .map((p) => [p, readFileSync(p, 'utf8')] as const)
      .filter(([, src]) => src.includes(NEEDLE))
      .filter(([, src]) => !src.includes('classifyLoadError'))
      .map(([p]) => p.slice(SRC.length + 1).replace(/\\/g, '/'));

    expect(
      offenders,
      `these screens decide for themselves what a failed load means, so a 403 lands as an empty ` +
        `list (ADR-056). Use classifyLoadError + LoadBlockNotice instead:\n  ${offenders.join('\n  ')}`,
    ).toEqual([]);
  });

  it('finds the sources it is supposed to be reading', () => {
    // Without this, a broken path or a renamed directory turns the check above into a test that
    // scans nothing and passes — the failure mode that makes a guard worse than no guard.
    const all = tsxFiles(SRC);
    expect(all.length).toBeGreaterThan(100);
    expect(all.filter((p) => readFileSync(p, 'utf8').includes('classifyLoadError')).length)
      .toBeGreaterThan(10);
  });
});
