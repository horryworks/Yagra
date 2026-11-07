// SPDX-License-Identifier: AGPL-3.0-only
// The body of a screen and the two boxes everything else is laid out in (ADR-246 decision 2).
//
// `Page` decides how a screen stacks what it holds - the page header, a toolbar, the sections -
// with one gap, so no screen spaces its own cards (five settings screens had cards touching, and
// the ones that did space them used 12, 16 and 24px). `Stack` and `Inline` take a step of the
// scale by its size in px; they replace the per-screen `display: flex; gap: 6px` boxes.
//
// Which layout a screen takes is in `ui-conventions.md`, in its ADR-246 section:
//   list      PageHeader + ListToolbar + DataTable; the table fills the height
//   sections  PageHeader + Cards stacked 16px apart (settings, forms, read-only status)
//   canvas    PageHeader + one surface that fills the height (a map, a dashboard)

import type { ElementType, ReactNode } from 'react';
import { cx, spaceVar, type Space } from './space';
import './Page.css';

export type PageLayout = 'list' | 'sections' | 'canvas';

export function Page({
  layout,
  className,
  children,
}: {
  layout: PageLayout;
  className?: string;
  children: ReactNode;
}) {
  return <div className={cx('ui-page', `is-${layout}`, className)}>{children}</div>;
}

interface BoxProps {
  /** The step between children, by its size in px. */
  gap?: Space;
  /** The element to render - a `ul`, a `section` - when a `div` would say the wrong thing. */
  as?: ElementType;
  className?: string;
  children: ReactNode;
}

/** Children one above the other. `fill` takes the height left in a list or canvas `Page`, for the
 *  box a `DataTable` sits in when it is not the page's own child (a tab's body), so the table
 *  still scrolls inside itself rather than the page. */
export function Stack({
  gap = 12,
  as: Tag = 'div',
  fill,
  className,
  children,
}: BoxProps & { fill?: boolean }) {
  return (
    <Tag className={cx('ui-stack', fill && 'is-fill', className)} style={{ gap: spaceVar(gap) }}>
      {children}
    </Tag>
  );
}

/** Children side by side, wrapping when they do not fit. `stackOnPhone` puts them one above the
 *  other, full width, on a phone - a row of unlabelled inputs and their buttons, which wrapped
 *  two to a line would read as two forms. */
export function Inline({
  gap = 8,
  as: Tag = 'div',
  justify = 'start',
  align = 'center',
  stackOnPhone,
  className,
  children,
}: BoxProps & {
  justify?: 'start' | 'end' | 'between';
  align?: 'center' | 'start' | 'baseline' | 'end';
  stackOnPhone?: boolean;
}) {
  return (
    <Tag
      className={cx(
        'ui-inline',
        `justify-${justify}`,
        `align-${align}`,
        stackOnPhone && 'is-phone-stack',
        className,
      )}
      style={{ gap: spaceVar(gap) }}
    >
      {children}
    </Tag>
  );
}
