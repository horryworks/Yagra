// Breadcrumb for drill-downs (§4: sidebar = fixed hierarchy, deep navigation = breadcrumb,
// e.g. `Nodes / All nodes / <node>`). Pure presentation; the caller supplies the trail.

import { Link } from 'react-router-dom';
import { useTranslation } from 'react-i18next';
import './Breadcrumb.css';

export interface Crumb {
  label: string;
  /** Omit on the final (current) crumb. */
  to?: string;
}

export function Breadcrumb({ trail }: { trail: Crumb[] }) {
  const { t } = useTranslation('nav');
  return (
    <nav className="breadcrumb" aria-label={t('shell.breadcrumb')}>
      {trail.map((c, i) => {
        const last = i === trail.length - 1;
        return (
          <span key={`${c.label}-${i}`} className="breadcrumb-seg">
            {c.to && !last ? (
              <Link className="breadcrumb-link" to={c.to}>
                {c.label}
              </Link>
            ) : (
              <span className={last ? 'breadcrumb-current' : 'breadcrumb-link'}>{c.label}</span>
            )}
            {!last && <span className="breadcrumb-sep">/</span>}
          </span>
        );
      })}
    </nav>
  );
}
