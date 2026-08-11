// SPDX-License-Identifier: AGPL-3.0-only
// A determinate or indeterminate progress bar.
//
// `web/src/components/ui/` had no progress primitive at all, so every long-running job on the
// Upgrade page had nothing to render with. The shapes already existed — `.ts-pbar` and friends in
// `troubleshoot.css` — but as page-local CSS, and copying them a second time is how a rule ends up
// maintained in N-1 places (extensibility.md §3). This is that shape promoted; Troubleshoot still
// uses its own and can move here later without changing what it looks like.
//
// **Indeterminate is a first-class state, not a fallback.** `value === null` means "something is
// happening and its position is not known" — an upgrade whose phase this build does not recognise,
// or one that has been requested and not yet picked up. Rendering that as a bar pinned at zero
// would be a claim that nothing has happened, which is a different and false statement.

import './ProgressBar.css';

interface Props {
  /** 0…1, or `null` for indeterminate. Values outside the range are clamped. */
  value: number | null;
  /** What the bar is measuring, for assistive technology. */
  label: string;
}

export function ProgressBar({ value, label }: Props) {
  const pct = value === null ? null : Math.round(Math.min(1, Math.max(0, value)) * 100);
  return (
    <div
      className="progressbar"
      role="progressbar"
      aria-label={label}
      aria-valuemin={0}
      aria-valuemax={100}
      // Omitted entirely when indeterminate: ARIA reads a missing `aria-valuenow` as "unknown",
      // which is the thing we mean. Passing 0 would announce "0 percent".
      aria-valuenow={pct ?? undefined}
    >
      <div
        className={pct === null ? 'progressbar-fill progressbar-fill-idle' : 'progressbar-fill'}
        style={pct === null ? undefined : { width: `${pct}%` }}
      />
    </div>
  );
}
