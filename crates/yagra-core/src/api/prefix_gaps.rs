// SPDX-License-Identifier: AGPL-3.0-only
//! Nodes ▸ Missing IP prefixes (ADR-170 Inc.2) — for every site, the subnets its devices carry that
//! none of its IP prefixes covers.
//!
//! One read and two writes. The read is the folder pane's comparison (`GET /node-groups/{id}/prefix-gaps`) run once per
//! site over a single read of every store, so the fleet costs one pass rather than one per site —
//! which is also why the per-folder 2,000-device refusal does not apply here (ADR-170 decision 13).
//! The answer is cut at [`SITE_GAPS_MAX`] gaps instead, and says how many there were.
//!
//! What a scoped caller is told follows the folder pane exactly, and the two share the code that
//! decides it ([`withhold_ranges`], [`fill_port_names`]): only the sites of devices it can see are
//! listed, and a range belonging to a folder it cannot see is withheld while the subnet is still
//! reported as claimed (ADR-014).
//!
//! The writes mark one gap as intentional and take the mark back (ADR-170 Inc.4). They are
//! `ManageConfig` and refused to a scoped caller - the same line Subnet overlaps draws, so the two
//! screens offer their buttons to the same people.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use axum::extract::Query;
use axum::http::StatusCode;
use axum::{routing::get, routing::put, Json, Router};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::error::{ApiError, ApiResult};
use super::extract::{Admin, RequireManageConfig, RequireView, Scoped};
use super::scope::{require_fleet_wide, NodeScope};
use super::{AdminState, ApiState};
use crate::prefix_gaps::{by_site, GapMark, PrefixGap};
use yagra_base::repo::StoredGapAck;
use yagra_common::{L3Snapshot, NodeKind, SubnetKey};

/// The most gaps one answer lists, across every site. Each site's `gap_count` is never capped.
pub(crate) const SITE_GAPS_MAX: usize = 2_000;

/// The longest note accepted on a mark - the overlap screen's limit, read from there.
const NOTE_MAX: usize = super::subnet_overlaps::TEXT_MAX;

const SCOPED_WRITE: &str =
    "an intentional mark applies to the whole site; a folder-scoped token cannot set one";

/// This domain's slice of the OpenAPI document (ADR-035), merged by [`super::openapi::document`].
#[derive(utoipa::OpenApi)]
#[openapi(paths(get_site_prefix_gaps, set_gap_ack, delete_gap_ack))]
pub(super) struct Doc;

/// The missing-prefix routes, merged into `/api/v1` by [`super::router`].
pub(super) fn routes() -> Router<ApiState> {
    Router::new()
        .route("/api/v1/prefix-gaps", get(get_site_prefix_gaps))
        .route(
            "/api/v1/prefix-gaps/acks",
            put(set_gap_ack).delete(delete_gap_ack),
        )
}

/// "This subnet is meant to stay out of this site's IP prefixes."
#[derive(Debug, Deserialize, utoipa::ToSchema)]
pub(crate) struct GapAckBody {
    /// The site's `site_id`, as the list returned it; `null` for the root.
    site_id: Option<Uuid>,
    /// The gap's `subnet`, as the list returned it.
    subnet: String,
    #[serde(default)]
    note: String,
}

/// Which mark to take back.
#[derive(Debug, Deserialize, utoipa::IntoParams)]
pub(crate) struct GapAckQuery {
    /// The site's `site_id`; omitted for the root.
    site_id: Option<Uuid>,
    /// The gap's `subnet`.
    subnet: String,
}

/// Where one site stands - the screen's four tabs, in their order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub(crate) enum SiteGapStatus {
    /// At least one subnet is missing from the site's IP prefixes.
    Gaps,
    /// Every subnet compared is covered.
    Clean,
    /// It has gaps, and an operator marked every one of them as intentional.
    Intentional,
    /// None of the site's devices has reported an address, so nothing was compared. Never read
    /// as complete (ADR-170 decision 6).
    NoData,
}

impl SiteGapStatus {
    /// Every status, for the token test and the WebUI's mirror.
    #[cfg(test)]
    const ALL: [SiteGapStatus; 4] = [Self::Gaps, Self::Clean, Self::Intentional, Self::NoData];
}

