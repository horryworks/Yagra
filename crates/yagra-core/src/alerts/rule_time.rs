// SPDX-License-Identifier: AGPL-3.0-only
//! When a routing rule is open, and where an alert goes now (ADR-262 decisions 3, 4 and 9).
//!
//! A rule's window is read in the local time of the node an alert is about: its nearest folder's
//! zone, else the deployment's default (ADR-226 decision 5, kept). Where an alert goes is one
//! judgement, [`route_at`], shared by the notifier (who to send to, or whether to hold) and the
//! screens that say "goes to NOC mail until 17:20" beside a site's clock, so the two cannot
//! disagree. Pure: the zones are handed in by the refresh that reads them.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use chrono::{DateTime, Datelike, Duration, Timelike, Utc};
use uuid::Uuid;
use yagra_base::groups::DeliveryNow;
use yagra_base::wall_clock::Tz;
use yagra_common::NodeId;

use crate::alerts::notifications::{RoutingRule, WhenMode};

/// How far ahead [`next_change`] looks: a week and a day finds every weekly window's edge.
const LOOKAHEAD_MINUTES: i64 = 8 * 24 * 60;

/// Each node's zone, for reading a rule's window in local time. A node absent from it, and an alert
/// about no node at all, keep the deployment's default zone.
#[derive(Debug, Clone)]
pub struct RuleClock {
    zones: HashMap<NodeId, Tz>,
    default: Tz,
}

impl Default for RuleClock {
    fn default() -> Self {
        Self {
            zones: HashMap::new(),
            default: yagra_base::wall_clock::zone_or_utc(None),
        }
    }
}

impl RuleClock {
    #[must_use]
    pub fn new(zones: HashMap<NodeId, Tz>, default: Tz) -> Self {
        Self { zones, default }
    }

    /// The zone `node` keeps time in.
    #[must_use]
    pub fn zone_of(&self, node: Option<NodeId>) -> &Tz {
        node.and_then(|n| self.zones.get(&n))
            .unwrap_or(&self.default)
    }

    /// ISO weekday and minute of the day at `now`, in `node`'s zone.
    fn local(&self, node: Option<NodeId>, now: DateTime<Utc>) -> (u8, u16) {
        let local = yagra_base::wall_clock::to_local(self.zone_of(node), now);
        // `number_from_monday` is 1..=7 and the minute of the day is below 1440, so both fit.
        let day = u8::try_from(local.weekday().number_from_monday()).unwrap_or(1);
        let minute = u16::try_from(local.hour() * 60 + local.minute()).unwrap_or(0);
        (day, minute)
    }

    /// Whether `rule` is open at `now` for an alert about `node`.
    #[cfg(test)]
    #[must_use]
    pub fn is_open(&self, rule: &RoutingRule, node: Option<NodeId>, now: DateTime<Utc>) -> bool {
        if rule.when.mode == WhenMode::Always {
            return true;
        }
        let (day, minute) = self.local(node, now);
        rule.when.open_at(day, minute)
    }
}

/// Where an alert goes at one instant (ADR-262 decision 4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RouteNow {
    /// To the channels of the matching rules that are open now.
    Rules(BTreeSet<Uuid>),
    /// No matching rule is open (or none matches): to the catch-all's channels.
    CatchAll(BTreeSet<Uuid>),
    /// Matching rules exist, none is open, and there is no catch-all: kept until one opens.
    Held,
    /// No rule matches and there is no catch-all: to no channel (the environment's default route
    /// still receives it, as it receives everything).
    Nowhere,
}

impl RouteNow {
    /// The channels it goes to now; empty when held or going nowhere.
    #[must_use]
    pub fn channels(&self) -> BTreeSet<Uuid> {
        match self {
            Self::Rules(c) | Self::CatchAll(c) => c.clone(),
            Self::Held | Self::Nowhere => BTreeSet::new(),
        }
    }
}

