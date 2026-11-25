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

use crate::alerts::notifications::{RoutingRule, WhenClosed, WhenMode};

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
/// whether some channel would hear the alert. `hears` says which channel ids can: a channel that
/// is switched off (or no longer exists) is not dispatched to, so it counts as no channel at all -
/// otherwise a rule whose only channel is off would keep an alert from the catch-all and from being
/// held, and nobody would hear it.
#[must_use]
pub fn route_at(
    matched: &[&RoutingRule],
    catch_all: &[Uuid],
    hears: Hears<'_>,
    clock: &RuleClock,
    node: Option<NodeId>,
    now: DateTime<Utc>,
) -> RouteNow {
    let (day, minute) = clock.local(node, now);
    route_local(matched, catch_all, hears, day, minute)
}

/// Which channel ids can hear an alert: the ones the notifier would dispatch to.
pub type Hears<'a> = &'a dyn Fn(&Uuid) -> bool;

/// [`route_at`] at minute `minute` of ISO weekday `day`, local time: the one judgement, which the
/// weekly coverage (ADR-262 decision 8) asks a slot at a time.
///
/// `matched` is in rule order, and the first rule in it that is open decides alone (ADR-262
/// decision 14): a holding rule holds, a sending rule sends to its channels, and no rule below it
/// hears the alert. A sending rule no channel of which can hear is passed over, as if it were not
/// there - otherwise switching a channel off would make a rule swallow alerts. A closed rule is
/// passed over too, unless it holds while closed (ADR-266): then it holds the alert there. Only
/// when no rule decides does the catch-all take the alert; without one it is held if a rule could
/// send it later, and goes nowhere if none could.
#[must_use]
pub fn route_local(
    matched: &[&RoutingRule],
    catch_all: &[Uuid],
    hears: Hears<'_>,
    day: u8,
    minute: u16,
) -> RouteNow {
    decide_local(matched, catch_all, hears, day, minute).route
}

/// Where an alert goes at one local minute, and which rule decided (ADR-266 decision 4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Decision {
    pub route: RouteNow,
    /// The rule that decided: an open rule that sends or holds, or a closed rule that holds while
    /// closed. `None` when the catch-all takes the alert, when it is held because no rule is open,
    /// and when it goes nowhere.
    pub rule: Option<Uuid>,
}

/// [`route_local`] with the rule that decided. The first matching rule that is open decides alone
/// (ADR-262 decision 14); a closed rule is passed over unless it holds while closed (ADR-266
/// decision 1), in which case it holds the alert there and no rule below it, nor the catch-all,
/// hears it.
#[must_use]
pub fn decide_local(
    matched: &[&RoutingRule],
    catch_all: &[Uuid],
    hears: Hears<'_>,
    day: u8,
    minute: u16,
) -> Decision {
    for rule in matched {
        if rule.when.open_at(day, minute) {
            if rule.hold {
                return Decision {
                    route: RouteNow::Held,
                    rule: Some(rule.id),
                };
            }
            let heard: BTreeSet<Uuid> = rule
                .channel_ids
                .iter()
                .copied()
                .filter(|c| hears(c))
                .collect();
            if !heard.is_empty() {
                return Decision {
                    route: RouteNow::Rules(heard),
                    rule: Some(rule.id),
                };
            }
        } else if holds_when_closed(rule, hears) {
            return Decision {
                route: RouteNow::Held,
                rule: Some(rule.id),
            };
        }
    }
    let catch_all: BTreeSet<Uuid> = catch_all.iter().copied().filter(|c| hears(c)).collect();
    let route = if !catch_all.is_empty() {
        RouteNow::CatchAll(catch_all)
    } else if matched.iter().any(|r| sends_to_someone(r, hears)) {
        RouteNow::Held
    } else {
        RouteNow::Nowhere
    };
    Decision { route, rule: None }
}

/// Whether `rule` sends, and some channel of it can hear.
fn sends_to_someone(rule: &RoutingRule, hears: Hears<'_>) -> bool {
    !rule.hold && rule.channel_ids.iter().any(hears)
}