/// One site's answer.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub(crate) struct SitePrefixGaps {
    /// The site's folder. `null` is the root: every device filed in no folder, taken as one site.
    pub(crate) site_id: Option<Uuid>,
    /// The folder's name; `null` for the root.
    pub(crate) name: Option<String>,
    /// The folders above it, outermost first — only those this caller may see. The site's own
    /// `name` is given even to a caller scoped below it: naming the folder above yours is the
    /// breadcrumb ADR-014 allows (`groups::group_ancestors`), and says nothing of what is in it.
    pub(crate) path: Vec<String>,
    /// Whether the folder is of type Site. A device with no Site folder above it is compared
    /// against its own folder, which is then listed as a site with this `false`.
    pub(crate) is_site: bool,
    pub(crate) status: SiteGapStatus,
    /// Devices in the site. URL, DNS, Meraki and wireless-AP nodes report no interface addresses
    /// and are not counted.
    pub(crate) nodes_total: u32,
    /// Of those, how many have reported their addresses at all.
    pub(crate) nodes_with_addresses: u32,
    /// Of those, how many address lists were cut at the per-device cap.
    pub(crate) nodes_truncated: u32,
    /// IP prefixes filed in the site's folder or beneath it — only in folders this caller may see.
    pub(crate) prefixes: u32,
    /// Distinct subnets compared, covered ones included.
    pub(crate) subnets_checked: u32,
    /// How many of its gaps nobody has marked as intentional. `gaps` may list fewer when the
    /// answer was cut.
    pub(crate) gap_count: u32,
    /// How many of its gaps were marked as intentional.
    pub(crate) intentional_count: u32,
    /// The unmarked gaps first, then the marked ones; inside each, by kind, then subnet - the
    /// folder pane's order. So a cut answer drops marked gaps before unmarked ones.
    pub(crate) gaps: Vec<PrefixGap>,
}

/// What Nodes ▸ Missing IP prefixes shows.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub(crate) struct PrefixGapSitesView {
    /// Every site holding a device this caller may see. Most unmarked gaps first, then by name.
    pub(crate) sites: Vec<SitePrefixGaps>,
    /// Devices across those sites.
    pub(crate) nodes_total: u32,
    /// Of those, how many have reported their addresses at all. No gaps is complete only when
    /// this equals `nodes_total`.
    pub(crate) nodes_with_addresses: u32,
    pub(crate) nodes_truncated: u32,
    /// Subnets compared, summed over sites — one carried at two sites counts twice.
    pub(crate) subnets_checked: u32,
    /// Gaps across every site, marked ones included.
    pub(crate) gaps_total: u32,
    /// Gaps listed in `sites`. Less than `gaps_total` when the answer was cut at 2,000.
    pub(crate) gaps_listed: u32,
}

#[utoipa::path(
    get, path = "/api/v1/prefix-gaps", tag = "groups",
    responses(
        (status = 200, description = "For every site holding a device this caller may see, the subnets its devices carry that none of the IP prefixes filed in the site's folder or beneath it contains, with why each is reported and whether an operator marked it as intentional. A site is the nearest folder of type Site above a device, else the device's own folder; devices filed in no folder are one site", body = PrefixGapSitesView),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks the View permission", body = super::error::ErrorBody),
        (status = 503, description = "This core has no write side (skeleton mode)", body = super::error::ErrorBody),
    ),
)]
async fn get_site_prefix_gaps(
    _guard: RequireView,
    Scoped(scope): Scoped,
    admin: Admin,
) -> ApiResult<Json<PrefixGapSitesView>> {
    Ok(Json(site_gaps_view(&admin, &scope).await?))
}

fn internal(e: &anyhow::Error, what: &'static str) -> ApiError {
    ApiError::from_internal(e.as_ref(), what, "failed to read prefix gaps")
}

/// How many gaps each site may list, out of one `budget` for the whole answer (ADR-170 decision 17).
///
/// Shared, not spent greedily. Spending it in display order — most gaps first — let one site whose
/// devices carry a few thousand subnets take the whole budget, and every other site came back with
/// a count and nothing to open. Each site with gaps first gets an equal share (at least 10), then
/// whatever is left goes down the list in order. Under the budget nothing is cut.
fn gap_allowance(lens: &[usize], budget: usize) -> Vec<usize> {
    if lens.iter().sum::<usize>() <= budget {
        return lens.to_vec();
    }
    let with_gaps = lens.iter().filter(|n| **n > 0).count().max(1);
    let share = (budget / with_gaps).max(10);
    let mut left = budget;
    let mut keep: Vec<usize> = lens
        .iter()
        .map(|n| {
            let k = (*n).min(share).min(left);
            left -= k;
            k
        })
        .collect();
    for (k, n) in keep.iter_mut().zip(lens) {
        let more = (n - *k).min(left);
        *k += more;
        left -= more;
    }
    keep
}