/// Where an alert about `node` goes at `now`, given the rules that match it (severity and target
/// already decided) and the catch-all's channels.
///
/// An open rule naming no channel sends nowhere, so it does not stop the catch-all: what counts is
/// whether some channel would hear the alert.
#[must_use]
pub fn route_at(
    matched: &[&RoutingRule],
    catch_all: &[Uuid],
    clock: &RuleClock,
    node: Option<NodeId>,
    now: DateTime<Utc>,
) -> RouteNow {
    let (day, minute) = clock.local(node, now);
    route_local(matched, catch_all, day, minute)
}

/// [`route_at`] at minute `minute` of ISO weekday `day`, local time: the one judgement, which the
/// weekly coverage (ADR-262 decision 8) asks a slot at a time.
#[must_use]
pub fn route_local(matched: &[&RoutingRule], catch_all: &[Uuid], day: u8, minute: u16) -> RouteNow {
    let open: BTreeSet<Uuid> = matched
        .iter()
        .filter(|r| r.when.open_at(day, minute))
        .flat_map(|r| r.channel_ids.iter().copied())
        .collect();
    if !open.is_empty() {
        return RouteNow::Rules(open);
    }
    if !catch_all.is_empty() {
        return RouteNow::CatchAll(catch_all.iter().copied().collect());
    }
    if matched.iter().any(|r| !r.channel_ids.is_empty()) {
        RouteNow::Held
    } else {
        RouteNow::Nowhere
    }
}

/// The first minute after `now` at which [`route_at`] answers differently, or `None` if it does
/// not change within the look-ahead (every rule always open, say). A minute at a time through the
/// zone, so a daylight-saving change is honoured without a rule of its own.
#[must_use]
pub fn next_change(
    matched: &[&RoutingRule],
    catch_all: &[Uuid],
    clock: &RuleClock,
    node: Option<NodeId>,
    now: DateTime<Utc>,
) -> Option<DateTime<Utc>> {
    if matched.iter().all(|r| r.when.mode == WhenMode::Always) {
        return None;
    }
    let here = route_at(matched, catch_all, clock, node, now);
    let start = now
        .with_second(0)
        .and_then(|t| t.with_nanosecond(0))
        .unwrap_or(now);
    (1..=LOOKAHEAD_MINUTES)
        .map(|m| start + Duration::minutes(m))
        .find(|t| route_at(matched, catch_all, clock, node, *t) != here)
}

/// The zone each node keeps time in: its nearest folder's, else `default`. `nodes` is every node's
/// own folder; `folders` is `(group, parent, own zone)` for every folder. A zone name this build
/// does not know falls to `default` rather than to UTC.
#[must_use]
pub fn build_clock(
    nodes: &[(NodeId, Option<Uuid>)],
    folders: &[(Uuid, Option<Uuid>, Option<String>)],
    default: Tz,
) -> RuleClock {
    let zones_by_folder = yagra_base::groups::resolve_nearest_ancestor(folders.iter().cloned());
    let parsed: HashMap<Uuid, Tz> = zones_by_folder
        .into_iter()
        .filter_map(|(g, (name, _))| Some((g, yagra_base::wall_clock::parse_zone(&name)?)))
        .collect();
    let zones = nodes
        .iter()
        .filter_map(|(n, g)| Some((*n, parsed.get(&(*g)?)?.clone())))
        .collect();
    RuleClock::new(zones, default)
}

/// How long one slot of the weekly coverage is, in minutes. A rule's window starts and ends on a
/// minute, so a coarser slot can blur an edge by up to this much; the strip is a picture of the
/// week, and the notifier itself judges by the minute.
pub const COVERAGE_SLOT_MINUTES: u16 = 15;

/// How many of a row's nodes are named.
pub const COVERAGE_NODES_SHOWN: usize = 50;

/// One stretch of the week in which an alert goes to the same place (ADR-262 decision 8). Minutes
/// are counted from Monday 00:00 local time.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, utoipa::ToSchema)]
pub struct CoverageSegment {
    pub start: u16,
    pub end: u16,
    pub kind: CoverageKind,
    /// The channels it goes to; empty when held or going nowhere.
    pub channel_ids: Vec<Uuid>,
}

/// Where an alert goes during a [`CoverageSegment`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum CoverageKind {
    Rules,
    CatchAll,
    Held,
    Nowhere,
}

