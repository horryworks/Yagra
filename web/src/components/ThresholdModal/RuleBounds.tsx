// SPDX-License-Identifier: AGPL-3.0-only
import { useTranslation } from 'react-i18next';
import { boundText } from '../../lib/portRuleForm';
import type { StoredThreshold } from '../../types/api';
import { Badge } from '../ui/Badge';

/** Every bound a rule names, each carrying the side it reads in — up to four since ADR-081.
 *
 *  Shared by Alert rules and Node detail ▸ Alert rules, so a rule reads the same on both.
 *  Rendering `warning`/`critical` instead would show half of a band rule, with nothing on the row
 *  to say a half is what it was. Read in the metric's own unit: an absolute interface rate is
 *  stored in bits per second (ADR-076 decision 9). */
export function RuleBounds({ rule }: { rule: StoredThreshold }) {
  const { t } = useTranslation('alertsConfig');
  return (
    <span className="thresholds-bounds">
      {(
        [
          ['warning', '≤', rule.warning_below],
          ['critical', '≤', rule.critical_below],
          ['warning', '≥', rule.warning_above],
          ['critical', '≥', rule.critical_above],
        ] as const
      )
        .filter(([, , v]) => v != null)
        .map(([tone, cmp, v]) => (
          <Badge key={`${tone}${cmp}`} tone={tone}>
            {t(tone === 'warning' ? 'thresholds.warnShort' : 'thresholds.critShort')} {cmp}
            {boundText(rule.metric, v as number)}
          </Badge>
        ))}
    </span>
  );
}
