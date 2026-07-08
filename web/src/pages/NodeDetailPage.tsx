// Node detail route (/nodes/:nodeId). A thin wrapper around the shared <NodeDetail> view — the same
// component rendered inline in the Nodes split — so the deep route and the inline pane never drift.
// It supplies the page chrome (breadcrumb), keeps the active sub-tab in the URL (`?tab=…`) so a
// reload restores it, loads the group list for the detail's breadcrumb/parent resolution, and wires
// the post-delete navigation back to the inventory.

import { useEffect, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { useNavigate, useParams, useSearchParams } from 'react-router-dom';
import { api } from '../services/api';
import { useAuthStore } from '../store';
import type { NodeGroup } from '../types/api';
import { Breadcrumb } from '../components/shell/Breadcrumb';
import { NodeDetail } from '../components/NodeDetail/NodeDetail';

const TABS = ['overview', 'interfaces', 'collection', 'events'];

export function NodeDetailPage() {
  const { t } = useTranslation();
  const { nodeId = '' } = useParams();
  const navigate = useNavigate();
  const authed = useAuthStore((s) => s.authed);
  // Keep the active sub-tab in the URL so a browser reload restores it instead of snapping back
  // to Overview.
  const [searchParams, setSearchParams] = useSearchParams();
  const tabParam = searchParams.get('tab') ?? '';
  const tab = TABS.includes(tabParam) ? tabParam : 'overview';
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
        canEdit={authed}
        tab={tab}
        onTabChange={setTab}
        groups={groups}
        onDeleted={() => navigate('/nodes')}
      />
    </div>
  );
}
