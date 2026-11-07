// SPDX-License-Identifier: AGPL-3.0-only
// Nodes ▸ Collection setup (ADR-231): one heading, a tab per screen, and the screen below. The
// screens draw no header of their own; the tab is the path, so each keeps its URL filters.
//
// A list page (ADR-246): each tab returns its toolbar and table as a fragment, so they are this
// page's own children - spaced by it, and the table takes the height left under the tabs.

import { useTranslation } from 'react-i18next';
import { Outlet, useLocation, useNavigate } from 'react-router-dom';
import { Page } from '../components/ui/Page';
import { PageHeader } from '../components/ui/PageHeader';
import { Tabs } from '../components/ui/Tabs';
import {
  COLLECTION_TAB_LABEL_KEYS,
  COLLECTION_TABS,
  collectionTabOf,
  collectionTabPath,
} from './collectionSetup';

export function CollectionSetupPage() {
  const { t } = useTranslation('nav');
  const { pathname } = useLocation();
  const navigate = useNavigate();
  const active = collectionTabOf(pathname);
  const title = t('nodes.collectionSetup');
  return (
    <Page layout="list">
      <PageHeader
        title={title}
        trail={[
          { label: t('sections.nodes') },
          { label: title },
          { label: t(COLLECTION_TAB_LABEL_KEYS[active]) },
        ]}
      />
      <Tabs
        tabs={COLLECTION_TABS.map((key) => ({ key, label: t(COLLECTION_TAB_LABEL_KEYS[key]) }))}
        active={active}
        onChange={(key) => navigate(collectionTabPath(key))}
      />
      <Outlet />
    </Page>
  );
}