/// The nodes one set of matching rules reaches, and where their alerts go across the week.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, utoipa::ToSchema)]
pub struct CoverageRow {
    /// The enabled rules that match these nodes at the asked severity, in rule order; empty when
    /// none does.
    pub rule_ids: Vec<Uuid>,
    /// The zone these nodes keep time in; `null` for the deployment's default.
    pub zone: Option<String>,
    pub node_count: usize,
    /// Up to fifty of them, for naming.
    pub node_ids: Vec<Uuid>,
    pub segments: Vec<CoverageSegment>,
}

/// One node as [`coverage`] needs it: its folder, its effective tags and its zone's name.
pub struct CoverageNode {
    pub id: Uuid,
    pub group: Option<Uuid>,
    pub tags: Vec<String>,
    pub zone: Option<String>,
}

/// The week of one set of matching rules, slot by slot, merged into stretches.
#[must_use]
pub fn week_of(matched: &[&RoutingRule], catch_all: &[Uuid]) -> Vec<CoverageSegment> {
    let mut out: Vec<CoverageSegment> = Vec::new();
    for day in 1..=7u8 {
        for slot in 0..(1440 / COVERAGE_SLOT_MINUTES) {
            let minute = slot * COVERAGE_SLOT_MINUTES;
            let start = u16::from(day - 1) * 1440 + minute;
            let route = route_local(matched, catch_all, day, minute);
            let kind = match route {
                RouteNow::Rules(_) => CoverageKind::Rules,
                RouteNow::CatchAll(_) => CoverageKind::CatchAll,
                RouteNow::Held => CoverageKind::Held,
                RouteNow::Nowhere => CoverageKind::Nowhere,
            };
            let channel_ids: Vec<Uuid> = route.channels().into_iter().collect();
            match out.last_mut() {
                Some(last) if last.kind == kind && last.channel_ids == channel_ids => {
                    last.end = start + COVERAGE_SLOT_MINUTES;
                }
                _ => out.push(CoverageSegment {
                    start,
                    end: start + COVERAGE_SLOT_MINUTES,
                    kind,
                    channel_ids,
                }),
            }
        }
    }
    out
}

/// Group every node by the rules that match it at `severity` and the zone it keeps time in, and
/// draw each group's week (ADR-262 decision 8). Neither a folder nor a rule can draw a gap, since a
/// gap is where no rule reaches; a group of nodes that share their rules can. Largest group first.
#[must_use]
pub fn coverage(
    rules: &[RoutingRule],
    catch_all: &[Uuid],
    severity: yagra_common::Severity,
    nodes: &[CoverageNode],
    edges: &[(Uuid, Option<Uuid>)],
) -> Vec<CoverageRow> {
    let live: Vec<&RoutingRule> = rules
        .iter()
        .filter(|r| r.enabled && r.severity.is_none_or(|s| s == severity))
        .collect();
    let mut chains: HashMap<Uuid, Vec<Uuid>> = HashMap::new();
    let mut groups: BTreeMap<(Vec<Uuid>, Option<String>), Vec<Uuid>> = BTreeMap::new();
    for n in nodes {
        let chain: Vec<Uuid> = match n.group {
            Some(g) => chains
                .entry(g)
                .or_insert_with(|| {
                    let mut c = vec![g];
                    c.extend(yagra_base::groups::group_ancestors(edges, g));
                    c
                })
                .clone(),
            None => Vec::new(),
        };
        let ids: Vec<Uuid> = live
            .iter()
            .filter(|r| {
                crate::alerts::applies_to::reaches_chain(
                    &r.group_ids,
                    &r.tags,
                    r.join,
                    &chain,
                    &n.tags,
                )
            })
            .map(|r| r.id)
            .collect();
        groups.entry((ids, n.zone.clone())).or_default().push(n.id);
    }
    let mut rows: Vec<CoverageRow> = groups
        .into_iter()
        .map(|((rule_ids, zone), members)| {
            let matched: Vec<&RoutingRule> = live
                .iter()
                .copied()
                .filter(|r| rule_ids.contains(&r.id))
                .collect();
            CoverageRow {
                segments: week_of(&matched, catch_all),
                rule_ids,
                zone,
                node_count: members.len(),
                node_ids: members.into_iter().take(COVERAGE_NODES_SHOWN).collect(),
            }
        })
        .collect();
    rows.sort_by_key(|r| std::cmp::Reverse(r.node_count));
    rows
}

