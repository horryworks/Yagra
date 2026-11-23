// SPDX-License-Identifier: AGPL-3.0-only
// What the routing rule's tag list offers (ADR-259 Inc.2), kept out of `RoutingPage.tsx` so Vitest
// can reach it.

import type { NodeTags } from '../types/api';

/** One row of the rule dialog's tag list. `nodes` is null when the count is unknown. */
export interface TagChoice {
  tag: string;
  nodes: number | null;
  /** The rule names it and no node carries it: it matches nothing. */
  gone: boolean;
}

/**
 * The rows the tag list shows: a tag the rule names that no node carries first (so it is seen and
 * can be unticked), then every carried tag in the order the server sorted them. When the carried
 * tags could not be read, the rule's own tags are still shown, unmarked, so a save keeps them.
 */
export function tagChoices(ruleTags: readonly string[], tags: NodeTags | null): TagChoice[] {
  if (tags === null) return ruleTags.map((tag) => ({ tag, nodes: null, gone: false }));
  const gone = tagsNoNodeCarries(ruleTags, tags) ?? new Set<string>();
  return [
    ...ruleTags.filter((t) => gone.has(t)).map((tag) => ({ tag, nodes: 0, gone: true })),
    ...carriedTags(tags).map((t) => ({ tag: t.tag, nodes: t.nodes, gone: false })),
  ];
}

/**
 * Which of a rule's tags no node carries now — the rule matches nothing through them. `null` when
 * the tag list could not be read, so a failed read never marks every tag as gone.
 */
export function tagsNoNodeCarries(
  ruleTags: readonly string[],
  tags: NodeTags | null,
): Set<string> | null {
  if (tags === null) return null;
  const carried = new Set(carriedTags(tags).map((t) => t.tag));
  return new Set(ruleTags.filter((t) => !carried.has(t)));
}

/**
 * The tags at least one node carries. Since ADR-263 the list also holds tags only a rule, an
 * exception or a refusal names (`nodes` 0), so the Tags screen can show leftovers; a rule's picker
 * offers what a node carries, and a tag nobody carries is the `gone` case above.
 */
function carriedTags(tags: NodeTags): NodeTags['tags'] {
  return tags.tags.filter((t) => t.nodes > 0);
}
