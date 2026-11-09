// SPDX-License-Identifier: AGPL-3.0-only
// Troubleshoot > Investigate (ADR-251 decision 10): one card per question. A node's own
// "Investigate" action opens this page asked about that node, and a card then carries it on.

import { useTranslation } from 'react-i18next';
import { Link, useSearchParams } from 'react-router-dom';
import { INVESTIGATE_QUESTIONS, type InvestigateQuestion } from '../../types/api';
import { Page, Inline } from '../../components/ui/Page';
import { PageHeader } from '../../components/ui/PageHeader';
import { Badge } from '../../components/ui/Badge';
import { useEntityNames } from '../../components/ui/entityNames';
import {
  INVESTIGATE_PATH,
  QUESTION_CARDS,
  questionHref,
  targetFromParams,
  type Target,
} from './investigate';

function QuestionCardLink({ q, target }: { q: InvestigateQuestion; target: Target | null }) {
  const { t } = useTranslation('troubleshoot');
  const card = QUESTION_CARDS[q];
  return (
    <Link to={questionHref(q, target)} className="ts-tool investigate-card">
      <div className="ts-tool-top">
        <div className="ts-tool-mono">{card.mono}</div>
        <div className="ts-tool-titles">
          <span className="ts-tool-name">{t(`investigate.question.${q}`)}</span>
        </div>
      </div>
      <p className="ts-tool-desc">{t(`investigate.questionDesc.${q}`)}</p>
      <div className="ts-tool-meta">
        <Inline gap={4}>
          {card.hypotheses.map((h) => (
            <Badge key={h} tone="neutral">
              {t(`investigate.hypothesis.${h}`)}
            </Badge>
          ))}
        </Inline>
      </div>
    </Link>
  );
}

export function InvestigateHome() {
  const { t } = useTranslation(['troubleshoot', 'nav']);
  const [params] = useSearchParams();
  const target = targetFromParams(params);
  const { nodeName, groupName } = useEntityNames();
  const targetName = target
    ? target.kind === 'node'
      ? nodeName(target.id)
      : groupName(target.id)
    : null;

  return (
    <Page layout="sections">
      <PageHeader
        title={t('nav:troubleshoot.investigate')}
        trail={[
          { label: t('nav:sections.troubleshoot'), to: '/troubleshoot' },
          { label: t('nav:troubleshoot.investigate') },
        ]}
      />
      {targetName && (
        <Inline gap={8}>
          <Badge tone="info">{t('investigate.about', { name: targetName })}</Badge>
          <Link to={INVESTIGATE_PATH}>{t('investigate.clearTarget')}</Link>
        </Inline>
      )}
      <div className="ts-tool-grid">
        {INVESTIGATE_QUESTIONS.map((q) => (
          <QuestionCardLink key={q} q={q} target={target} />
        ))}
      </div>
    </Page>
  );
}