/// What the screens need to say where an alert about one place goes now (ADR-262 decision 9),
/// read once per request: the enabled rules, the catch-all, the channels' names, the folder tree
/// and the zones.
pub struct PlaceRoutes {
    pub rules: Vec<RoutingRule>,
    pub catch_all: Vec<Uuid>,
    pub names: HashMap<Uuid, String>,
    pub edges: Vec<(Uuid, Option<Uuid>)>,
    /// Each folder's zone after inheritance, by folder, with the folder that set it.
    pub zones: HashMap<Uuid, (Tz, Uuid)>,
    pub default: Tz,
}

impl PlaceRoutes {
    /// Where a critical alert about a node filed in `group` and carrying `tags` goes at `now`, and
    /// until when; `None` when it goes to no channel. Critical, because that is the alert a person
    /// is woken for: a rule written for warnings only does not change what this line says.
    #[must_use]
    pub fn at(&self, group: Uuid, tags: &[String], now: DateTime<Utc>) -> Option<DeliveryNow> {
        self.at_memo(group, tags, now, &mut PlaceMemo::new())
    }

    /// [`Self::at`] for many places at once: places with the same matching rules and the same
    /// zone share one judgement, because the judgement steps a minute at a time for up to eight
    /// days and a folder list holds as many folders as a deployment has sites.
    #[must_use]
    pub fn at_memo(
        &self,
        group: Uuid,
        tags: &[String],
        now: DateTime<Utc>,
        memo: &mut PlaceMemo,
    ) -> Option<DeliveryNow> {
        let mut chain = vec![group];
        chain.extend(yagra_base::groups::group_ancestors(&self.edges, group));
        let matched: Vec<&RoutingRule> = self
            .rules
            .iter()
            .filter(|r| {
                r.enabled
                    && r.severity
                        .is_none_or(|s| s == yagra_common::Severity::Critical)
                    && crate::alerts::applies_to::reaches_chain(
                        &r.group_ids,
                        &r.tags,
                        r.join,
                        &chain,
                        tags,
                    )
            })
            .collect();
        let key = (
            matched.iter().map(|r| r.id).collect::<Vec<Uuid>>(),
            self.zones.get(&group).map(|(_, owner)| *owner),
        );
        if let Some(hit) = memo.get(&key) {
            return hit.clone();
        }
        let zone = self
            .zones
            .get(&group)
            .map_or_else(|| self.default.clone(), |(z, _)| z.clone());
        let clock = RuleClock::new(HashMap::new(), zone);
        let route = route_at(&matched, &self.catch_all, &clock, None, now);
        let until = next_change(&matched, &self.catch_all, &clock, None, now);
        let name_of = |id: &Uuid| self.names.get(id).cloned();
        let answer = match route {
            RouteNow::Rules(c) => Some(DeliveryNow {
                open: true,
                closes_at: until,
                opens_at: None,
                channels: c.iter().filter_map(name_of).collect(),
                catch_all: false,
            }),
            RouteNow::CatchAll(c) => Some(DeliveryNow {
                open: true,
                closes_at: until,
                opens_at: None,
                channels: c.iter().filter_map(name_of).collect(),
                catch_all: true,
            }),
            RouteNow::Held => Some(DeliveryNow {
                open: false,
                closes_at: None,
                opens_at: until,
                channels: Vec::new(),
                catch_all: false,
            }),
            RouteNow::Nowhere => None,
        };
        memo.insert(key, answer.clone());
        answer
    }
}

/// What [`PlaceRoutes::at_memo`] has already judged: by the matching rules and the folder whose
/// zone applies.
pub type PlaceMemo = HashMap<(Vec<Uuid>, Option<Uuid>), Option<DeliveryNow>>;

