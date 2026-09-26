// SPDX-License-Identifier: AGPL-3.0-only
// A page's tab bar: one underlined row of buttons, each with an optional count chip (ADR-179 決定 9).
//
// Lifted out of Reports, which had it as page CSS, when Discovery needed the same bar — one look for
// "this page has several views", not two that drift. The selection itself is the caller's, and on a
// page it belongs in the URL (`useEnumParam('tab', …)`), so a reload or a shared link opens the same
// view.

import './Tabs.css';

export interface TabSpec<K extends string> {
  key: K;
  label: string;
  /** Drawn as a chip after the label; omitted when undefined. */
  count?: number;
}

export function Tabs<K extends string>({
  tabs,
  active,
  onChange,
}: {
  tabs: readonly TabSpec<K>[];
  active: K;
  onChange: (key: K) => void;
}) {
  return (
    <div className="ui-tabs" role="tablist">
      {tabs.map((tb) => (
        <button
          key={tb.key}
          type="button"
          role="tab"
          aria-selected={active === tb.key}
          className={active === tb.key ? 'ui-tab active' : 'ui-tab'}
          onClick={() => onChange(tb.key)}
        >
          {tb.label}
          {tb.count != null && <span className="ui-tab-count">{tb.count}</span>}
        </button>
      ))}
    </div>
  );
}
