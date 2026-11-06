// SPDX-License-Identifier: AGPL-3.0-only
// An on/off switch: a checkbox with `role="switch"`, drawn as a track. Used where turning a thing
// on or off is the whole action (a node's config backup, ADR-240; the backup list's rows), so the
// state and the control are one object rather than a label beside two buttons.

import './Switch.css';

interface Props {
  checked: boolean;
  onChange: (next: boolean) => void;
  /** The accessible name. With `label` it is also what the reader sees beside the track. */
  ariaLabel: string;
  /** Text drawn before the track; leave it out inside a table cell, where the column names it. */
  label?: string;
  disabled?: boolean;
}

export function Switch({ checked, onChange, ariaLabel, label, disabled }: Props) {
  return (
    <label className="ysw">
      {label && <span className="ysw-label">{label}</span>}
      <input
        type="checkbox"
        role="switch"
        checked={checked}
        disabled={disabled}
        aria-label={ariaLabel}
        onChange={() => onChange(!checked)}
      />
      <span className="ysw-track" aria-hidden="true" />
    </label>
  );
}
