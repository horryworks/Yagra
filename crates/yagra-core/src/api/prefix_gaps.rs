// SPDX-License-Identifier: AGPL-3.0-only
//! Nodes ▸ Missing IP prefixes (ADR-170 Inc.2) — for every site, the subnets its devices carry that
//! none of its IP prefixes covers.
//!
//! One read. It is the folder pane's comparison (`GET /node-groups/{id}/prefix-gaps`) run once per
//! site over a single read of every store, so the fleet costs one pass rather than one per site —
//! which is also why the per-folder 2,000-device refusal does not apply here (ADR-170 decision 13).
//! The answer is cut at [`SITE_GAPS_MAX`] gaps instead, and says how many there were.
//!
//! What a scoped caller is told follows the folder pane exactly, and the two share the code that
//! decides it ([`withhold_ranges`], [`fill_port_names`]): only the sites of devices it can see are
//! listed, and a range belonging to a folder it cannot see is withheld while the subnet is still
//! reported as claimed (ADR-014).

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use axum::{routing::get, Json, Router};
use serde::Serialize;
use uuid::Uuid;

use super::error::{ApiError, ApiResult};
use super::extract::{Admin, RequireView, Scoped};
use super::scope::NodeScope;
use super::{AdminState, ApiState};
use crate::prefix_gaps::{by_site, PrefixGap};
use yagra_common::{L3Snapshot, NodeKind, SubnetKey};

/// The most gaps one answer lists, across every site. Each site's `gap_count` is never capped.
pub(crate) const SITE_GAPS_MAX: usize = 2_000;

/// This domain's slice of the OpenAPI document (ADR-035), merged by [`super::openapi::document`].
#[derive(utoipa::OpenApi)]
#[openapi(paths(get_site_prefix_gaps))]
pub(super) struct Doc;

/// The missing-prefix route, merged into `/api/v1` by [`super::router`].
pub(super) fn routes() -> Router<ApiState> {
    Router::new().route("/api/v1/prefix-gaps", get(get_site_prefix_gaps))
}

/// Where one site stands — the screen's three tabs, in their order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub(crate) enum SiteGapStatus {
    /// At least one subnet is missing from the site's IP prefixes.
    Gaps,
    /// Every subnet compared is covered.
    Clean,
    /// None of the site's devices has reported an address, so nothing was compared. Never read
    /// as complete (ADR-170 decision 6).
    NoData,
}

impl SiteGapStatus {
    /// Every status, for the token test and the WebUI's mirror.
    #[cfg(test)]
    const ALL: [SiteGapStatus; 3] = [Self::Gaps, Self::Clean, Self::NoData];
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
    /// How many gaps the site has. `gaps` may list fewer when the answer was cut.
    pub(crate) gap_count: u32,
    /// Ordered by kind, then subnet — the folder pane's order.
    pub(crate) gaps: Vec<PrefixGap>,
}

/// What Nodes ▸ Missing IP prefixes shows.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub(crate) struct PrefixGapSitesView {
    /// Every site holding a device this caller may see. Most gaps first, then by name.
    pub(crate) sites: Vec<SitePrefixGaps>,
    /// Devices across those sites.
    pub(crate) nodes_total: u32,
    /// Of those, how many have reported their addresses at all. No gaps is complete only when
    /// this equals `nodes_total`.
    pub(crate) nodes_with_addresses: u32,
    pub(crate) nodes_truncated: u32,
    /// Subnets compared, summed over sites — one carried at two sites counts twice.
    pub(crate) subnets_checked: u32,
    /// Gaps across every site.
    pub(crate) gaps_total: u32,
    /// Gaps listed in `sites`. Less than `gaps_total` when the answer was cut at 2,000.
    pub(crate) gaps_listed: u32,
}

