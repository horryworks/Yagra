// SPDX-License-Identifier: AGPL-3.0-only
// Node detail route (/nodes/:nodeId). A thin wrapper around the shared <NodeDetail> view — the same
// component rendered inline in the Nodes split — so the deep route and the inline pane never drift.
// It supplies the page chrome (breadcrumb), keeps the active sub-tab in the URL (`?tab=…`) so a
// reload restores it, loads the group list for the detail's breadcrumb/parent resolution, and wires
// the post-delete navigation back to the inventory.

import { useEffect, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { useNavigate, useParams, useSearchParams } from 'react-router-dom';
import { api } from '../services/api';
import { useCan, useNodeTabStore } from '../store';
import type { NodeGroup } from '../types/api';
import { Breadcrumb } from '../components/shell/Breadcrumb';
import { NodeDetail } from '../components/NodeDetail/NodeDetail';
import { requestedNodeDetailTab } from '../components/NodeDetail/tabs';

export function NodeDetailPage() {
  const { t } = useTranslation();
  const { nodeId = '' } = useParams();
  const navigate = useNavigate();
  const canConfig = useCan('manage_config');
  // Keep the active sub-tab in the URL so a browser reload restores it instead of snapping back
  // to Overview. When the URL names no tab — which is every in-app arrival here, since the search
  // box, the topology map and the dependency list all navigate to a bare `/nodes/<id>` — the tab
  // the operator last clicked stands in (ADR-134). The URL still wins whenever it says anything.
  const [searchParams, setSearchParams] = useSearchParams();
  const tabParam = searchParams.get('tab') ?? '';
  const remembered = useNodeTabStore((s) => s.tab);
  const tab = requestedNodeDetailTab(tabParam, remembered);
  const setTab = (next: string) => {
    const params = new URLSearchParams(searchParams);
    params.set('tab', next);
    setSearchParams(params, { replace: true });
  };

  const [groups, setGroups] = useState<NodeGroup[]>([]);
  useEffect(() => {
    api.listNodeGroups().then(setGroups).catch(() => setGroups([]));
  }, []);

  return (
    <div className="page-fill">
      <Breadcrumb
        trail={[{ label: t('nav:sections.nodes') }, { label: t('nav:nodes.all'), to: '/nodes' }]}
      />
      <NodeDetail
        nodeId={nodeId}
        variant="page"
        canEdit={canConfig}
        tab={tab}
        onTabChange={setTab}
        groups={groups}
        onDeleted={() => navigate('/nodes')}
      />
    </div>
  );
}
