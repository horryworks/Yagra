// SPDX-License-Identifier: AGPL-3.0-only
// The frames inside a section (ADR-246 decision 2): its heading, a callout, a part that folds,
// and a list of configured things that is not a table.
//
// Each replaces a family of near-copies measured across the screens - 45 rules for the small
// heading alone, in five letter-spacings and three weights. There is one heading look, and it is
// the `Card` title's: `.card-title` and `.ui-section-title` are one rule in `Section.css`.

import type { ReactNode } from 'react';
import { cx, isShown } from './space';
import './Section.css';

/** A heading inside a section (or above a table in one), with actions on the right. */
export function SectionHead({
  title,
  actions,
  level = 3,
}: {
  title: ReactNode;
  actions?: ReactNode;
  /** 2 when the heading sits directly under the page header rather than inside a Card. */
  level?: 2 | 3;
}) {
  const H = level === 2 ? 'h2' : 'h3';
  return (
    <div className="ui-section-head">
      <H className="ui-section-title">{title}</H>
      {isShown(actions) && <div className="ui-section-actions">{actions}</div>}
    </div>
  );
}

/** A band of text the operator should read before going on - not a refusal (that is an error
 *  line) and not an explanation of the screen (ADR-200 keeps those off it).
 *  `live` announces it when it appears (a load that just failed). A band that is part of the
 *  screen from the start - a standing warning - is not live, or a screen reader would read it out
 *  every time the screen opens. */
export function Callout({
  tone = 'info',
  live,
  children,
}: {
  tone?: 'info' | 'warning' | 'critical';
  live?: boolean;
  children: ReactNode;
}) {
  return (
    <div className={cx('ui-callout', `is-${tone}`)} role={live ? 'alert' : undefined}>
      {children}
    </div>
  );
}

/** A part of a section that folds. The closed row says what is set (ADR-240 decision 3, ADR-244
 *  decision 3), so it reads as a summary rather than as a heading with nothing under it.
 *  Uncontrolled with `defaultOpen`; controlled with `open` + `onToggle` (a screen that opens it
 *  itself to show an error). */
export function Disclosure({
  summary,
  defaultOpen,
  open,
  onToggle,
  className,
  children,
}: {
  summary: ReactNode;
  defaultOpen?: boolean;
  open?: boolean;
  onToggle?: (open: boolean) => void;
  className?: string;
  children: ReactNode;
}) {
  return (
    <details
      className={cx('ui-disclosure', className)}
      open={open ?? defaultOpen}
      onToggle={(e) => onToggle?.(e.currentTarget.open)}
    >
      <summary className="ui-disclosure-summary">{summary}</summary>
      <div className="ui-disclosure-body">{children}</div>
    </details>
  );
}

/** Things an operator has set up, one per row, when they are too few and too varied for a
 *  `DataTable` (an organization, a provider, a role). */
export function ItemList({ children }: { children: ReactNode }) {
  return <ul className="ui-item-list">{children}</ul>;
}

export function ItemRow({
  title,
  detail,
  actions,
  children,
}: {
  title: ReactNode;
  /** What is true of the item now (its state, a count) - not a description of it (ADR-200). */
  detail?: ReactNode;
  actions?: ReactNode;
  children?: ReactNode;
}) {
  return (
    <li className="ui-item-row">
      <div className="ui-item-main">
        <div className="ui-item-title">{title}</div>
        {isShown(detail) && <div className="ui-item-detail">{detail}</div>}
        {children}
      </div>
      {isShown(actions) && <div className="ui-item-actions">{actions}</div>}
    </li>
  );
}
