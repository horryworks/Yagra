// SPDX-License-Identifier: AGPL-3.0-only
import { describe, expect, it } from 'vitest';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { rowActionsWidth } from './rowActions';

const read = (rel: string) => readFileSync(fileURLToPath(new URL(rel, import.meta.url)), 'utf8');

describe('rowActionsWidth', () => {
  it('fits the buttons, their gaps and the cell padding', () => {
    expect(rowActionsWidth(1)).toBe('52px');
    expect(rowActionsWidth(2)).toBe('84px');
    expect(rowActionsWidth(3)).toBe('116px');
    expect(rowActionsWidth(4)).toBe('148px');
  });

  it('never answers narrower than one button', () => {
    expect(rowActionsWidth(0)).toBe('52px');
  });

  // The three numbers are copies of CSS. If one moves, the column is wrong again and nothing but
  // a browser would notice — so read the CSS and fail here instead.
  it('agrees with the CSS it copies', () => {
    expect(read('../components/ui/IconButton.css')).toMatch(/\.icon-btn \{\s*width: 28px;/);
    // 4px, spelled as its step of the scale since ADR-246 (`components/ui/space.test.ts` pins the
    // token's value), so a change of step fails here just as a change of number did.
    expect(read('../styles/table.css')).toMatch(/\.ytable-actions \{[^}]*gap: var\(--space-1\);/);
    // 12px, as its step (`--space-3`) since ADR-246 Inc.8.
    expect(read('../components/ui/DataTable.css')).toMatch(/\.dt-cell \{[^}]*padding: 0 var\(--space-3\);/);
  });
});
