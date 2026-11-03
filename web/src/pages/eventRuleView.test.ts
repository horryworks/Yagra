// SPDX-License-Identifier: AGPL-3.0-only
import { describe, expect, it } from 'vitest';
import {
  eventRuleViewFrom,
  eventTargetOf,
  inEventRuleView,
  targetFields,
  writeEventRuleView,
} from './eventRuleView';

const NODE = '11111111-1111-4111-8111-111111111111';
const FOLDER = '22222222-2222-4222-8222-222222222222';
const OTHER = '33333333-3333-4333-8333-333333333333';

const every = { node_id: null, group_id: null };
const onFolder = { node_id: null, group_id: FOLDER };
const onNode = { node_id: NODE, group_id: null };

describe('event rule targets (ADR-232)', () => {
  it('names what a rule targets', () => {
    expect(eventTargetOf(every)).toBe('global');
    expect(eventTargetOf(onFolder)).toBe('folder');
    expect(eventTargetOf(onNode)).toBe('node');
  });

  it('a tab shows its kind of target, and the chosen one when there is one', () => {
    const all = [every, onFolder, onNode];
    const shown = (tab: string, target: string | null = null) =>
      all.filter((r) => inEventRuleView(r, eventRuleViewFrom(new URLSearchParams(
        target ? { target_level: tab, target } : { target_level: tab },
      ))));
    expect(shown('all')).toEqual(all);
    expect(shown('global')).toEqual([every]);
    expect(shown('folder')).toEqual([onFolder]);
    expect(shown('folder', FOLDER)).toEqual([onFolder]);
    expect(shown('folder', OTHER)).toEqual([]);
    expect(shown('node', NODE)).toEqual([onNode]);
  });

  it('reads an unknown tab or a malformed target as no narrowing, and writes a short URL', () => {
    expect(eventRuleViewFrom(new URLSearchParams('target_level=port'))).toEqual({
      tab: 'all',
      target: null,
    });
    expect(eventRuleViewFrom(new URLSearchParams('target_level=node&target=x'))).toEqual({
      tab: 'node',
      target: null,
    });
    // A target on a tab that names nothing is dropped.
    expect(eventRuleViewFrom(new URLSearchParams(`target_level=global&target=${NODE}`)).target)
      .toBeNull();
    const params = new URLSearchParams('q=link&target_level=node&target=' + NODE);
    writeEventRuleView(params, { tab: 'all', target: NODE });
    expect(params.toString()).toBe('q=link');
  });

  it('a request names one target at most, and never an empty one', () => {
    expect(targetFields('global', NODE)).toEqual({ node_id: null, group_id: null });
    expect(targetFields('folder', FOLDER)).toEqual({ node_id: null, group_id: FOLDER });
    expect(targetFields('node', NODE)).toEqual({ node_id: NODE, group_id: null });
    expect(targetFields('node', '')).toEqual({ node_id: null, group_id: null });
  });
});
