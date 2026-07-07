// Table toolbar — one consistent control row above every list (search → filters → spacer →
// result count → primary action). The pieces are small composables so each screen arranges the
// slots it needs; styles live with the table standard (styles/table.css).

import type { ReactNode } from 'react';
import { Trans, useTranslation } from 'react-i18next';
import { SearchIcon } from './icons';

/** The toolbar row. Lay children left → right; drop a <TableSpacer/> before the count/action. */
export function TableToolbar({ children }: { children: ReactNode }) {
  return <div className="table-toolbar">{children}</div>;
}

/** Flexible gap that pushes the result count + primary action to the right. */
export function TableSpacer() {
  return <div className="table-spacer" />;
}

/** Search box with a leading magnifier. Controlled. */
export function SearchInput({
  value,
  onChange,
  placeholder,
  ariaLabel,
}: {
  value: string;
  onChange: (v: string) => void;
  placeholder?: string;
  ariaLabel?: string;
}) {
  const { t } = useTranslation('common');
  return (
    <div className="table-search">
      <SearchIcon />
      <input
        type="search"
        value={value}
        onChange={(e) => onChange(e.target.value)}
        placeholder={placeholder}
        aria-label={ariaLabel ?? t('actions.search')}
      />
    </div>
  );
}

/** Result count: "N of M <noun>" (omit `total` for "N <noun>"), with `shown` emphasized. The
 *  word order comes from the translation key so a language like Japanese can reorder it; callers
 *  pass `noun` already localized for their context. */
export function ResultCount({
  shown,
  total,
  noun,
}: {
  shown: number;
  total?: number;
  noun: string;
}) {
  const { t } = useTranslation('common');
  return (
    <span className="table-count">
      <Trans
        t={t}
        i18nKey={total != null ? 'resultCount' : 'resultCountNoTotal'}
        values={{ shown, total, noun }}
        components={{ b: <strong /> }}
      />
    </span>
  );
}
