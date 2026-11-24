// SPDX-License-Identifier: AGPL-3.0-only
import { useTranslation } from 'react-i18next';
import { alertTitle } from '../../lib/alertName';
import { flagFault, flagSeverity } from '../../lib/flagRule';
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
  // ADR-264: a 0/1 metric's rule in its one simple shape reads as the alert it raises. Any other
  // shape keeps the bounds, as the dialog keeps the boxes for it.
  const fault = flagFault(rule.metric);
  const sev =
    fault === null
      ? null
      : flagSeverity(fault, {
          warningBelow: rule.warning_below ?? undefined,
          criticalBelow: rule.critical_below ?? undefined,
          warningAbove: rule.warning_above ?? undefined,
          criticalAbove: rule.critical_above ?? undefined,
        });
  if (sev === 'warning' || sev === 'critical') {
    return (
      <span className="thresholds-bounds">
        <Badge tone={sev}>
          {t(sev === 'warning' ? 'thresholds.warnShort' : 'thresholds.critShort')}{' '}
          {alertTitle(rule.metric)?.text ?? rule.metric}
        </Badge>
      </span>
    );
  }
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