/// Whether `rule`, while its hours are closed, keeps an alert back (ADR-266 decision 1). A sending
/// rule no channel of which can hear does not: it is passed over as if it were not there, open or
/// closed, for the reason [`route_local`] gives.
fn holds_when_closed(rule: &RoutingRule, hears: Hears<'_>) -> bool {
    rule.when_closed == WhenClosed::Hold && (rule.hold || sends_to_someone(rule, hears))
}

/// Whether `rule` decides at every minute it matches: it is always open, or it holds while closed,
/// and it holds or a channel of it hears.
fn always_decides(rule: &RoutingRule, hears: Hears<'_>) -> bool {
    match rule.when.mode {
        WhenMode::Always => rule.hold || sends_to_someone(rule, hears),
        WhenMode::During | WhenMode::Outside => holds_when_closed(rule, hears),
    }
}

/// The matching rules that can ever decide, in order: everything down to and including the first
/// rule that decides at every minute (always open, or holding while closed - see
/// [`always_decides`]). A rule below that one never decides, so two places whose rules differ only
/// there go to the same places all week, and the coverage and the clock line share one judgement
/// for them.
#[must_use]
pub fn deciding<'a>(matched: &[&'a RoutingRule], hears: Hears<'_>) -> Vec<&'a RoutingRule> {
    let mut out = Vec::with_capacity(matched.len());
    for rule in matched {
        out.push(*rule);
        if always_decides(rule, hears) {
            break;
        }
    }
    out
}

