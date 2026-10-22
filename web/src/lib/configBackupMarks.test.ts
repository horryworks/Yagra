// SPDX-License-Identifier: AGPL-3.0-only
import { describe, expect, it } from 'vitest';
import type { ConfigBackupRow } from '../types/api';
import { markOf, marksOf } from './configBackupMarks';

const row = (over: Partial<ConfigBackupRow>): ConfigBackupRow => ({
  node_id: 'n1',
  credential_id: 'c1',
  login_confirmed: false,
  pending: false,
  try_after: null,
  last: null,
  last_success_at: null,
  ...over,
});

const at = '2026-10-22T10:00:00Z';

describe('config backup marks', () => {
  it('a node that is fetched with a working login is ok', () => {
    expect(
      markOf(
        row({
          login_confirmed: true,
          last: { at, credential_id: 'c1', outcome: 'success', new_version: false },
        }),
      ),
    ).toBe('ok');
  });

  it('a fetch that is out, or a node not tried yet, is in progress', () => {
    expect(markOf(row({ pending: true }))).toBe('busy');
    expect(markOf(row({}))).toBe('busy');
  });

  it('a failed attempt, or a login still not found, needs attention', () => {
    expect(
      markOf(
        row({
          login_confirmed: true,
          last: { at, credential_id: 'c1', outcome: 'auth_failed', new_version: null },
        }),
      ),
    ).toBe('trouble');
    expect(
      markOf(row({ last: { at, credential_id: 'c1', outcome: 'unreachable', new_version: null } })),
    ).toBe('trouble');
  });

  it('marks are keyed by node', () => {
    const marks = marksOf([row({ node_id: 'a' }), row({ node_id: 'b', pending: true })]);
    expect([...marks.keys()]).toEqual(['a', 'b']);
  });
});
