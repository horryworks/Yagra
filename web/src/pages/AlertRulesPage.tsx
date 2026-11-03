// SPDX-License-Identifier: AGPL-3.0-only
// Alerts ▸ Alert rules (ADR-232): one heading, a tab per way an alert is made, and the screen below.
// The chip beside the tabs says how that tab's rules combine — the one difference between the two
// that the screens did not show before. The tab is the path, so each screen keeps its URL filters.

import { useTranslation } from 'react-i18next';
import { Outlet, useLocation, useNavigate } from 'react-router-dom';
import { PageHeader } from '../components/ui/PageHeader';
import { Tabs } from '../components/ui/Tabs';
import {
  ALERT_RULE_HOW_KEYS,
  ALERT_RULE_TAB_LABEL_KEYS,
  ALERT_RULE_TABS,
  alertRuleTabOf,
  alertRuleTabPath,
} from './alertRules';
import './AlertRulesPage.css';

export function AlertRulesPage() {
  const { t } = useTranslation('nav');
  const { pathname } = useLocation();
  const navigate = useNavigate();
  const active = alertRuleTabOf(pathname);
  const title = t('alerts.alertRules');
  return (
    <div>
      <PageHeader
        title={title}
        trail={[
          { label: t('sections.alerts') },
          { label: title },
          { label: t(ALERT_RULE_TAB_LABEL_KEYS[active]) },
        ]}
      />
      <div className="alert-rules-tabs">
        <Tabs
          tabs={ALERT_RULE_TABS.map((key) => ({ key, label: t(ALERT_RULE_TAB_LABEL_KEYS[key]) }))}
          active={active}
          onChange={(key) => navigate(alertRuleTabPath(key))}
        />
        <span className="alert-rules-how">{t(ALERT_RULE_HOW_KEYS[active])}</span>
      </div>
      <Outlet />
    </div>
  );
}
