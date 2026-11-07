// SPDX-License-Identifier: AGPL-3.0-only
// "Name: value" read-only facts (ADR-246 decision 2) - fifteen hand-built variants before it,
// in four shapes, now three layouts of one list:
//   rows    label beside value, labels in one column (a certificate, a remote, the About page)
//   grid    label over value, as many columns as fit (a node's overview, a host's metrics)
//   inline  pairs side by side on one wrapping line (a summary strip above a history)

import type { ReactNode } from 'react';
import { cx } from './space';
import './Facts.css';
// The grid layout's label is the one heading look, defined there.
import './Section.css';

export interface Fact {
  label: ReactNode;
  value: ReactNode;
  /** Needed when the label is not a string. */
  key?: string;
}

export function Facts({
  items,
  layout = 'rows',
  className,
}: {
  items: Fact[];
  layout?: 'rows' | 'grid' | 'inline';
  className?: string;
}) {
  return (
    <dl className={cx('ui-facts', `is-${layout}`, className)}>
      {items.map((f, i) => (
        <div className="ui-fact" key={f.key ?? (typeof f.label === 'string' ? f.label : i)}>
          <dt>{f.label}</dt>
          <dd>{f.value}</dd>
        </div>
      ))}
    </dl>
  );
}
