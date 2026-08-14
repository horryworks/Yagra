// SPDX-License-Identifier: AGPL-3.0-only
// Add-widget catalog: the registry grouped by section, each widget a clickable card that adds
// an instance to the board. Backing tags (live/rollup) carry over from the catalog so the
// operator knows what's data-backed. Stays open after an add so several can be placed at once.

import { useState } from 'react';
import { useTranslation } from 'react-i18next';
import { Badge } from '../components/ui/Badge';
import { Button } from '../components/ui/Button';
import { Modal } from '../components/ui/Modal';
import { useLayoutStoreContext } from './LayoutStoreContext';
import { SearchInput } from '../components/ui/SearchInput';
import { catalogBySection } from './registry';
import { countWidgets, filterCatalog } from './catalogFilter';
import type { Backing } from './types';
import './CatalogModal.css';

const BACKING_TONE: Record<Backing, 'up' | 'info' | 'neutral'> = {
  live: 'up',
  rollup: 'info',
  new: 'neutral',
};

export function CatalogModal({ onClose }: { onClose: () => void }) {
  const { t } = useTranslation('dashboard');
  const useStore = useLayoutStoreContext();
  const addWidget = useStore((s) => s.addWidget);
  const [q, setQ] = useState('');
  const all = catalogBySection();
  // Forty-six widgets across nine sections: until now the only way to find one was to read all of
  // them. Client-side and instant — the registry is already in the bundle, so there is nothing to
  // debounce. The search runs over the *rendered* words, see `catalogFilter.ts`.
  const sections = filterCatalog(all, q, t);
  const shown = countWidgets(sections);
  return (
    <Modal
      title={t('catalog.title')}
      onClose={onClose}
      footer={<Button onClick={onClose}>{t('actions.done')}</Button>}
    >
      <div className="catalog-search">
        <SearchInput
          value={q}
          onChange={setQ}
          placeholder={t('catalog.searchPlaceholder')}
          ariaLabel={t('catalog.searchAria')}
        />
        <span className="catalog-count muted">
          {t('catalog.count', { shown, total: countWidgets(all) })}
        </span>
      </div>
      <div className="catalog">
        {sections.length === 0 && <p className="muted">{t('common:filter.noMatch')}</p>}
        {sections.map(({ section, widgets }) => (
          <div className="catalog-section" key={section}>
            <h3 className="catalog-section-title">{t(section)}</h3>
            <div className="catalog-grid">
              {widgets.map((def) => (
                <button
                  type="button"
                  className="catalog-item"
                  key={def.type}
                  onClick={() => addWidget(def.type)}
                  title={def.backing === 'new' ? t('catalog.plannedHint') : undefined}
                >
                  <span className="catalog-item-head">
                    <span className="catalog-item-title">{t(def.title)}</span>
                    <Badge tone={BACKING_TONE[def.backing]}>{t(`catalog.backing.${def.backing}`)}</Badge>
                  </span>
                  <span className="catalog-item-blurb">{t(def.blurb)}</span>
                  <span className="catalog-item-add" aria-hidden="true">
                    + {t('common:actions.add')}
                  </span>
                </button>
              ))}
            </div>
          </div>
        ))}
      </div>
    </Modal>
  );
}