fn count(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

/// The marks, keyed by (site, subnet): the nil uuid is the root, as `prefix_gap_acks` stores it.
type Marks = HashMap<(Uuid, String), StoredGapAck>;

/// Mark the gaps an operator acknowledged, then put the unmarked ones first (ADR-170 Inc.4).
///
/// A mark applies only while the gap still has the kind it had when it was marked: a subnet that
/// was "in no prefix" and is now "partly registered" is a different finding, and is open again.
/// Returns how many were marked. The sort is stable, so each half keeps the kind-then-subnet order.
fn apply_marks(site: Option<Uuid>, gaps: &mut [PrefixGap], marks: &Marks) -> usize {
    let site = site.unwrap_or(Uuid::nil());
    let mut marked = 0;
    for gap in gaps.iter_mut() {
        gap.intentional = marks
            .get(&(site, gap.subnet.clone()))
            .filter(|m| m.kind == gap.kind.token())
            .map(|m| GapMark {
                note: m.note.clone(),
            });
        marked += usize::from(gap.intentional.is_some());
    }
    gaps.sort_by_key(|g| g.intentional.is_some());
    marked
}

/// Which tab a site belongs on.
fn site_status(with_addresses: usize, gaps: usize, marked: usize) -> SiteGapStatus {
    if with_addresses == 0 {
        SiteGapStatus::NoData
    } else if gaps == 0 {
        SiteGapStatus::Clean
    } else if marked == gaps {
        SiteGapStatus::Intentional
    } else {
        SiteGapStatus::Gaps
    }
}

/// Every site's gaps, marked and uncut, before anything is withheld.
struct Compared {
    sites: Vec<SitePrefixGaps>,
    nodes_total: usize,
    nodes_with_addresses: usize,
    nodes_truncated: usize,
    groups: Vec<yagra_base::groups::GroupSummary>,
}

/// Every site's gaps, as this caller may see them — the seam REST and the MCP
/// `get_site_prefix_gaps` tool share, so the two cannot disclose differently.
pub(crate) async fn site_gaps_view(
    admin: &AdminState,
    scope: &NodeScope,
) -> ApiResult<PrefixGapSitesView> {
    let Compared {
        sites: mut out,
        nodes_total,
        nodes_with_addresses,
        nodes_truncated,
        groups,
    } = compare(admin, scope).await?;

    let gaps_total: usize = out.iter().map(|s| s.gaps.len()).sum();
    let lens: Vec<usize> = out.iter().map(|s| s.gaps.len()).collect();
    for (site, keep) in out.iter_mut().zip(gap_allowance(&lens, SITE_GAPS_MAX)) {
        site.gaps.truncate(keep);
    }

    let names: HashMap<Uuid, &str> = groups.iter().map(|g| (g.id, g.name.as_str())).collect();
    withhold_ranges(
        out.iter_mut().flat_map(|s| s.gaps.iter_mut()),
        &names,
        scope,
    );
    fill_port_names(
        admin,
        out.iter_mut().flat_map(|s| s.gaps.iter_mut()).collect(),
    )
    .await?;

    Ok(PrefixGapSitesView {
        nodes_total: count(nodes_total),
        nodes_with_addresses: count(nodes_with_addresses),
        nodes_truncated: count(nodes_truncated),
        subnets_checked: count(out.iter().map(|s| s.subnets_checked as usize).sum()),
        gaps_total: count(gaps_total),
        gaps_listed: count(out.iter().map(|s| s.gaps.len()).sum()),
        sites: out,
    })
}

/// The comparison itself, read from every store once.
async fn compare(admin: &AdminState, scope: &NodeScope) -> ApiResult<Compared> {
    let (groups, nodes, snapshots, acks) = tokio::try_join!(
        admin.groups.list(),
        admin.repo.node_folders(),
        admin.l3.all_current(),
        admin.repo.prefix_gap_acks(),
    )
    .map_err(|e| internal(&e, "read prefix gap inputs"))?;
    let marks: Marks = acks
        .into_iter()
        .map(|a| ((a.site, a.subnet.clone()), a))
        .collect();

    let site_of = crate::sites::sites_by_node(&groups, &nodes);
    let visible: Vec<Uuid> = nodes
        .iter()
        .filter(|(_, _, g)| scope.allows_group(*g))
        .map(|(id, ..)| *id)
        .collect();
    // What a node is comes from the one resolution every surface asks; a failed read degrades
    // to Device there, which over-counts rather than hiding a device.
    let kinds = super::nodes::node_kinds(admin, &visible).await;
    let devices: HashSet<Uuid> = visible
        .iter()
        .copied()
        .filter(|id| kinds.get(id).is_none_or(|k| *k == NodeKind::Device))
        .collect();

    let mut device_count: BTreeMap<Option<Uuid>, usize> = BTreeMap::new();
    for id in &devices {
        if let Some(site) = site_of.get(id) {
            *device_count.entry(*site).or_default() += 1;
        }
    }
    let sites: BTreeSet<Option<Uuid>> = device_count.keys().copied().collect();

    let mut observed_count = 0;
    let mut read: BTreeMap<Option<Uuid>, (usize, usize)> = BTreeMap::new();
    for (node, snapshot) in snapshots.iter().filter(|(n, _)| devices.contains(&n.0)) {
        observed_count += 1;
        if let Some(site) = site_of.get(&node.0) {
            let entry = read.entry(*site).or_default();
            entry.0 += 1;
            entry.1 += usize::from(snapshot.truncated);
        }
    }

    let edges: Vec<(Uuid, Option<Uuid>)> = groups.iter().map(|g| (g.id, g.parent_id)).collect();
    // A stored range that does not parse (a `/0`, which is no subnet) covers nothing.
    let prefixes: Vec<(Uuid, SubnetKey)> = groups
        .iter()
        .flat_map(|g| {
            g.prefixes
                .iter()
                .filter_map(move |p| Some((g.id, p.prefix.parse().ok()?)))
        })
        .collect();
    let counted: HashSet<Uuid> = groups
        .iter()
        .map(|g| g.id)
        .filter(|g| scope.allows_group(Some(*g)))
        .collect();
    // Every site against every prefix is pure CPU and grows with the fleet, so it runs off the
    // async workers — the same reason the threshold override count does.
    let compared = {
        let edges = edges.clone();
        tokio::task::spawn_blocking(move || {
            let observed: Vec<(Uuid, &L3Snapshot)> = snapshots
                .iter()
                .filter(|(n, _)| devices.contains(&n.0))
                .map(|(n, s)| (n.0, s))
                .collect();
            by_site(&observed, &site_of, &sites, &edges, &prefixes, |g| {
                counted.contains(&g)
            })
        })
        .await
        .map_err(|e| {
            ApiError::from_internal(&e, "compare prefix gaps", "failed to read prefix gaps")
        })?
    };

    let by_id: HashMap<Uuid, &yagra_base::groups::GroupSummary> =
        groups.iter().map(|g| (g.id, g)).collect();
    let mut out: Vec<SitePrefixGaps> = compared
        .into_iter()
        .map(|(site, c)| {
            let (with, truncated) = read.get(&site).copied().unwrap_or_default();
            let folder = site.and_then(|id| by_id.get(&id));
            let path = site
                .map(|id| {
                    let mut above = yagra_base::groups::group_ancestors(&edges, id);
                    above.reverse();
                    above
                        .into_iter()
                        .filter(|g| scope.allows_group(Some(*g)))
                        .filter_map(|g| by_id.get(&g).map(|f| f.name.clone()))
                        .collect()
                })
                .unwrap_or_default();
            let mut gaps = c.gaps;
            let marked = apply_marks(site, &mut gaps, &marks);
            let status = site_status(with, gaps.len(), marked);
            SitePrefixGaps {
                site_id: site,
                name: folder.map(|f| f.name.clone()),
                path,
                is_site: folder.is_some_and(|f| f.group_type == "site"),
                status,
                nodes_total: count(device_count.get(&site).copied().unwrap_or_default()),
                nodes_with_addresses: count(with),
                nodes_truncated: count(truncated),
                prefixes: count(c.prefixes),
                subnets_checked: count(c.subnets_checked),
                gap_count: count(gaps.len() - marked),
                intentional_count: count(marked),
                gaps,
            }
        })
        .collect();
    out.sort_by(|a, b| {
        b.gap_count
            .cmp(&a.gap_count)
            .then_with(|| a.name.cmp(&b.name))
    });

    Ok(Compared {
        sites: out,
        nodes_total: device_count.values().sum(),
        nodes_with_addresses: observed_count,
        nodes_truncated: read.values().map(|(_, t)| t).sum(),
        groups,
    })
}

#[utoipa::path(
    put, path = "/api/v1/prefix-gaps/acks", tag = "groups",
    request_body = GapAckBody,
    responses(
        (status = 204, description = "The gap is recorded as intentional for this site. The mark applies while the gap keeps the kind it has now; if the subnet's kind changes it is open again"),
        (status = 400, description = "`text_too_long`", body = super::error::ErrorBody),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks ManageConfig, or the token is scoped to some folders (`scope_unsupported`)", body = super::error::ErrorBody),
        (status = 404, description = "`gap_not_found`: the site has no current gap for this subnet", body = super::error::ErrorBody),
        (status = 503, description = "This deployment has no write side (skeleton mode)", body = super::error::ErrorBody),
    ),
)]
async fn set_gap_ack(
    _perm: RequireManageConfig,
    Scoped(scope): Scoped,
    admin: Admin,
    Json(body): Json<GapAckBody>,
) -> ApiResult<StatusCode> {
    require_fleet_wide(&scope, SCOPED_WRITE)?;
    if body.note.chars().count() > NOTE_MAX {
        return Err(ApiError::bad_request(
            "text_too_long",
            format!("note is at most {NOTE_MAX} characters"),
        ));
    }
    // The kind is read from the comparison itself rather than taken from the client, so a mark
    // records what the screen showed. Uncut: a gap past the listing's cap can still be marked.
    let compared = compare(&admin, &scope).await?;
    let kind = compared
        .sites
        .iter()
        .find(|s| s.site_id == body.site_id)
        .and_then(|s| s.gaps.iter().find(|g| g.subnet == body.subnet))
        .map(|g| g.kind)
        .ok_or_else(|| {
            ApiError::not_found(
                "gap_not_found",
                "the site has no current gap for this subnet",
            )
        })?;
    admin
        .repo
        .set_prefix_gap_ack(&StoredGapAck {
            site: body.site_id.unwrap_or(Uuid::nil()),
            subnet: body.subnet,
            kind: kind.token().to_owned(),
            note: body.note.trim().to_owned(),
        })
        .await
        .map_err(|e| ApiError::from_internal(e.as_ref(), "set prefix gap ack", "failed to save"))?;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(
    delete, path = "/api/v1/prefix-gaps/acks", tag = "groups",
    params(GapAckQuery),
    responses(
        (status = 204, description = "The gap is open again"),
        (status = 401, description = "No valid bearer token", body = super::error::ErrorBody),
        (status = 403, description = "Role lacks ManageConfig, or the token is scoped to some folders (`scope_unsupported`)", body = super::error::ErrorBody),
        (status = 404, description = "`ack_not_found`: this gap was not marked as intentional", body = super::error::ErrorBody),
        (status = 503, description = "This deployment has no write side (skeleton mode)", body = super::error::ErrorBody),
    ),
)]
async fn delete_gap_ack(
    _perm: RequireManageConfig,
    Scoped(scope): Scoped,
    admin: Admin,
    Query(q): Query<GapAckQuery>,
) -> ApiResult<StatusCode> {
    require_fleet_wide(&scope, SCOPED_WRITE)?;
    let removed = admin
        .repo
        .delete_prefix_gap_ack(q.site_id.unwrap_or(Uuid::nil()), &q.subnet)
        .await
        .map_err(|e| {
            ApiError::from_internal(e.as_ref(), "delete prefix gap ack", "failed to save")
        })?;
    if removed {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::not_found(
            "ack_not_found",
            "this gap was not marked as intentional",
        ))
    }
}