/// The first minute after `now` at which [`route_at`] answers differently, or `None` if it does
/// not change within the look-ahead (every rule always open, say). A minute at a time through the
/// zone, so a daylight-saving change is honoured without a rule of its own.
#[must_use]
pub fn next_change(
    matched: &[&RoutingRule],
    catch_all: &[Uuid],
    hears: Hears<'_>,
    clock: &RuleClock,
    node: Option<NodeId>,
    now: DateTime<Utc>,
) -> Option<DateTime<Utc>> {
    if matched.iter().all(|r| r.when.mode == WhenMode::Always) {
        return None;
    }
    let here = route_at(matched, catch_all, hears, clock, node, now);
    let start = now
        .with_second(0)
        .and_then(|t| t.with_nanosecond(0))
        .unwrap_or(now);
    (1..=LOOKAHEAD_MINUTES)
        .map(|m| start + Duration::minutes(m))
        .find(|t| route_at(matched, catch_all, hears, clock, node, *t) != here)
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
    /// The rule that decides during the stretch (ADR-266 decision 4): the open rule that sends or
    /// holds, or a closed rule that holds while closed. `null` when the catch-all takes the alert,
    /// when it is held because no rule is open, and when it goes nowhere.
    pub rule_id: Option<Uuid>,
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
    /// The enabled rules that match these nodes at the asked severity and can ever decide where
    /// their alerts go, in rule order (ADR-262 decision 14); empty when none does.
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

/// The week of one set of matching rules, judged every `slot` minutes and merged into stretches.
/// `slot` divides a day (1 and 15 do); a slot that does not is read as one minute. The coverage
/// card takes [`COVERAGE_SLOT_MINUTES`]; the rule dialog's week takes one, so the edge of a rule's
/// hours is drawn where it is (ADR-266 decision 5).
#[must_use]
pub fn week_in_slots(
    matched: &[&RoutingRule],
    catch_all: &[Uuid],
    hears: Hears<'_>,
    slot: u16,
) -> Vec<CoverageSegment> {
    let slot = if slot > 0 && 1440 % slot == 0 {
        slot
    } else {
        1
    };
    let mut out: Vec<CoverageSegment> = Vec::new();
    for day in 1..=7u8 {
        for step in 0..(1440 / slot) {
            let minute = step * slot;
            let start = u16::from(day - 1) * 1440 + minute;
            let Decision { route, rule } = decide_local(matched, catch_all, hears, day, minute);
            let kind = match route {
                RouteNow::Rules(_) => CoverageKind::Rules,
                RouteNow::CatchAll(_) => CoverageKind::CatchAll,
                RouteNow::Held => CoverageKind::Held,
                RouteNow::Nowhere => CoverageKind::Nowhere,
            };
            let channel_ids: Vec<Uuid> = route.channels().into_iter().collect();
            match out.last_mut() {
                Some(last)
                    if last.kind == kind
                        && last.channel_ids == channel_ids
                        && last.rule_id == rule =>
                {
                    last.end = start + slot;
                }
                _ => out.push(CoverageSegment {
                    start,
                    end: start + slot,
                    kind,
                    channel_ids,
                    rule_id: rule,
                }),
            }
        }
    }
    out
}

/// Group every node by the rules that match it at `severity` and can decide (see [`deciding`]) and
/// the zone it keeps time in, and
/// draw each group's week (ADR-262 decision 8). Neither a folder nor a rule can draw a gap, since a
/// gap is where no rule reaches; a group of nodes that share their rules can. Largest group first.
#[must_use]
pub fn coverage(
    rules: &[RoutingRule],
    catch_all: &[Uuid],
    hears: Hears<'_>,
    severity: yagra_common::Severity,
    nodes: &[CoverageNode],
    edges: &[(Uuid, Option<Uuid>)],
) -> Vec<CoverageRow> {
    coverage_in_slots(
        rules,
        catch_all,
        hears,
        severity,
        nodes,
        edges,
        COVERAGE_SLOT_MINUTES,
    )
}

/// [`coverage`] with each week judged every `slot` minutes ([`week_in_slots`]). The rule dialog's
/// week asks for one-minute slots (ADR-266 decision 5).
#[must_use]
pub fn coverage_in_slots(
    rules: &[RoutingRule],
    catch_all: &[Uuid],
    hears: Hears<'_>,
    severity: yagra_common::Severity,
    nodes: &[CoverageNode],
    edges: &[(Uuid, Option<Uuid>)],
    slot: u16,
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
        let reaching: Vec<&RoutingRule> = live
            .iter()
            .copied()
            .filter(|r| {
                crate::alerts::applies_to::reaches_chain(
                    &r.group_ids,
                    &r.tags,
                    r.join,
                    &chain,
                    &n.tags,
                )
            })
            .collect();
        let ids: Vec<Uuid> = deciding(&reaching, hears).iter().map(|r| r.id).collect();
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
                segments: week_in_slots(&matched, catch_all, hears, slot),
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
/// read once per request: every rule (`at_memo` keeps the enabled ones), the catch-all, the channels' names, the folder tree
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
        // A switched-off channel hears nothing (`names` holds the enabled ones only).
        let hears = |id: &Uuid| self.names.contains_key(id);
        let matched = deciding(&matched, &hears);
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
        let route = route_at(&matched, &self.catch_all, &hears, &clock, None, now);
        let until = next_change(&matched, &self.catch_all, &hears, &clock, None, now);
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

    /// Every channel hears: the channels these tests name all exist and are switched on.
    const ALL: fn(&Uuid) -> bool = |_| true;

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
            hold: false,
            when_closed: crate::alerts::notifications::WhenClosed::NextRule,
        }
    }

    /// A rule that holds rather than sends, open `mode` the weekday 08:30-17:20 window.
    fn holding(mode: WhenMode) -> RoutingRule {
        RoutingRule {
            channel_ids: Vec::new(),
            hold: true,
            ..rule(Uuid::new_v4(), mode)
        }
    }

    /// ADR-262 decision 14, the case the user asked for: a holding rule for one site's nights sits
    /// above the rule for everything. At night the site's alert is held and the rule below never
    /// hears it; in the morning the holding rule closes and the same alert goes to mail, which is
    /// what the release of a held fire asks. Another site is not held at all.
    #[test]
    fn a_holding_rule_above_keeps_one_sites_nights_quiet() {
        let mail = Uuid::new_v4();
        let nights = holding(WhenMode::Outside);
        let everything = rule(mail, WhenMode::Always);
        let clock = RuleClock::default();
        let site_x = [&nights, &everything];
        let site_y = [&everything];
        assert_eq!(
            route_at(&site_x, &[], &ALL, &clock, None, at(23, 23, 0)),
            RouteNow::Held
        );
        assert_eq!(
            route_at(&site_x, &[mail], &ALL, &clock, None, at(23, 23, 0)),
            RouteNow::Held,
            "a holding rule keeps the alert from the catch-all too"
        );
        assert_eq!(
            next_change(&site_x, &[], &ALL, &clock, None, at(23, 23, 0)),
            Some(at(24, 8, 30)),
            "held until the holding rule closes, Tuesday 08:30"
        );
        assert_eq!(
            route_at(&site_x, &[], &ALL, &clock, None, at(24, 8, 30)),
            RouteNow::Rules([mail].into_iter().collect())
        );
        assert_eq!(
            route_at(&site_y, &[], &ALL, &clock, None, at(23, 23, 0)),
            RouteNow::Rules([mail].into_iter().collect())
        );
    }

    /// The first open rule decides alone: two rules open at once do not both send, and moving the
    /// other one up moves the alert.
    #[test]
    fn the_first_open_rule_decides_and_the_order_moves_the_alert() {
        let (mail, pager) = (Uuid::new_v4(), Uuid::new_v4());
        let a = rule(mail, WhenMode::Always);
        let b = rule(pager, WhenMode::Always);
        let clock = RuleClock::default();
        assert_eq!(
            route_at(&[&a, &b], &[], &ALL, &clock, None, at(23, 10, 0)),
            RouteNow::Rules([mail].into_iter().collect())
        );
        assert_eq!(
            route_at(&[&b, &a], &[], &ALL, &clock, None, at(23, 10, 0)),
            RouteNow::Rules([pager].into_iter().collect())
        );
        // A closed rule above does not decide: it is skipped, not held for.
        let day = rule(mail, WhenMode::During);
        assert_eq!(
            route_at(&[&day, &b], &[], &ALL, &clock, None, at(23, 23, 0)),
            RouteNow::Rules([pager].into_iter().collect())
        );
    }

    /// A sending rule whose channels are all off is passed over, so switching a channel off cannot
    /// make a rule swallow the alerts below it.
    #[test]
    fn a_sending_rule_nobody_hears_is_passed_over() {
        let (off, pager) = (Uuid::new_v4(), Uuid::new_v4());
        let hears = |id: &Uuid| *id != off;
        let dead = rule(off, WhenMode::Always);
        let live = rule(pager, WhenMode::Always);
        let clock = RuleClock::default();
        assert_eq!(
            route_at(&[&dead, &live], &[], &hears, &clock, None, at(23, 10, 0)),
            RouteNow::Rules([pager].into_iter().collect())
        );
        // Only holding rules match, and none is open: nothing could ever send it.
        let nights = holding(WhenMode::Outside);
        assert_eq!(
            route_at(&[&nights], &[], &ALL, &clock, None, at(23, 10, 0)),
            RouteNow::Nowhere
        );
    }

    /// Rules below the first always-open rule that decides never decide, so they do not split a
    /// coverage row or a clock line.
    #[test]
    fn deciding_stops_at_the_first_always_open_rule_that_decides() {
        let (mail, off) = (Uuid::new_v4(), Uuid::new_v4());
        let hears = |id: &Uuid| *id != off;
        let day = rule(mail, WhenMode::During);
        let dead = rule(off, WhenMode::Always);
        let all = rule(mail, WhenMode::Always);
        let below = rule(mail, WhenMode::Outside);
        let ids = |v: Vec<&RoutingRule>| v.iter().map(|r| r.id).collect::<Vec<_>>();
        assert_eq!(
            ids(deciding(&[&day, &dead, &all, &below], &hears)),
            vec![day.id, dead.id, all.id]
        );
        let hold = holding(WhenMode::Always);
        assert_eq!(ids(deciding(&[&hold, &all], &hears)), vec![hold.id]);
        // A rule that holds while closed decides at every minute, so it cuts the list too
        // (ADR-266); one whose channels are all off does not, as it is passed over open or closed.
        let held_nights = RoutingRule {
            when_closed: WhenClosed::Hold,
            ..rule(mail, WhenMode::During)
        };
        assert_eq!(
            ids(deciding(&[&held_nights, &all], &hears)),
            vec![held_nights.id]
        );
        let dead_nights = RoutingRule {
            when_closed: WhenClosed::Hold,
            ..rule(off, WhenMode::During)
        };
        assert_eq!(
            ids(deciding(&[&dead_nights, &all], &hears)),
            vec![dead_nights.id, all.id]
        );
    }

    /// ADR-266, the case found on a PoC: a weekday-hours rule for the nodes tagged `8x5x5` sits
    /// above an always-open rule that reaches the same nodes by another tag and sends to the same
    /// channels. Passed over when closed (ADR-262 decision 14), the rule below sends the 17:41
    /// alert. Holding while closed keeps it there - from the rule below and from the catch-all -
    /// until the hours open on Thursday at 08:30.
    #[test]
    fn a_rule_that_holds_while_closed_keeps_the_alert_from_the_rules_below() {
        let (jsm, chat) = (Uuid::new_v4(), Uuid::new_v4());
        let office = rule(jsm, WhenMode::During);
        let everything = rule(jsm, WhenMode::Always);
        let clock = RuleClock::default();
        let wed_1741 = at(25, 17, 41);
        // Today: the rule below sends after hours. Same channel, different rule.
        let passed = [&office, &everything];
        assert_eq!(
            decide_local(&passed, &[], &ALL, 3, 17 * 60 + 41),
            Decision {
                route: RouteNow::Rules([jsm].into_iter().collect()),
                rule: Some(everything.id),
            }
        );
        let held_nights = RoutingRule {
            when_closed: WhenClosed::Hold,
            ..office.clone()
        };
        let held = [&held_nights, &everything];
        assert_eq!(
            decide_local(&held, &[chat], &ALL, 3, 17 * 60 + 41),
            Decision {
                route: RouteNow::Held,
                rule: Some(held_nights.id),
            },
            "held at the closed rule: neither the rule below nor the catch-all hears it"
        );
        assert_eq!(
            next_change(&held, &[], &ALL, &clock, None, wed_1741),
            Some(at(26, 8, 30)),
            "held until the hours open, Thursday 08:30"
        );
        assert_eq!(
            route_at(&held, &[], &ALL, &clock, None, at(26, 8, 30)),
            RouteNow::Rules([jsm].into_iter().collect())
        );
        // A weekend is closed all day, so it is held all day.
        assert_eq!(
            route_at(&held, &[], &ALL, &clock, None, at(28, 12, 0)),
            RouteNow::Held
        );
        // A rule nobody can hear is passed over when closed as well as when open.
        let off = |id: &Uuid| *id != jsm;
        assert_eq!(
            decide_local(&held, &[chat], &off, 3, 17 * 60 + 41),
            Decision {
                route: RouteNow::CatchAll([chat].into_iter().collect()),
                rule: None,
            }
        );
        // A rule that is always open has no closed hours to hold in.
        let always_held = RoutingRule {
            when_closed: WhenClosed::Hold,
            ..everything.clone()
        };
        assert_eq!(
            route_at(&[&always_held], &[], &ALL, &clock, None, wed_1741),
            RouteNow::Rules([jsm].into_iter().collect())
        );
    }

    /// The dialog's week is judged every minute and split by the rule that decides, so the 17:20
    /// edge between two rules sending to the same channel is drawn where it is (ADR-266 decisions 4
    /// and 5). The coverage card's 15-minute week cannot draw it at 17:20.
    #[test]
    fn a_minute_week_splits_where_the_deciding_rule_changes() {
        let jsm = Uuid::new_v4();
        let office = rule(jsm, WhenMode::During);
        let everything = rule(jsm, WhenMode::Always);
        let matched = [&office, &everything];
        let week = week_in_slots(&matched, &[], &ALL, 1);
        let (from, to) = (2 * 1440, 3 * 1440);
        let wednesday: Vec<(u16, u16, Option<Uuid>)> = week
            .iter()
            .filter(|s| s.start < to && s.end > from)
            .map(|s| (s.start.max(from) - from, s.end.min(to) - from, s.rule_id))
            .collect();
        assert_eq!(
            wednesday,
            vec![
                (0, 8 * 60 + 30, Some(everything.id)),
                (8 * 60 + 30, 17 * 60 + 20, Some(office.id)),
                (17 * 60 + 20, 24 * 60, Some(everything.id)),
            ],
            "the rule below until 08:30, this rule until 17:20, then the rule below again"
        );
        assert!(week
            .iter()
            .all(|s| s.kind == CoverageKind::Rules && s.channel_ids == vec![jsm]));
        let coarse = week_in_slots(&matched, &[], &ALL, COVERAGE_SLOT_MINUTES);
        assert!(
            coarse.iter().all(|s| s.start % COVERAGE_SLOT_MINUTES == 0),
            "the coverage card keeps its 15-minute slots"
        );
        assert!(!coarse.iter().any(|s| s.start == 2 * 1440 + 17 * 60 + 20));
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
            route_at(&both, &[], &ALL, &clock, None, at(23, 10, 0)),
            RouteNow::Rules([mail].into_iter().collect())
        );
        assert_eq!(
            route_at(&both, &[], &ALL, &clock, None, at(23, 22, 0)),
            RouteNow::Rules([oncall].into_iter().collect())
        );
        assert_eq!(
            next_change(&both, &[], &ALL, &clock, None, at(23, 10, 0)),
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
            route_at(&[&day], &[chat], &ALL, &clock, None, saturday),
            RouteNow::CatchAll([chat].into_iter().collect())
        );
        assert_eq!(
            route_at(&[&day], &[], &ALL, &clock, None, saturday),
            RouteNow::Held
        );
        assert_eq!(
            next_change(&[&day], &[], &ALL, &clock, None, saturday),
            Some(at(30, 8, 30)),
            "held until Monday 08:30"
        );
    }

    #[test]
    fn no_matching_rule_goes_to_the_catch_all_or_nowhere() {
        let chat = Uuid::new_v4();
        let clock = RuleClock::default();
        assert_eq!(
            route_at(&[], &[chat], &ALL, &clock, None, at(23, 10, 0)),
            RouteNow::CatchAll([chat].into_iter().collect())
        );
        assert_eq!(
            route_at(&[], &[], &ALL, &clock, None, at(23, 10, 0)),
            RouteNow::Nowhere
        );
    }

    #[test]
    fn an_always_rule_never_changes_and_a_channel_less_rule_does_not_block_the_catch_all() {
        let (mail, chat) = (Uuid::new_v4(), Uuid::new_v4());
        let always = rule(mail, WhenMode::Always);
        let clock = RuleClock::default();
        assert_eq!(
            next_change(&[&always], &[], &ALL, &clock, None, at(23, 3, 0)),
            None
        );
        let empty = RoutingRule {
            channel_ids: Vec::new(),
            ..rule(mail, WhenMode::Always)
        };
        assert_eq!(
            route_at(&[&empty], &[chat], &ALL, &clock, None, at(23, 3, 0)),
            RouteNow::CatchAll([chat].into_iter().collect())
        );
        assert_eq!(
            route_at(&[&empty], &[], &ALL, &clock, None, at(23, 3, 0)),
            RouteNow::Nowhere
        );
    }

    /// A channel that is switched off (or deleted) hears nothing, so it neither keeps an alert from
    /// the catch-all nor counts as a place to hold it for, and the catch-all naming only such a
    /// channel is no catch-all.
    #[test]
    fn a_channel_that_cannot_hear_counts_as_no_channel() {
        let (off, chat) = (Uuid::new_v4(), Uuid::new_v4());
        let hears = |id: &Uuid| *id != off;
        let clock = RuleClock::default();
        let open = rule(off, WhenMode::Always);
        assert_eq!(
            route_at(&[&open], &[chat], &hears, &clock, None, at(23, 10, 0)),
            RouteNow::CatchAll([chat].into_iter().collect()),
            "an open rule whose only channel is off falls to the catch-all"
        );
        assert_eq!(
            route_at(&[&open], &[], &hears, &clock, None, at(23, 10, 0)),
            RouteNow::Nowhere,
            "and without one it reaches nobody, which is not a reason to hold"
        );
        let day = rule(chat, WhenMode::During);
        assert_eq!(
            route_at(&[&day], &[off], &hears, &clock, None, at(28, 10, 0)),
            RouteNow::Held,
            "a catch-all naming only an off channel does not take a closed rule's alert"
        );
        assert_eq!(
            week_in_slots(&[&open], &[off], &hears, COVERAGE_SLOT_MINUTES),
            vec![CoverageSegment {
                start: 0,
                end: 7 * 1440,
                kind: CoverageKind::Nowhere,
                channel_ids: Vec::new(),
                rule_id: None,
            }]
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
            &ALL,
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
                rule_id: None,
            }]
        );
        // At warning, the critical-only rule no longer reaches the core node.
        let warn = coverage(
            &rules,
            &[],
            &ALL,
            yagra_common::Severity::Warning,
            &nodes,
            &edges,
        );
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
