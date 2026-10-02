// SPDX-License-Identifier: AGPL-3.0-only
import { describe, expect, it } from 'vitest';
import {
  insertList,
  isTemplateVariable,
  TEMPLATE_VARIABLE_GROUP_OF,
  TEMPLATE_VARIABLE_GROUPS,
  TEMPLATE_VARIABLE_NAMES,
} from './templateVariables';

const label = (n: string) => `label-${n}`;
const desc = (n: string) => (n === 'threshold' ? 'しきい値' : `desc-${n}`);

describe('the insert list', () => {
  it('lists every variable once, under its group, in the declared order', () => {
    const all = insertList('', label, desc);
    expect(all.map((g) => g.group)).toEqual([...TEMPLATE_VARIABLE_GROUPS]);
    const flat = all.flatMap((g) => g.names);
    expect(new Set(flat).size).toBe(TEMPLATE_VARIABLE_NAMES.length);
    for (const g of all) for (const n of g.names) expect(TEMPLATE_VARIABLE_GROUP_OF[n]).toBe(g.group);
  });

  it('matches the name, the label and the explanation, and drops empty groups', () => {
    expect(insertList('dedup', label, desc).flatMap((g) => g.names)).toEqual(['dedup_key']);
    expect(insertList('label-node_address', label, desc).flatMap((g) => g.names)).toEqual(['node_address']);
    const byJapanese = insertList('しきい', label, desc);
    expect(byJapanese).toEqual([{ group: 'numbers', names: ['threshold'] }]);
    expect(insertList('zzz', label, desc)).toEqual([]);
  });

  it('knows its own names and nothing else', () => {
    expect(isTemplateVariable('node_name')).toBe(true);
    expect(isTemplateVariable('password')).toBe(false);
  });
});
