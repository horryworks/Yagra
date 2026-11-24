// SPDX-License-Identifier: AGPL-3.0-only
// The judgement behind the Nodes tree's tag filter (ADR-265). In a `.ts` because Vitest never runs
// a `.tsx` (testing.md).
//
// A deployment may carry thousands of tags, so the picker never lists them all: with nothing typed
// it offers the few carried by the most nodes, and with text it offers the server's best matches.
// The selected tags are always shown, first and ticked, whether or not the answer holds them.

import { decodeSet } from '../lib/columnFilter';
import type { TagSuggestions } from '../types/api';

/** How many tags to offer with nothing typed (ADR-265 decision 4). */
export const TAG_IDLE_LIMIT = 8;
/** How many tags to offer for a typed term. */
export const TAG_TYPED_LIMIT = 20;

/** How many to ask the server for. The selected tags are asked for too, because they may be among
 *  the answer and are taken out of it, and the offer must still be the full count. */
export function tagRequestLimit(typed: string, selectedCount: number): number {
  const want = typed.trim() === '' ? TAG_IDLE_LIMIT : TAG_TYPED_LIMIT;
  return Math.min(50, want + selectedCount);
}

export interface TagPickerView {
  /** The selected tags first, then the offered ones, each once. */
  options: string[];
  /** Node counts for every offered and chosen tag the answer holds. */
  counts: Record<string, number>;
  /** How many matching tags were not offered. */
  more: number;
}

/** What the picker draws for `answer`, given the tags already chosen (the URL's comma set). */
export function tagPickerView(
  answer: TagSuggestions | null,
  chosen: string,
  typed: string,
): TagPickerView {
  const selected = decodeSet(chosen);
  const taken = new Set(selected);
  const counts: Record<string, number> = {};
  for (const t of [...(answer?.tags ?? []), ...(answer?.chosen ?? [])]) counts[t.tag] = t.nodes;
  const want = typed.trim() === '' ? TAG_IDLE_LIMIT : TAG_TYPED_LIMIT;
  const offered = (answer?.tags ?? []).map((t) => t.tag).filter((t) => !taken.has(t));
  const shown = offered.slice(0, want);
  return {
    options: [...selected, ...shown],
    counts,
    more: (answer?.more ?? 0) + (offered.length - shown.length),
  };
}

/** Add or remove one tag, keeping the order they were picked in. */
export function toggleTag(chosen: string, tag: string): string {
  const now = decodeSet(chosen);
  return (now.includes(tag) ? now.filter((t) => t !== tag) : [...now, tag]).join(',');
}
