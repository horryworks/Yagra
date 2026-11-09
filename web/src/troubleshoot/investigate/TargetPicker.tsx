// SPDX-License-Identifier: AGPL-3.0-only
// What a question is asked about (ADR-251 decision 11), four ways: a name or address, a node with
// an alert, a node opened lately, or a site / folder and everything beneath it — the way in for
// someone who knows the place but not the node.

import { useMemo, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { api } from '../../services/api';
import type { NodeGroup } from '../../types/api';
import { useAlertStore } from '../../store';
import { useLoad } from '../../lib/useLoad';
import { groupOptions } from '../../lib/nodeTree';
import { readRecentNodes } from '../../lib/recentNodes';
import { Card } from '../../components/ui/Card';
import { Inline, Stack } from '../../components/ui/Page';
import { Button } from '../../components/ui/Button';
import { EmptyState } from '../../components/ui/EmptyState';
import { GroupPicker } from '../../components/ui/GroupPicker';
import { NodePicker } from '../../components/NodePicker/NodePicker';
import { useEntityNames } from '../../components/ui/entityNames';
import { alertingNodeIds, type Target, type TargetKind } from './investigate';

/** The most nodes each short list offers. */
const LIST_MAX = 8;

function NodeButtons({ ids, onPick }: { ids: string[]; onPick: (t: Target) => void }) {
  const { nodeName } = useEntityNames();
  return (
    <Inline gap={6}>
      {ids.map((id) => (
        <Button key={id} type="button" onClick={() => onPick({ kind: 'node', id })}>
          {nodeName(id)}
        </Button>
      ))}
    </Inline>
  );
}

export function TargetPicker({
  kinds,
  onPick,
}: {
  kinds: readonly TargetKind[];
  onPick: (t: Target) => void;
}) {
  const { t } = useTranslation('troubleshoot');
  const alerts = useAlertStore((s) => s.alerts);
  const alerting = useMemo(() => alertingNodeIds(Object.values(alerts), LIST_MAX), [alerts]);
  const [recent] = useState(readRecentNodes);
  const groups = useLoad<NodeGroup[]>(() => api.listNodeGroups(), [], {
    initial: [],
    enabled: kinds.includes('folder'),
  });
  const options = useMemo(() => groupOptions(groups.data), [groups.data]);

  return (
    <Stack gap={16}>
      {kinds.includes('node') && (
        <Card title={t('investigate.pick.search')}>
          <NodePicker
            value={null}
            placeholder={t('investigate.pick.searchPlaceholder')}
            onChange={(n) => n && onPick({ kind: 'node', id: n.id })}
          />
        </Card>
      )}
      {kinds.includes('folder') && (
        <Card title={t('investigate.pick.folder')}>
          <GroupPicker
            options={options}
            value=""
            placeholder={t('investigate.pick.folderPlaceholder')}
            onChange={(id) => id && onPick({ kind: 'folder', id })}
          />
        </Card>
      )}
      {kinds.includes('node') && (
        <Card title={t('investigate.pick.alerting')}>
          {alerting.length > 0 ? (
            <NodeButtons ids={alerting} onPick={onPick} />
          ) : (
            <EmptyState text={t('investigate.pick.noAlerting')} />
          )}
        </Card>
      )}
      {kinds.includes('node') && recent.length > 0 && (
        <Card title={t('investigate.pick.recent')}>
          <NodeButtons ids={recent} onPick={onPick} />
        </Card>
      )}
    </Stack>
  );
}