/// Name the folder each gap's range belongs to, or withhold both when the caller may not see that
/// folder (ADR-014). The subnet keeps its kind either way — calling a claimed subnet
/// `unregistered` would send the operator to create a duplicate.
pub(crate) fn withhold_ranges<'a>(
    gaps: impl IntoIterator<Item = &'a mut PrefixGap>,
    names: &HashMap<Uuid, &str>,
    scope: &NodeScope,
) {
    for gap in gaps {
        match gap.range_group {
            Some(g) if scope.allows_group(Some(g)) => {
                gap.range_group_name = names.get(&g).map(|n| (*n).to_owned());
            }
            Some(_) => {
                gap.range = None;
                gap.range_group = None;
            }
            None => {}
        }
    }
}

/// Fill each listed place's port name from the interface inventory, in one read.
///
/// One row per listed port, through the primary key. Reading every port of every listed node
/// instead (`interface_idents_for`) was ~48 rows per name on a 48-port fleet — about half a million
/// rows for the 10,000 places a fleet-wide answer can list.
pub(crate) async fn fill_port_names(
    admin: &AdminState,
    mut gaps: Vec<&mut PrefixGap>,
) -> ApiResult<()> {
    let listed: Vec<(Uuid, u32)> = gaps
        .iter()
        .flat_map(|g| g.seen_on.iter().map(|s| (s.node_id, s.ifindex)))
        .collect::<BTreeSet<(Uuid, u32)>>()
        .into_iter()
        .collect();
    if listed.is_empty() {
        return Ok(());
    }
    let names = admin.repo.port_names_for(&listed).await.map_err(|e| {
        ApiError::from_internal(
            e.as_ref(),
            "read interface names",
            "failed to read interfaces",
        )
    })?;
    for seen in gaps.iter_mut().flat_map(|g| g.seen_on.iter_mut()) {
        seen.if_name = names.get(&(seen.node_id, seen.ifindex)).cloned();
    }
    Ok(())
}

