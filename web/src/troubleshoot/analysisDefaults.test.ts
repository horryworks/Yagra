// SPDX-License-Identifier: AGPL-3.0-only
import { describe, expect, it } from 'vitest';
import i18n from '../i18n';
import { defaultAnalysisInput, DEFAULT_BASELINE_SECS, DEFAULT_SIGMA } from './analysisDefaults';

// The builder takes the caller's `t`; the global instance (English bundled, default lng) is
// already a standalone bound translator, so pass it straight through for the pure-function test.
const t = i18n.t;

describe('defaultAnalysisInput', () => {
  it('targets all nodes with the standard defaults', () => {
    const input = defaultAnalysisInput('anomaly', t);
    expect(input.tool).toBe('anomaly');
    expect(input.scope_kind).toBe('all');
    expect(input.scope_id).toBeNull();
    expect(input.depth).toBe('standard');
    expect(input.window_secs).toBe(7 * 86_400);
    expect(input.baseline_secs).toBe(DEFAULT_BASELINE_SECS);
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
