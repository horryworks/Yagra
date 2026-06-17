// Add-widget catalog: the registry grouped by section, each widget a clickable card that adds
// an instance to the board. Backing tags (live/rollup) carry over from the catalog so the
// operator knows what's data-backed. Stays open after an add so several can be placed at once.

import { Badge } from '../components/ui/Badge';
import { Button } from '../components/ui/Button';
import { Modal } from '../components/ui/Modal';
import { useLayoutStore } from './layoutStore';
import { catalogBySection } from './registry';
import type { Backing } from './types';
import './CatalogModal.css';

const BACKING_TONE: Record<Backing, 'up' | 'info' | 'neutral'> = {
  live: 'up',
  rollup: 'info',
  new: 'neutral',
};
const BACKING_LABEL: Record<Backing, string> = {
  live: 'Live',
  rollup: 'Rollup',
  new: 'Planned',
};

export function CatalogModal({ onClose }: { onClose: () => void }) {
  const addWidget = useLayoutStore((s) => s.addWidget);
  const sections = catalogBySection();
  return (
    <Modal
      title="Add a widget"
      onClose={onClose}
      footer={<Button onClick={onClose}>Done</Button>}
    >
      <div className="catalog">
        {sections.map(({ section, widgets }) => (
          <div className="catalog-section" key={section}>
            <h3 className="catalog-section-title">{section}</h3>
            <div className="catalog-grid">
              {widgets.map((def) => (
                <button
                  type="button"
                  className="catalog-item"
                  key={def.type}
                  onClick={() => addWidget(def.type)}
                >
                  <span className="catalog-item-head">
                    <span className="catalog-item-title">{def.title}</span>
                    <Badge tone={BACKING_TONE[def.backing]}>{BACKING_LABEL[def.backing]}</Badge>
                  </span>
                  <span className="catalog-item-blurb">{def.blurb}</span>
                  <span className="catalog-item-add" aria-hidden="true">
                    + Add
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