/// Take the routing facts out of an answer for a caller who may not read routing (ADR-057): where
/// an alert goes stays (open, held, until when), which channels does not.
#[must_use]
pub fn without_names(mut d: DeliveryNow) -> DeliveryNow {
    d.channels.clear();
    d.catch_all = false;
    d
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::alerts::notifications::RuleWhen;
    use chrono::TimeZone;

    fn rule(channel: Uuid, mode: WhenMode) -> RoutingRule {
        RoutingRule {
            id: Uuid::new_v4(),
            name: "r".to_owned(),
            enabled: true,
            severity: None,
            tags: Vec::new(),
            group_ids: Vec::new(),
            join: crate::alerts::applies_to::Join::Or,
            channel_ids: vec![channel],
            when: RuleWhen {
                mode,
                days: vec![1, 2, 3, 4, 5],
                start_minute: 8 * 60 + 30,
                end_minute: 17 * 60 + 20,
            },
        }
    }

    /// 2026-11-23 is a Monday. UTC, so the times read as written.
    fn at(day: u32, h: u32, m: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 11, day, h, m, 0)
            .single()
            .expect("time")
    }

    #[test]
    fn a_day_rule_and_a_night_rule_hand_the_alert_over_at_the_window_edge() {
        let (mail, oncall) = (Uuid::new_v4(), Uuid::new_v4());
        let day = rule(mail, WhenMode::During);
        let night = rule(oncall, WhenMode::Outside);
        let clock = RuleClock::default();
        let both = [&day, &night];
        assert_eq!(
            route_at(&both, &[], &clock, None, at(23, 10, 0)),
            RouteNow::Rules([mail].into_iter().collect())
        );
        assert_eq!(
            route_at(&both, &[], &clock, None, at(23, 22, 0)),
            RouteNow::Rules([oncall].into_iter().collect())
        );
        assert_eq!(
            next_change(&both, &[], &clock, None, at(23, 10, 0)),
            Some(at(23, 17, 20)),
            "the day rule closes at 17:20, local"
        );
    }

    #[test]
    fn a_closed_rule_falls_to_the_catch_all_and_without_one_is_held() {
        let (mail, chat) = (Uuid::new_v4(), Uuid::new_v4());
        let day = rule(mail, WhenMode::During);
        let clock = RuleClock::default();
        let saturday = at(28, 10, 0);
        assert_eq!(
            route_at(&[&day], &[chat], &clock, None, saturday),
            RouteNow::CatchAll([chat].into_iter().collect())
        );
        assert_eq!(
            route_at(&[&day], &[], &clock, None, saturday),
            RouteNow::Held
        );
        assert_eq!(
            next_change(&[&day], &[], &clock, None, saturday),
            Some(at(30, 8, 30)),
            "held until Monday 08:30"
        );
    }

    #[test]
    fn no_matching_rule_goes_to_the_catch_all_or_nowhere() {
        let chat = Uuid::new_v4();
        let clock = RuleClock::default();
        assert_eq!(
            route_at(&[], &[chat], &clock, None, at(23, 10, 0)),
            RouteNow::CatchAll([chat].into_iter().collect())
        );
        assert_eq!(
            route_at(&[], &[], &clock, None, at(23, 10, 0)),
            RouteNow::Nowhere
        );
    }

    #[test]
    fn an_always_rule_never_changes_and_a_channel_less_rule_does_not_block_the_catch_all() {
        let (mail, chat) = (Uuid::new_v4(), Uuid::new_v4());
        let always = rule(mail, WhenMode::Always);
        let clock = RuleClock::default();
        assert_eq!(
            next_change(&[&always], &[], &clock, None, at(23, 3, 0)),
            None
        );
        let empty = RoutingRule {
            channel_ids: Vec::new(),
            ..rule(mail, WhenMode::Always)
        };
        assert_eq!(
            route_at(&[&empty], &[chat], &clock, None, at(23, 3, 0)),
            RouteNow::CatchAll([chat].into_iter().collect())
        );
        assert_eq!(
            route_at(&[&empty], &[], &clock, None, at(23, 3, 0)),
            RouteNow::Nowhere
        );
    }

    #[test]
    fn coverage_groups_nodes_by_their_rules_and_draws_each_week() {
        let (mail, oncall) = (Uuid::new_v4(), Uuid::new_v4());
        let (site, other) = (Uuid::new_v4(), Uuid::new_v4());
        let day = RoutingRule {
            group_ids: vec![site],
            join: crate::alerts::applies_to::Join::Or,
            ..rule(mail, WhenMode::During)
        };
        let core = RoutingRule {
            tags: vec!["core".to_owned()],
            severity: Some(yagra_common::Severity::Critical),
            ..rule(oncall, WhenMode::Always)
        };
        let node = |group, tags: &[&str]| CoverageNode {
            id: Uuid::new_v4(),
            group: Some(group),
            tags: tags.iter().map(|t| (*t).to_owned()).collect(),
            zone: None,
        };
        let nodes = [
            node(site, &[]),
            node(site, &[]),
            node(site, &["core"]),
            node(other, &[]),
        ];
        let edges = [(site, None), (other, None)];
        let rules = [day.clone(), core.clone()];
        let rows = coverage(
            &rules,
            &[],
            yagra_common::Severity::Critical,
            &nodes,
            &edges,
        );
        assert_eq!(rows.len(), 3, "site, site + core, and nothing");
        assert_eq!(rows[0].rule_ids, vec![day.id]);
        assert_eq!(rows[0].node_count, 2);
        // Weekdays 08:30-17:20 go to mail; the rest of the week is held (no catch-all).
        assert_eq!(rows[0].segments[0].kind, CoverageKind::Held);
        assert_eq!(rows[0].segments[1].start, 8 * 60 + 30);
        assert_eq!(rows[0].segments[1].channel_ids, vec![mail]);
        let nothing = rows
            .iter()
            .find(|r| r.rule_ids.is_empty())
            .expect("a row for no rule");
        assert_eq!(
            nothing.segments,
            vec![CoverageSegment {
                start: 0,
                end: 7 * 1440,
                kind: CoverageKind::Nowhere,
                channel_ids: Vec::new(),
            }]
        );
        // At warning, the critical-only rule no longer reaches the core node.
        let warn = coverage(&rules, &[], yagra_common::Severity::Warning, &nodes, &edges);
        assert_eq!(warn.len(), 2);
    }

    #[test]
    fn daylight_saving_moves_the_utc_instant_not_the_wall_time() {
        let mail = Uuid::new_v4();
        let day = rule(mail, WhenMode::During);
        let (site, node) = (Uuid::new_v4(), NodeId::new());
        let clock = build_clock(
            &[(node, Some(site))],
            &[(site, None, Some("America/New_York".to_owned()))],
            yagra_base::wall_clock::zone_or_utc(None),
        );
        // 08:30 New York is 12:30 UTC in summer (EDT) and 13:30 UTC in winter (EST).
        let utc = |mo, d, h, mi| {
            Utc.with_ymd_and_hms(2026, mo, d, h, mi, 0)
                .single()
                .expect("t")
        };
        assert!(clock.is_open(&day, Some(node), utc(7, 6, 12, 30)));
        assert!(!clock.is_open(&day, Some(node), utc(12, 7, 12, 30)));
        assert!(clock.is_open(&day, Some(node), utc(12, 7, 13, 30)));
    }

    #[test]
    fn the_window_is_read_in_the_nodes_own_zone() {
        let mail = Uuid::new_v4();
        let day = rule(mail, WhenMode::During);
        let (site, node) = (Uuid::new_v4(), NodeId::new());
        let clock = build_clock(
            &[(node, Some(site))],
            &[(site, None, Some("Asia/Tokyo".to_owned()))],
            yagra_base::wall_clock::zone_or_utc(None),
        );
        // 01:00 UTC Monday is 10:00 in Tokyo: open there, closed in UTC.
        assert!(clock.is_open(&day, Some(node), at(23, 1, 0)));
        assert!(!clock.is_open(&day, None, at(23, 1, 0)));
    }
}
