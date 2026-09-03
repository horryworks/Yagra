// SPDX-License-Identifier: AGPL-3.0-only
// Settings ▸ Integrations. Catalogue of external systems Yagra can read.
//
// **Every tile comes from `registry.ts`** — this file holds no vendor's name, link or probe.
// It used to: Meraki's `<Link>` and a "more coming" placeholder were written here in JSX, and
// ADR-037 recorded that a second integration should turn that into a registry. NetBox is the
// second (ADR-100), so adding a third is one entry there and two locale strings, not an edit here.
//
// The status chip is derived, not authored: each entry says how to read its own live state, and
// this file only knows how to render a chip and what to do when a probe is refused.

import { useEffect, useState } from 'react';
import { Link } from 'react-router-dom';
import { useTranslation } from 'react-i18next';
import { PageHeader } from '../../components/ui/PageHeader';
import { classifyLoadError } from '../../lib/loadState';
import { LoadBlockNotice } from '../../components/ui/LoadBlockNotice';
import { blockedChip, chipLabel, loadingChip, type Chip } from './chip';
import { integrationCards, type IntegrationId } from './registry';
import './IntegrationsCatalogPage.css';

/** A load failure that is about the deployment or the caller rather than the vendor. */
type Block = 'unavailable' | 'forbidden';

export function IntegrationsCatalogPage() {
  const { t } = useTranslation('system');
  const cards = integrationCards();
  const [chips, setChips] = useState<Partial<Record<IntegrationId, Chip>>>({});
  const [block, setBlock] = useState<Block | null>(null);

  useEffect(() => {
    let alive = true;
    for (const card of cards) {
      card
        .probe()
        .then((chip) => {
          if (alive) setChips((prev) => ({ ...prev, [card.id]: chip }));
        })
        .catch((e: unknown) => {
          if (!alive) return;
          // A refusal or an outage is about the deployment, so it is shown once for the page as
          // well as on the tile — but the tile still renders, because "you may not read this" is
          // not "this integration does not exist".
          const b = classifyLoadError(e) as Block | null;
          setChips((prev) => ({
            ...prev,
            [card.id]: b ? blockedChip(b) : { labelKey: 'integrations.status.notConfigured', tone: 'idle' },
          }));
          if (b) setBlock((prev) => prev ?? b);
        });
    }
    return () => {
      alive = false;
    };
    // `cards` is derived from a module-level constant, so it is stable across renders.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  return (
    <div>
      <PageHeader
        title={t('nav:settings.integrations')}
        trail={[{ label: t('nav:sections.settings') }, { label: t('nav:settings.integrations') }]}
        note={t('integrations.note')}
      />

      <div className="integrations-grid">
        {cards.map((card) => {
          const chip = chips[card.id] ?? loadingChip();
          return (
            <Link key={card.id} className="integration-card" to={card.path}>
              <div className="integration-card-head">
                <span className="integration-card-name">{t(card.nameKey)}</span>
                <span className={`integration-chip ${chip.tone}`}>{chipLabel(chip, t)}</span>
              </div>
              <p className="integration-card-desc">{t(card.descKey)}</p>
            </Link>
          );
        })}
      </div>

      {block && <LoadBlockNotice block={block} unavailable={t('integrations.unavailable')} />}
    </div>
  );
}