impl PrefixGapSitesView {
    /// One instance of every field, for the MCP result canary.
    #[cfg(test)]
    pub(crate) fn sample() -> Self {
        use crate::prefix_gaps::{GapKind, SeenOn};
        Self {
            sites: vec![SitePrefixGaps {
                site_id: Some(Uuid::new_v4()),
                name: Some("site-a".to_owned()),
                path: vec!["region".to_owned()],
                is_site: true,
                status: SiteGapStatus::Gaps,
                nodes_total: 2,
                nodes_with_addresses: 1,
                nodes_truncated: 0,
                prefixes: 1,
                subnets_checked: 3,
                gap_count: 1,
                intentional_count: 0,
                gaps: vec![PrefixGap {
                    subnet: "10.1.2.0/24".to_owned(),
                    kind: GapKind::OtherFolder,
                    range: Some("10.1.2.0/24".to_owned()),
                    range_group: Some(Uuid::new_v4()),
                    range_group_name: Some("site-b".to_owned()),
                    node_count: 1,
                    seen_on: vec![SeenOn {
                        node_id: Uuid::new_v4(),
                        ifindex: 3,
                        if_name: Some("Vlan10".to_owned()),
                        ip: "10.1.2.1".to_owned(),
                    }],
                    intentional: Some(GapMark {
                        note: "kept out on purpose".to_owned(),
                    }),
                }],
            }],
            nodes_total: 2,
            nodes_with_addresses: 1,
            nodes_truncated: 0,
            subnets_checked: 3,
            gaps_total: 1,
            gaps_listed: 1,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_noisy_site_cannot_take_every_other_sites_share() {
        // The failure this replaced: the first site took all 2,000 and the rest listed nothing.
        let lens = [3_000, 40, 5, 0, 12];
        let keep = gap_allowance(&lens, SITE_GAPS_MAX);
        assert_eq!(keep.iter().sum::<usize>(), SITE_GAPS_MAX);
        assert_eq!(
            &keep[1..],
            &[40, 5, 0, 12],
            "the small sites are listed whole"
        );
        assert_eq!(
            keep[0],
            SITE_GAPS_MAX - 57,
            "the remainder goes to the large one"
        );
    }

    #[test]
    fn under_the_budget_nothing_is_cut_and_over_it_every_site_gets_a_floor() {
        assert_eq!(gap_allowance(&[7, 0, 3], 2_000), vec![7, 0, 3]);
        let keep = gap_allowance(&[900; 4], 2_000);
        assert_eq!(keep, vec![500; 4], "an equal share when all four are large");
        // More sites than the budget can give ten each: the floor holds down the list and the
        // budget is still the ceiling.
        let keep = gap_allowance(&[50; 300], 2_000);
        assert_eq!(keep.iter().sum::<usize>(), 2_000);
        assert!(keep[..200].iter().all(|k| *k == 10), "{keep:?}");
    }

    fn gap(subnet: &str, kind: crate::prefix_gaps::GapKind) -> PrefixGap {
        PrefixGap {
            subnet: subnet.to_owned(),
            kind,
            range: None,
            range_group: None,
            range_group_name: None,
            node_count: 1,
            seen_on: vec![],
            intentional: None,
        }
    }

    fn mark(site: Uuid, subnet: &str, kind: &str) -> ((Uuid, String), StoredGapAck) {
        (
            (site, subnet.to_owned()),
            StoredGapAck {
                site,
                subnet: subnet.to_owned(),
                kind: kind.to_owned(),
                note: "on purpose".to_owned(),
            },
        )
    }

    /// A mark applies only to its own site and only while the gap keeps the kind it was marked
    /// with; marked gaps go after the unmarked ones without disturbing either half's order.
    #[test]
    fn a_mark_applies_to_its_site_and_kind_and_moves_the_gap_after_the_open_ones() {
        use crate::prefix_gaps::GapKind;
        let site = Uuid::from_u128(7);
        let marks: Marks = [
            mark(site, "10.0.1.0/24", "unregistered"),
            // Marked when it was unregistered; it is partly registered now.
            mark(site, "10.0.2.0/24", "unregistered"),
            // Another site's mark for the same subnet.
            mark(Uuid::from_u128(8), "10.0.3.0/24", "unregistered"),
        ]
        .into_iter()
        .collect();
        let mut gaps = vec![
            gap("10.0.1.0/24", GapKind::Unregistered),
            gap("10.0.3.0/24", GapKind::Unregistered),
            gap("10.0.2.0/24", GapKind::Partial),
        ];
        assert_eq!(apply_marks(Some(site), &mut gaps, &marks), 1);
        let order: Vec<(&str, bool)> = gaps
            .iter()
            .map(|g| (g.subnet.as_str(), g.intentional.is_some()))
            .collect();
        assert_eq!(
            order,
            [
                ("10.0.3.0/24", false),
                ("10.0.2.0/24", false),
                ("10.0.1.0/24", true)
            ]
        );
        assert_eq!(gaps[2].intentional.as_ref().unwrap().note, "on purpose");

        // The root is stored under the nil uuid.
        let root: Marks = [mark(Uuid::nil(), "10.0.1.0/24", "unregistered")]
            .into_iter()
            .collect();
        let mut gaps = vec![gap("10.0.1.0/24", GapKind::Unregistered)];
        assert_eq!(apply_marks(None, &mut gaps, &root), 1);
    }

    #[test]
    fn a_site_whose_every_gap_is_marked_is_intentional_and_never_clean() {
        assert_eq!(site_status(0, 0, 0), SiteGapStatus::NoData);
        assert_eq!(site_status(3, 0, 0), SiteGapStatus::Clean);
        assert_eq!(site_status(3, 2, 2), SiteGapStatus::Intentional);
        assert_eq!(site_status(3, 2, 1), SiteGapStatus::Gaps);
    }

    #[test]
    fn every_status_serializes_to_its_documented_token() {
        let tokens: Vec<String> = SiteGapStatus::ALL
            .iter()
            .map(|s| {
                serde_json::to_value(s)
                    .unwrap()
                    .as_str()
                    .unwrap()
                    .to_owned()
            })
            .collect();
        assert_eq!(tokens, ["gaps", "clean", "intentional", "no_data"]);
    }

    /// The screen's tabs are built from a TS copy of this enum; a status added here and not there
    /// would be a site no tab shows.
    #[test]
    fn the_webuis_status_list_is_this_enum_in_order() {
        let ts = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../web/src/pages/missingPrefixes.ts"
        ))
        .expect("read missingPrefixes.ts");
        let line = ts
            .lines()
            .find(|l| l.starts_with("export const SITE_GAP_STATUSES"))
            .expect("SITE_GAP_STATUSES is declared on one line");
        let quoted: Vec<&str> = line.split('\'').skip(1).step_by(2).collect();
        let tokens: Vec<String> = SiteGapStatus::ALL
            .iter()
            .map(|s| {
                serde_json::to_value(s)
                    .unwrap()
                    .as_str()
                    .unwrap()
                    .to_owned()
            })
            .collect();
        assert_eq!(quoted, tokens);
    }

