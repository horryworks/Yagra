// SPDX-License-Identifier: AGPL-3.0-only
// The judgement behind Nodes > Tags (ADR-263), kept out of `TagsPage.tsx` so Vitest can reach it.

import { labelProblem, normalizeLabel, type LabelProblem } from '../components/ui/labelRules';
import type { NodeTag } from '../types/api';

/** Why a rename cannot be sent. `same` is the one the label rules do not know. */
export const RENAME_PROBLEMS = ['same'] as const;
export type RenameProblem = LabelProblem | (typeof RENAME_PROBLEMS)[number];

/** What the rename dialog knows before it sends: why it cannot, or which tag it merges into. */
export interface RenamePlan {
  problem: RenameProblem | null;
  /** The existing tag the new name already belongs to: a rename onto it merges the two. */
  mergeInto: NodeTag | null;
}

/**
 * Judge a rename from `from` to the typed `raw`, against the listed tags.
 *
 * The name is checked with the label rules every tag editor uses (`labelProblem`, which mirrors
 * `api/nodes.rs::validated_labels`), against no list: a rename never adds a label to a row, so
 * the per-row cap cannot be crossed. Case counts, as it does where tags are stored, so `core` to
 * `Core` is a rename, not `same`.
 */
export function planRename(from: string, raw: string, tags: readonly NodeTag[]): RenamePlan {
  const to = normalizeLabel(raw);
  const problem: RenameProblem | null = to === from ? 'same' : labelProblem(to, []);
  const mergeInto = problem === null ? (tags.find((t) => t.tag === to) ?? null) : null;
  return { problem, mergeInto };
}

/** Every place a tag is named, added up: what a rename or a delete will touch. */
export function placesNamed(t: NodeTag): number {
  return t.own + t.folders + t.refusals + t.rules + t.exceptions;
}
