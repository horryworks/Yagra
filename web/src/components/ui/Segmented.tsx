// SPDX-License-Identifier: AGPL-3.0-only
// Segmented control: equal-width buttons with 0.5px dividers; the selected button gets the
// tertiary fill + a 2px accent inset under-border. Used for the drawer's time-window / depth /
// notify choices, and (`size="sm"`) for a chart's unit and axis choices. Keyboard-operable (each
// option is a real button).

import './Segmented.css';

interface Option {
  value: string;
  label: string;
}

interface Props {
  options: Option[];
  value: string;
  onChange: (value: string) => void;
  ariaLabel?: string;
  /** `sm` sizes the control to a chart's title row rather than to a form field. */
  size?: 'sm';
}

export function Segmented({ options, value, onChange, ariaLabel, size }: Props) {
  return (
    <div
      className={size === 'sm' ? 'ts-seg ts-seg-sm' : 'ts-seg'}
      role="group"
      aria-label={ariaLabel}
    >
      {options.map((o) => (
        <button
          key={o.value}
          type="button"
          className={o.value === value ? 'on' : undefined}
          aria-pressed={o.value === value}
          onClick={() => onChange(o.value)}
        >
          {o.label}
        </button>
      ))}
    </div>
  );
}
