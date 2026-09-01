// SPDX-License-Identifier: AGPL-3.0-only
// Group detail pane (shown in the split when a group row is selected, instead of a node detail).
// No tabs: a header (type eyebrow · breadcrumb name · counts · Edit/Add actions) over a Health
// rollup (full-width bar + per-state legend) and the group's direct member nodes. Subgroup-only
// groups say so rather than showing an empty member list. Reuses the parent page's modals for edit
// and add-node.

import { useTranslation } from 'react-i18next';
import { StatusDot } from '../ui/StatusDot';
import { Button } from '../ui/Button';
import { HealthBar } from '../HealthBar/HealthBar';
import { GroupIcon } from '../NodeTree/GroupIcon';
import {
  asGroupType,
  buildNodeTree,
  descendantNodes,
  groupPath,
  STATE_ORDER,
  tallyStates,
} from '../../lib/nodeTree';
import { stateLabel } from '../../lib/format';
import { NODE_KIND_SPEC } from '../../lib/nodeKind';
import type { NodeGroup, NodeSummary } from '../../types/api';
import './NodeDetail.css';
import { findTreeGroup } from '../../lib/nodeTree';

interface Props {
  group: NodeGroup;
  groups: NodeGroup[];
  nodes: NodeSummary[];
  canEdit: boolean;
  onEditGroup: (group: NodeGroup) => void;
  onAddNode: () => void;
}

export function GroupDetail({ group, groups, nodes, canEdit, onEditGroup, onAddNode }: Props) {
  const { t } = useTranslation('nodes');
  const tree = buildNodeTree(groups, nodes);
  const tg = findTreeGroup(tree.roots, group.id);
  const all = tg ? descendantNodes(tg) : [];
  const directMembers = tg ? tg.nodes : [];
  const subgroups = tg ? tg.children.length : 0;
  const tally = tallyStates(all);
  const path = groupPath(groups, group.id);

  return (
    <div className="nd">
      <div className="nd-head">
        <div className="nd-eyebrow">
          <GroupIcon type={asGroupType(group.group_type)} />{' '}
          {group.group_type === 'generic'
            ? t('groupType.genericFolder')
            : t(`groupType.${group.group_type}`)}
        </div>
        <div className="nd-namerow">
          <div className="nd-namewrap">
            <span className="nd-name">{path.join(' / ') || group.name}</span>
          </div>
          {canEdit && (
            <div className="nd-actions">
              <Button variant="outline" onClick={() => onEditGroup(group)}>
                {t('group.edit')}
              </Button>
              <Button variant="primary" onClick={onAddNode}>
                {t('add.node')}
              </Button>
            </div>
          )}
        </div>
        <div className="nd-sub">
          <span>
            {all.length} {t('common:noun.node', { count: all.length })}
          </span>
          <span className="nd-sep">·</span>
          <span>{t('count.subgroup', { count: subgroups })}</span>
          <span className="nd-sep">·</span>
          <span className={tally.needAttention ? 'nd-attention' : undefined}>
            {t('inventory.needAttention', { count: tally.needAttention })}
          </span>
        </div>
      </div>

      <div className="nd-grpbody">
        <section>
          <div className="nd-section-t">{t('groupDetail.health')}</div>
          <HealthBar nodes={all} className="nd-grp-healthbar" />
          <div className="nd-grp-legend">
            {STATE_ORDER.filter((s) => tally.counts[s] > 0).map((s) => (
              <span className="nd-grp-legend-item" key={s}>
                <StatusDot state={s} withLabel={false} />
                {tally.counts[s]} {stateLabel(s)}
              </span>
            ))}
            {all.length === 0 && <span className="nd-muted">{t('groupDetail.noNodes')}</span>}
          </div>
        </section>

        <section>
          <div className="nd-section-t">{t('groupDetail.members')}</div>
          {directMembers.length > 0 ? (
            <div className="nd-members">
              {directMembers.map((n) => (
                <div className="nd-member" key={n.id}>
                  <StatusDot state={n.state} withLabel={false} />
                  <span className="nd-member-name">{n.name}</span>
                  {NODE_KIND_SPEC[n.kind].badge && (
                    <span className="nd-kind" title={t(NODE_KIND_SPEC[n.kind].labelKey)}>
                      {NODE_KIND_SPEC[n.kind].badge}
                    </span>
                  )}
                  <span className="nd-member-addr mono">{n.address}</span>
                </div>
              ))}
            </div>
          ) : (
            <p className="nd-muted">{t('groupDetail.noDirectMembers')}</p>
          )}
        </section>
      </div>
    </div>
  );
}
