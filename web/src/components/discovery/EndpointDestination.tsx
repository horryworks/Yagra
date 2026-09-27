// SPDX-License-Identifier: AGPL-3.0-only
// Where an unregistered endpoint goes when it is monitored (ADR-179 増分 8): a folder, and whether
// a folder whose IP range holds the address takes it instead — the range-scan import's two
// controls, for Discovery ▸ Unregistered devices and the Node ▸ Neighbors setup panel alike.
//
// The caller draws it only with manage_config (ADR-056): it only matters to an import.

import { useMemo } from 'react';
import { useTranslation } from 'react-i18next';
import type { NodeGroup } from '../../types/api';
import { groupOptions } from '../../lib/nodeTree';
import type { SetupDestination } from '../../pages/discoveredEndpoints';
import { GroupPicker } from '../ui/GroupPicker';
import { FieldHint } from '../ui/Field';

interface Props {
  groups: NodeGroup[];
  value: SetupDestination;
  onChange: (next: SetupDestination) => void;
  disabled?: boolean;
  /** Whether to draw "File by IP range" at all — not once no folder holds a range (the scan
   *  import's rule: a control that explains nothing is not drawn). */
  offerFilingByRange: boolean;
}

export function EndpointDestination({
  groups,
  value,
  onChange,
  disabled,
  offerFilingByRange,
}: Props) {
  const { t } = useTranslation('monitoring');
  const options = useMemo(() => groupOptions(groups), [groups]);
  return (
    <div className="ep-dest">
      <label className="form-label">
        {t('discovery.seen.dest.label')}
        <GroupPicker
          options={options}
          value={value.groupId}
          onChange={(groupId) => onChange({ ...value, groupId })}
          emptyOption={t('discovery.dest.root')}
          disabled={disabled}
        />
      </label>
      {/* Hint outside the label, for the reason the scan import's is. */}
      {offerFilingByRange && (
        <>
          <label className="form-label form-check">
            <input
              type="checkbox"
              checked={value.fileByPrefix}
              disabled={disabled}
              onChange={(e) => onChange({ ...value, fileByPrefix: e.target.checked })}
            />
            {t('discovery.seen.dest.byRange')}
          </label>
          <FieldHint>{t('discovery.seen.dest.byRangeHint')}</FieldHint>
        </>
      )}
    </div>
  );
}