    /// 🚨 The read is **answered** for every site at once: each site is compared against its own
    /// folder, a site whose devices reported nothing is `no_data` rather than clean, and a scoped
    /// caller is listed only its own site and is not told whose range a claimed subnet falls in.
    #[sqlx::test(migrator = "yagra_base::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn every_sites_missing_subnets_are_listed_and_a_scoped_caller_sees_its_own(
        pool: sqlx::PgPool,
    ) {
        use crate::api::tests_support::{live_state, scoped_token, send, token};
        let st = live_state(pool.clone()).await;
        let tok = token(&st, yagra_common::Role::Admin);
        let folder = |name: &'static str, kind: &'static str, parent: Option<Uuid>| {
            let st = st.clone();
            let tok = tok.clone();
            async move {
                let (_, body) = send(
                    &st,
                    "POST",
                    "/api/v1/node-groups",
                    &tok,
                    Some(serde_json::json!({ "name": name, "group_type": kind, "parent_id": parent })),
                )
                .await;
                body["id"]
                    .as_str()
                    .expect("id")
                    .parse::<Uuid>()
                    .expect("uuid")
            }
        };
        let region = folder("region", "region", None).await;
        let site_a = folder("site-a", "site", Some(region)).await;
        let floor = folder("site-a-floor", "generic", Some(site_a)).await;
        let site_b = folder("site-b", "site", Some(region)).await;
        let site_c = folder("site-c", "site", Some(region)).await;
        yagra_base::pgtest::prefix(&pool, region, "10.1.0.0/16").await;
        yagra_base::pgtest::prefix(&pool, site_a, "10.1.1.0/24").await;
        yagra_base::pgtest::prefix(&pool, site_b, "10.9.0.0/24").await;

        let l3 = crate::l3::L3Repo::new(pool.clone());
        let snap = |rows: &[(u32, &str, u8)]| {
            yagra_common::L3Snapshot::new(
                rows.iter()
                    .map(|(i, ip, len)| {
                        yagra_common::L3Address::new(*i, ip.parse().expect("ip"), *len)
                    })
                    .collect(),
            )
        };
        let a = yagra_base::pgtest::node(&pool, "cs-a", 1, Some(floor)).await;
        let b = yagra_base::pgtest::node(&pool, "cs-b", 2, Some(site_b)).await;
        // Filed in site-c, never walked: the site is listed, and not as complete.
        yagra_base::pgtest::node(&pool, "ping-only", 3, Some(site_c)).await;
        l3.record_observation(
            a,
            &snap(&[
                (1, "10.1.1.1", 24),  // the site's own
                (2, "10.9.0.5", 24),  // site-b's range
                (3, "10.1.3.1", 24),  // only the region's /16
                (4, "192.0.2.1", 24), // nothing anywhere
            ]),
        )
        .await
        .expect("record a");
        l3.record_observation(b, &snap(&[(1, "10.9.0.6", 24)]))
            .await
            .expect("record b");

        let (status, body) = send(&st, "GET", "/api/v1/prefix-gaps", &tok, None).await;
        assert_eq!(status, axum::http::StatusCode::OK, "{body}");
        let sites = body["sites"].as_array().expect("sites");
        let names: Vec<&str> = sites
            .iter()
            .map(|s| s["name"].as_str().expect("name"))
            .collect();
        assert_eq!(
            names,
            ["site-a", "site-b", "site-c"],
            "most gaps first: {body}"
        );
        let first = &sites[0];
        assert_eq!(first["status"], "gaps", "{body}");
        assert_eq!(first["is_site"], true);
        assert_eq!(first["path"], serde_json::json!(["region"]));
        assert_eq!(first["gap_count"], 3, "{body}");
        let kinds: Vec<(&str, &str)> = first["gaps"]
            .as_array()
            .expect("gaps")
            .iter()
            .map(|g| (g["subnet"].as_str().unwrap(), g["kind"].as_str().unwrap()))
            .collect();
        assert_eq!(
            kinds,
            [
                ("192.0.2.0/24", "unregistered"),
                ("10.9.0.0/24", "other_folder"),
                ("10.1.3.0/24", "parent_only"),
            ],
            "{body}"
        );
        assert_eq!(first["gaps"][1]["range_group_name"], "site-b");
        assert_eq!(sites[1]["status"], "clean", "{body}");
        assert_eq!(sites[2]["status"], "no_data", "{body}");
        assert_eq!(body["nodes_total"], 3, "{body}");
        assert_eq!(body["nodes_with_addresses"], 2, "{body}");
        assert_eq!(body["gaps_total"], 3, "{body}");
        assert_eq!(body["gaps_listed"], 3, "{body}");

        let scoped = scoped_token(&st, &[site_a]);
        let (status, body) = send(&st, "GET", "/api/v1/prefix-gaps", &scoped, None).await;
        assert_eq!(status, axum::http::StatusCode::OK, "{body}");
        let sites = body["sites"].as_array().expect("sites");
        assert_eq!(sites.len(), 1, "only its own site: {body}");
        assert_eq!(sites[0]["name"], "site-a");
        assert_eq!(
            sites[0]["path"],
            serde_json::json!([]),
            "the region is not its to see"
        );
        let claimed = sites[0]["gaps"]
            .as_array()
            .expect("gaps")
            .iter()
            .find(|g| g["kind"] == "other_folder")
            .cloned()
            .expect("still reported as claimed");
        assert!(claimed["range"].is_null(), "{claimed}");
        assert!(claimed["range_group_name"].is_null(), "{claimed}");
        assert_eq!(body["nodes_total"], 1, "{body}");

        // A scoped token may not mark a gap: the mark silences it for everyone.
        let mark =
            serde_json::json!({ "site_id": site_a, "subnet": "192.0.2.0/24", "note": " lab " });
        let (status, _) = send(&st, "PUT", ACKS, &scoped, Some(mark.clone())).await;
        assert_eq!(status, axum::http::StatusCode::FORBIDDEN);

        // Marking is ACCEPTED, moves the gap after the open ones and counts it apart.
        let (status, body) = send(&st, "PUT", ACKS, &tok, Some(mark)).await;
        assert_eq!(status, axum::http::StatusCode::NO_CONTENT, "{body}");
        let (_, body) = send(&st, "GET", "/api/v1/prefix-gaps", &tok, None).await;
        let first = &body["sites"][0];
        assert_eq!(first["name"], "site-a", "{body}");
        assert_eq!(first["gap_count"], 2, "{body}");
        assert_eq!(first["intentional_count"], 1, "{body}");
        assert_eq!(first["status"], "gaps", "{body}");
        let last = &first["gaps"][2];
        assert_eq!(last["subnet"], "192.0.2.0/24", "{body}");
        assert_eq!(last["intentional"]["note"], "lab", "{body}");
        assert!(first["gaps"][0]["intentional"].is_null(), "{body}");
        assert_eq!(body["gaps_total"], 3, "marked gaps still count: {body}");

        // A subnet the site does not miss cannot be marked.
        let (status, body) = send(
            &st,
            "PUT",
            ACKS,
            &tok,
            Some(serde_json::json!({ "site_id": site_a, "subnet": "10.1.1.0/24" })),
        )
        .await;
        assert_eq!(status, axum::http::StatusCode::NOT_FOUND, "{body}");
        assert_eq!(body["error"]["code"], "gap_not_found", "{body}");

        // A note longer than the overlap screen's limit is refused before anything is compared.
        let long = "x".repeat(NOTE_MAX + 1);
        let (status, body) = send(
            &st,
            "PUT",
            ACKS,
            &tok,
            Some(serde_json::json!({ "site_id": site_a, "subnet": "10.1.3.0/24", "note": long })),
        )
        .await;
        assert_eq!(status, axum::http::StatusCode::BAD_REQUEST, "{body}");
        assert_eq!(body["error"]["code"], "text_too_long", "{body}");

        // Taking it back is ACCEPTED once, and the second time there is nothing to take back.
        let undo = format!("{ACKS}?site_id={site_a}&subnet=192.0.2.0/24");
        let (status, body) = send(&st, "DELETE", &undo, &tok, None).await;
        assert_eq!(status, axum::http::StatusCode::NO_CONTENT, "{body}");
        let (_, body) = send(&st, "GET", "/api/v1/prefix-gaps", &tok, None).await;
        assert_eq!(body["sites"][0]["intentional_count"], 0, "{body}");
        let (status, _) = send(&st, "DELETE", &undo, &tok, None).await;
        assert_eq!(status, axum::http::StatusCode::NOT_FOUND);
    }

    const ACKS: &str = "/api/v1/prefix-gaps/acks";
}
