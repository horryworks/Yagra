// SPDX-License-Identifier: AGPL-3.0-only
// The judgement behind Nodes ▸ Duplicates (ADR-148), in a `.ts` so a test can reach it — Vitest never
// runs a `.tsx` (`testing.md`). `DuplicateNodesPage.tsx` is layout plus the calls.
//
// What is decided here: how the server's groups become table rows, which selections a delete may
// send, what the delete names, and what an empty table says. What is NOT decided here is which nodes
// are duplicates — that is the server's (`crates/yagra-core/src/duplicates.rs`), and nothing below
// second-guesses it.

import type {
  DuplicateEvidenceKind,
  DuplicateGroup,
  DuplicateIgnoredValue,
  DuplicateMember,
  DuplicateNodesView,
} from '../types/api';

/** The most nodes one delete may name. The server refuses more rather than cutting the list
 *  (`NODE_MOVE_BATCH_MAX` in `api/nodes.rs`), so the screen says so before it is pressed. */
export const DELETE_MAX = 1000;

/** One table row: a member of a group. `DataTable` has no heading row, so the group travels with
 *  each of its members and only the first one shows it. */
export interface DuplicateRow {
  /** 1-based, in the order the server listed the groups. */
  groupNumber: number;
  group: DuplicateGroup;
  member: DuplicateMember;
  /** The first row of its group — the one that shows the group's confidence and evidence. */
  first: boolean;
  /** Alternates per group, so two neighbouring groups can be told apart. */
  shade: 'even' | 'odd';
}

/** Every member on its own row, groups in the server's order. */
export function flattenRows(view: DuplicateNodesView | null): DuplicateRow[] {
  if (!view) return [];
  return view.groups.flatMap((group, g) =>
    group.members.map(
      (member, m): DuplicateRow => ({
        groupNumber: g + 1,
        group,
        member,
        first: m === 0,
        shade: g % 2 === 0 ? 'even' : 'odd',
      }),
    ),
  );
}

/** How many of the listed groups are likely duplicates and how many are to be checked. */
export function confidenceCounts(view: DuplicateNodesView | null): {
  confident: number;
  possible: number;
} {
  const groups = view?.groups ?? [];
  const confident = groups.filter((g) => g.confidence === 'confident').length;
  return { confident, possible: groups.length - confident };
}

/** Every member except each group's suggested keeper — the shortcut an operator reaches for when
 *  they trust the suggestion. Nothing is selected until they press it. */
export function selectAllButKeepers(view: DuplicateNodesView | null): ReadonlySet<string> {
  const out = new Set<string>();
  for (const group of view?.groups ?? []) {
    for (const m of group.members) if (!m.suggested_keep) out.add(m.node_id);
  }
  return out;
}

/** The selection after a reload: only nodes still listed stay selected.
 *
 *  A node that left the list — deleted, or no longer looking like a duplicate — must not stay selected
 *  where nobody can see it, or the next delete would take a node the operator can no longer inspect. */
export function pruneSelection(
  selected: ReadonlySet<string>,
  view: DuplicateNodesView | null,
): ReadonlySet<string> {
  const listed = new Set(flattenRows(view).map((r) => r.member.node_id));
  return new Set([...selected].filter((id) => listed.has(id)));
}

/** The groups (1-based numbers) whose every member is selected: deleting them would remove the device
 *  from monitoring altogether, which is never what cleaning up a duplicate means. */
export function groupsLeftEmpty(
  view: DuplicateNodesView | null,
  selected: ReadonlySet<string>,
): number[] {
  return (view?.groups ?? []).flatMap((group, i) =>
    group.members.length > 0 && group.members.every((m) => selected.has(m.node_id)) ? [i + 1] : [],
  );
}

export type DeleteBlock =
  | { key: 'duplicates.block.keepOne'; groups: number[] }
  | { key: 'duplicates.block.tooMany'; max: number };

/** Why the delete may not be pressed, or `null` when it may. An empty selection is not a block —
 *  the button is simply not ready, and says nothing. */
export function deleteBlock(
  view: DuplicateNodesView | null,
  selected: ReadonlySet<string>,
  max = DELETE_MAX,
): DeleteBlock | null {
  const empty = groupsLeftEmpty(view, selected);
  if (empty.length > 0) return { key: 'duplicates.block.keepOne', groups: empty };
  if (selected.size > max) return { key: 'duplicates.block.tooMany', max };
  return null;
}

/** What the delete sends and names, in the order the table lists them. Only listed nodes: a selection
 *  that outlived its row cannot reach the server. */
export function deleteTargets(
  view: DuplicateNodesView | null,
  selected: ReadonlySet<string>,
): { id: string; name: string }[] {
  return flattenRows(view)
    .filter((r) => selected.has(r.member.node_id))
    .map((r) => ({ id: r.member.node_id, name: r.member.node_name }));
}

/** The group's evidence as one line, strongest first (the server's order). */
export function evidenceSummary(
  group: DuplicateGroup,
  kindLabel: (kind: DuplicateEvidenceKind) => string,
): string {
  return group.evidence.map((e) => `${kindLabel(e.kind)}: ${e.value}`).join(' · ');
}

/** The ignored values worth naming inline, and how many more there are.
 *
 *  `more` counts from the server's `ignored_total`, not from the list it sent — the list is capped, and
 *  a count read off it would say "none left" about values it never carried. */
export function ignoredShown(
  view: DuplicateNodesView | null,
  max = 5,
): { shown: DuplicateIgnoredValue[]; more: number } {
  const shown = (view?.ignored ?? []).slice(0, max);
  return { shown, more: Math.max(0, (view?.ignored_total ?? 0) - shown.length) };
}

/** What the empty table says, and the number it says it with.
 *
 *  An empty list means three different things. Fewer than two nodes: there was nothing to compare.
 *  No node with a serial number or an address list: only the address and the name were compared, and
 *  "no duplicates" would read as an all-clear about evidence nobody has collected yet. Otherwise: the
 *  fleet was compared, and nothing matched. */
export function emptyState(
  view: Pick<DuplicateNodesView, 'scanned' | 'with_serial' | 'with_address_list'> | null,
): {
  key: 'duplicates.empty' | 'duplicates.emptyNothingToCompare' | 'duplicates.emptyNoEvidence';
  count: number;
} {
  if (view && view.scanned < 2) return { key: 'duplicates.emptyNothingToCompare', count: view.scanned };
  if (view && view.with_serial === 0 && view.with_address_list === 0) {
    return { key: 'duplicates.emptyNoEvidence', count: view.scanned };
  }
  return { key: 'duplicates.empty', count: view?.scanned ?? 0 };
}
