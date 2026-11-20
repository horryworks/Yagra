// SPDX-License-Identifier: AGPL-3.0-only
// What the routing rule's tag field picks from (ADR-259 Inc.2), kept out of `RoutingPage.tsx` so
// Vitest can reach it.

import type { NodeTags } from '../types/api';
import type { ChipOption } from '../components/ui/labelRules';

/** The tags nodes carry as the field's options, each with its node count as the detail. */
export function tagOptions(tags: NodeTags, nodes: (count: number) => string): ChipOption[] {
  return tags.tags.map((t) => ({ value: t.tag, detail: nodes(t.nodes) }));
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
  const carried = new Set(tags.tags.map((t) => t.tag));
  return new Set(ruleTags.filter((t) => !carried.has(t)));
}