#[utoipa::path(
    get, path = "/api/v1/prefix-gaps", tag = "groups",
    responses(
        (status = 200, description = "For every site holding a device this caller may see, the subnets its devices carry that none of the IP prefixes filed in the site's folder or beneath it contains, with why each is reported. A site is the nearest folder of type Site above a device, else the device's own folder; devices filed in no folder are one site", body = PrefixGapSitesView),
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

fn count(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

/// Every site's gaps, as this caller may see them — the seam REST and the MCP
/// `get_site_prefix_gaps` tool share, so the two cannot disclose differently.
pub(crate) async fn site_gaps_view(
    admin: &AdminState,
    scope: &NodeScope,
) -> ApiResult<PrefixGapSitesView> {
    let (groups, nodes, snapshots) = tokio::try_join!(
        admin.groups.list(),
        admin.repo.node_folders(),
        admin.l3.all_current(),
    )
    .map_err(|e| internal(&e, "read prefix gap inputs"))?;

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

    let observed: Vec<(Uuid, &L3Snapshot)> = snapshots
        .iter()
        .filter(|(n, _)| devices.contains(&n.0))
        .map(|(n, s)| (n.0, s))
        .collect();
    let mut read: BTreeMap<Option<Uuid>, (usize, usize)> = BTreeMap::new();
    for (node, snapshot) in &observed {
        if let Some(site) = site_of.get(node) {
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
    let compared = by_site(&observed, &site_of, &sites, &edges, &prefixes, |g| {
        scope.allows_group(Some(g))
    });

    let by_id: HashMap<Uuid, &crate::groups::GroupSummary> =
        groups.iter().map(|g| (g.id, g)).collect();
    let mut out: Vec<SitePrefixGaps> = compared
        .into_iter()
        .map(|(site, c)| {
            let (with, truncated) = read.get(&site).copied().unwrap_or_default();
            let folder = site.and_then(|id| by_id.get(&id));
            let path = site
                .map(|id| {
                    let mut above = crate::groups::group_ancestors(&edges, id);
                    above.reverse();
                    above
                        .into_iter()
                        .filter(|g| scope.allows_group(Some(*g)))
                        .filter_map(|g| by_id.get(&g).map(|f| f.name.clone()))
                        .collect()
                })
                .unwrap_or_default();
            let status = if with == 0 {
                SiteGapStatus::NoData
            } else if c.gaps.is_empty() {
                SiteGapStatus::Clean
            } else {
                SiteGapStatus::Gaps
            };
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
                gap_count: count(c.gaps.len()),
                gaps: c.gaps,
            }
        })
        .collect();
    out.sort_by(|a, b| {
        b.gap_count
            .cmp(&a.gap_count)
            .then_with(|| a.name.cmp(&b.name))
    });

    let gaps_total: usize = out.iter().map(|s| s.gaps.len()).sum();
    let mut budget = SITE_GAPS_MAX;
    for site in &mut out {
        site.gaps.truncate(budget);
        budget -= site.gaps.len();
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
        nodes_total: count(device_count.values().sum()),
        nodes_with_addresses: count(observed.len()),
        nodes_truncated: count(read.values().map(|(_, t)| t).sum()),
        subnets_checked: count(out.iter().map(|s| s.subnets_checked as usize).sum()),
        gaps_total: count(gaps_total),
        gaps_listed: count(out.iter().map(|s| s.gaps.len()).sum()),
        sites: out,
    })
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
pub(crate) async fn fill_port_names(
    admin: &AdminState,
    mut gaps: Vec<&mut PrefixGap>,
) -> ApiResult<()> {
    let listed: Vec<Uuid> = gaps
        .iter()
        .flat_map(|g| g.seen_on.iter().map(|s| s.node_id))
        .collect::<BTreeSet<Uuid>>()
        .into_iter()
        .collect();
    if listed.is_empty() {
        return Ok(());
    }
    let idents = admin
        .repo
        .interface_idents_for(&listed)
        .await
        .map_err(|e| {
            ApiError::from_internal(
                e.as_ref(),
                "read interface names",
                "failed to read interfaces",
            )
        })?;
    for seen in gaps.iter_mut().flat_map(|g| g.seen_on.iter_mut()) {
        let key = (
            seen.node_id,
            i32::try_from(seen.ifindex).unwrap_or(i32::MAX),
        );
        seen.if_name = idents.get(&key).and_then(|i| i.if_name.clone());
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
        assert_eq!(tokens, ["gaps", "clean", "no_data"]);
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
    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
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
        crate::pgtest::prefix(&pool, region, "10.1.0.0/16").await;
        crate::pgtest::prefix(&pool, site_a, "10.1.1.0/24").await;
        crate::pgtest::prefix(&pool, site_b, "10.9.0.0/24").await;

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
        let a = crate::pgtest::node(&pool, "cs-a", 1, Some(floor)).await;
        let b = crate::pgtest::node(&pool, "cs-b", 2, Some(site_b)).await;
        // Filed in site-c, never walked: the site is listed, and not as complete.
        crate::pgtest::node(&pool, "ping-only", 3, Some(site_c)).await;
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
    }
}
