// SPDX-License-Identifier: AGPL-3.0-only
// Group detail pane (shown in the split when a group row is selected, instead of a node detail).
// No tabs: a header (type eyebrow · breadcrumb name · counts · Edit/Add actions) over a Health
// rollup (full-width bar + per-state legend) and the group's direct member nodes. Subgroup-only
// groups say so rather than showing an empty member list. Reuses the parent page's modals for edit
// and add-node.

import { useMemo } from 'react';
import { useTranslation } from 'react-i18next';
import { StatusDot } from '../ui/StatusDot';
import { Badge } from '../ui/Badge';
import { Button } from '../ui/Button';
import { HealthBar } from '../HealthBar/HealthBar';
import { GroupIcon } from '../NodeTree/GroupIcon';
import {
  asGroupType,
  buildNodeTree,
  groupPath,
  STATE_ORDER,
  subtreeTallyMap,
  tallyStates,
  type StateCounts,
} from '../../lib/nodeTree';
import { stateLabel } from '../../lib/format';
import { NODE_KIND_SPEC } from '../../lib/nodeKind';
import type { NodeGroup, NodeSummary } from '../../types/api';
import './NodeDetail.css';

interface Props {
  group: NodeGroup;
  groups: NodeGroup[];
  nodes: NodeSummary[];
  /** The server's per-group DIRECT member counts (`/fleet/group-summary`) — the same input the tree
   *  row's rollup uses. Required, and that is the point: see the rollup note below. */
  groupCounts: Record<string, StateCounts>;
  canEdit: boolean;
  onEditGroup: (group: NodeGroup) => void;
  onAddNode: () => void;
}

export function GroupDetail({
  group,
  groups,
  nodes,
  groupCounts,
  canEdit,
  onEditGroup,
  onAddNode,
}: Props) {
  const { t } = useTranslation('nodes');
  // 🚨 **The rollup comes from the server counts, exactly as the tree row's does** (ADR-125). It
  // used to be `tallyStates(descendantNodes(...))` over the members that happened to be LOADED,
  // while the row beside it read `subtreeTallyMap(groupCounts)` — so the same folder could show two
  // different numbers, and which one you got depended on what had been fetched
  // (`extensibility.md` §3). It also cost the whole subtree load: the member cache fetched every
  // descendant folder purely so this arithmetic would come out right, which on the deployment this
  // was measured on meant selecting a root folder fired ~500 requests.
  //
  // The tree is built from the groups alone — `buildNodeTree(groups, [])` — because only the shape
  // is needed here; the nodes are not walked at all. Memoized because this component re-renders on
  // every SSE frame.
  const tally = useMemo(() => {
    const roots = buildNodeTree(groups, []).roots;
    return subtreeTallyMap(roots, groupCounts).get(group.id) ?? tallyStates([]);
  }, [groups, groupCounts, group.id]);
  const directMembers = useMemo(
    () => nodes.filter((n) => n.group_id === group.id),
    [nodes, group.id],
  );
  const subgroups = useMemo(
    () => groups.filter((g) => g.parent_id === group.id).length,
    [groups, group.id],
  );
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
            {tally.total} {t('common:noun.node', { count: tally.total })}
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
          <HealthBar tally={tally} className="nd-grp-healthbar" />
          <div className="nd-grp-legend">
            {STATE_ORDER.filter((s) => tally.counts[s] > 0).map((s) => (
              <span className="nd-grp-legend-item" key={s}>
                <StatusDot state={s} withLabel={false} />
                {tally.counts[s]} {stateLabel(s)}
              </span>
            ))}
            {tally.total === 0 && <span className="nd-muted">{t('groupDetail.noNodes')}</span>}
          </div>
        </section>

        {/* The folder's labels (ADR-135 inc. 2). Two marked groups, same as a node's overview: its
            own and the ones it inherits from above. Hidden entirely when it carries neither — an
            empty section would claim the operator had decided something they have not. The
            inherited half is `effective_tags` minus `tags`, both resolved on the row by the
            server, so nothing here walks the folder tree. */}
        {group.effective_tags.length > 0 && (
          <section>
            <div className="nd-section-t">{t('groupDetail.tags')}</div>
            {group.tags.length > 0 && (
              <div className="nd-tag-chips">
                {[...group.tags].sort((a, b) => a.localeCompare(b)).map((label) => (
                  <Badge key={label}>{label}</Badge>
                ))}
              </div>
            )}
            {group.effective_tags.some((l) => !group.tags.includes(l)) && (
              <>
                <div className="nd-tag-inherited-t">{t('field.tagsInheritedFrom')}</div>
                <div className="nd-tag-chips">
                  {group.effective_tags
                    .filter((l) => !group.tags.includes(l))
                    .sort((a, b) => a.localeCompare(b))
                    .map((label) => (
                      <Badge key={label}>{label}</Badge>
                    ))}
                </div>
              </>
            )}
          </section>
        )}

        {/* The site's IP prefixes (ADR-100 decision 10). Drawn only when the folder has some,
            which for a Region — and for every folder on a deployment with no NetBox — is never.
            An empty section here would read as "this site has no subnets", a claim nothing has
            made. */}
        {group.prefixes.length > 0 && (
          <section>
            <div className="nd-section-t">{t('groupDetail.prefixes')}</div>
            <div className="nd-prefixes">
              {group.prefixes.map((p) => (
                <div className="nd-prefix" key={p.prefix}>
                  <span className="nd-prefix-cidr mono">{p.prefix}</span>
                  {p.description && <span className="nd-prefix-desc">{p.description}</span>}
                  {/* Where the range came from (ADR-131). Worth saying here for the same reason
                      the editor disables a sync row: an operator looking at a range they cannot
                      change should be able to see why without opening the dialog. */}
                  <span className="nd-prefix-src">{t(`group.prefixSource.${p.source}`)}</span>
                </div>
              ))}
            </div>
          </section>
        )}

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
