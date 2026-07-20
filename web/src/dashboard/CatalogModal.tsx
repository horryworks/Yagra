// SPDX-License-Identifier: AGPL-3.0-only
// Add-widget catalog: the registry grouped by section, each widget a clickable card that adds
// an instance to the board. Backing tags (live/rollup) carry over from the catalog so the
// operator knows what's data-backed. Stays open after an add so several can be placed at once.

import { useTranslation } from 'react-i18next';
import { Badge } from '../components/ui/Badge';
import { Button } from '../components/ui/Button';
import { Modal } from '../components/ui/Modal';
import { useLayoutStoreContext } from './LayoutStoreContext';
import { catalogBySection } from './registry';
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
  const sections = catalogBySection();
  return (
    <Modal
      title={t('catalog.title')}
      onClose={onClose}
      footer={<Button onClick={onClose}>{t('actions.done')}</Button>}
    >
      <div className="catalog">
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
