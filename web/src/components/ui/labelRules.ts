// The judgement behind `ChipInput` (ADR-135 inc. 2).
//
// In a `.ts` rather than the component, because Vitest runs with `include: ['src/**/*.test.ts']`
// and never loads a `.tsx` — a rule `src/tsxJudgement.test.ts` enforces. Everything here is pure,
// so the boundary rules that decide whether a label can be saved are testable without a browser.

/** The longest label the API accepts. Mirrors `api/nodes.rs::LABEL_MAX`. */
export const LABEL_MAX = 64;
/** The most labels one node or one folder may carry. Mirrors `api/nodes.rs::LABELS_MAX`. */
export const LABELS_MAX = 32;

/**
 * Why a label cannot be added.
 *
 * An `as const` array rather than a bare union because `ChipInput` builds its message key at
 * runtime (`` t(`field.tagErr.${problem}`) ``), and `i18nEnumKeys.test.ts` iterates arrays like
 * this to prove both locales carry every member. EN/JA parity alone cannot: a new member is
 * missing from both, so parity passes while the screen renders a raw key.
 *
 * 🚨 `duplicate` is deliberately **not** in here. Re-adding a label a chip row already has is a
 * silent no-op, not an error — the operator asked for a state that is already true.
 */
export const LABEL_PROBLEMS = ['empty', 'tooLong', 'control', 'tooMany'] as const;
export type LabelProblem = (typeof LABEL_PROBLEMS)[number];

/** Trim to the form the server stores. */
export function normalizeLabel(raw: string): string {
  return raw.trim();
}

/**
 * Split pasted text into candidate labels.
 *
 * Commas and newlines, because those are what a list copied out of a spreadsheet or another tool
 * is separated by. A label may contain spaces (`Matsuyama 本社`), so a space is **not** a
 * separator — splitting on it would silently turn one label into two.
 */
export function splitPastedLabels(raw: string): string[] {
  return raw
    .split(/[,\n\r]+/)
    .map(normalizeLabel)
    .filter((s) => s !== '');
}

/**
 * Whether `raw` can join `existing`, and why not.
 *
 * Mirrors `api/nodes.rs::validated_labels` so the operator is told before the save rather than by
 * a 400. The rules are deliberately loose on character: a label is a word a person reads off a
 * badge, so `JAPAN`, `松山本社` and `spare parts` are all legal. Only control characters are
 * refused, because they are invisible and would make two labels that look identical differ.
 */
export function labelProblem(raw: string, existing: readonly string[]): LabelProblem | null {
  const label = normalizeLabel(raw);
  if (label === '') return 'empty';
  // Code points, not UTF-16 units, matching the backend's `.chars().count()`.
  if ([...label].length > LABEL_MAX) return 'tooLong';
  // Compared by code point rather than with a character class. A regex literal holding real
  // control characters is invisible in a diff, and this file was written with two of them in it
  // (a NUL among them) on the first attempt -- tsc, Vitest and the build all pass such a file,
  // and grep saying "Binary file matches" was the only signal. All printable ASCII here.
  if ([...label].some((c) => c < ' ' || c === String.fromCharCode(127))) return 'control';
  if (!existing.includes(label) && existing.length >= LABELS_MAX) return 'tooMany';
  return null;
}

/**
 * `existing` plus `raw`, or `existing` unchanged when it cannot be added.
 *
 * Adding one that is already there returns the same list — see `LABEL_PROBLEMS`' note on why that
 * is not an error.
 */
export function addLabel(existing: readonly string[], raw: string): string[] {
  const label = normalizeLabel(raw);
  if (labelProblem(label, existing) !== null || existing.includes(label)) return [...existing];
  return [...existing, label];
}

/** `existing` without `label`. */
export function removeLabel(existing: readonly string[], label: string): string[] {
  return existing.filter((l) => l !== label);
}

/**
 * Whether a whole list can be saved.
 *
 * ⚠️ Checks each label **against the list without it**, so a list that is exactly at the cap does
 * not report every one of its members as `tooMany`.
 *
 * 🚨 This can be false for a list the operator never typed: migration 0109 converts labels from a
 * column whose values could be twice as long and never rejected a control character, so a node can
 * read back carrying one this refuses. That is why the offending chip is marked and removable
 * rather than the dialog being blocked with no way forward — and why a *removal* list is never
 * routed through these rules (`api/nodes.rs::normalized_removals` says the same thing server-side).
 */
export function labelsAreValid(list: readonly string[]): boolean {
  return (
    list.length <= LABELS_MAX &&
    list.every((l, i) => labelProblem(l, list.filter((_, j) => j !== i)) === null)
  );
}

/** Which labels in `list` cannot be saved, so the chip row can mark exactly those. */
export function labelProblems(list: readonly string[]): Map<string, LabelProblem> {
  const out = new Map<string, LabelProblem>();
  list.forEach((l, i) => {
    const problem = labelProblem(
      l,
      list.filter((_, j) => j !== i),
    );
    if (problem !== null) out.set(l, problem);
  });
  return out;
}
