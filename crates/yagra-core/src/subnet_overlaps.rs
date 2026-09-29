// SPDX-License-Identifier: AGPL-3.0-only
//! Which address ranges two **sites** both use (ADR-187).
//!
//! Every device's interface addresses are already stored (`node_l3`, ADR-043/157). This module
//! turns each into its subnet, files it under the device's site, and reports every range that
//! more than one site carries — the same range at two sites, or one site's range wholly inside
//! another's. Two CIDR blocks are either nested or disjoint, so those are the only shapes there
//! are; the same range splits in two by whether any single **address** is also shared, because a
//! segment several sites genuinely sit on (a carrier's WAN) cannot hold one address twice.
//!
//! What is *expected* to repeat is taken out in three ways, each attributed:
//!
//! * **A link between sites** — a `/30`/`/31` (`/126`/`/127`) carried by exactly two devices at two
//!   addresses. That is one cable, not a reuse, and it is never listed as open.
//! * **An operator's rule** — a range, a word in the port's name or description, or both. A rule
//!   removes the *places* it matches before the ranges are compared, so an overlap it leaves with
//!   one site is gone and is listed as excluded, naming the rule.
//! * **An acknowledgement** — "this reuse is deliberate", recorded against the set of sites it
//!   covered then. A site joining the overlap later reopens it.
//!
//! A **hint** ("this looks like a WAN") is offered and never acted on: excluding by guess would
//! hide the reuse the screen exists to find.
//!
//! Pure: the stores are read by `api::subnet_overlaps`, which also decides what a scoped caller
//! may be told about a site it cannot see. The subnet rule is [`yagra_common::L3Address::subnet`],
//! the one the network map and ADR-170 use, so the three never disagree about what a subnet is.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::net::IpAddr;

use serde::{Deserialize, Serialize};
use uuid::Uuid;
use yagra_common::{subnet_key, L3Snapshot, SubnetKey};

/// How many places are listed with one overlap. `place_count` beside them is never capped.
pub const PLACES_MAX: usize = 50;

/// How many inner ranges a nested overlap lists. `inner_count` beside them is never capped.
pub const INNER_MAX: usize = 20;

/// From how many sites carrying one range the "looks like a template" hint is offered.
pub const TEMPLATE_MIN_SITES: usize = 10;

/// From how many sites a same-range overlap with one device per site reads as a shared line.
pub const SHARED_LINE_MIN_SITES: usize = 3;

/// Words that, as a whole token of a port's name or description, suggest a WAN-facing port.
/// A token matches when it is the word, optionally followed by digits (`Dialer1`, `wwan0`).
pub const WAN_WORDS: &[&str] = &[
    "wan", "isp", "internet", "onu", "dialer", "wwan", "lte", "cellular", "pppoe", "carrier",
];

/// Words that suggest a port used only between the two members of a redundant pair.
pub const REDUNDANCY_WORDS: &[&str] = &["ha", "hasync", "sync", "heartbeat", "failover"];

/// A site: the nearest folder of type Site above a device, else its own folder; `None` is the root.
pub type SiteId = Option<Uuid>;

/// How two sites' ranges meet.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum OverlapKind {
    /// The same range at two sites, and at least one address in it is configured at both — which
    /// one shared segment cannot hold, so this is almost certainly a reuse.
    SameAddress,
    /// One site's range wholly contains another site's.
    Nested,
    /// The same range at two sites with no address in common: a reuse, or a segment the sites
    /// genuinely share (a carrier's WAN). Yagra cannot tell which, so it asks.
    SameRange,
}

impl OverlapKind {
    /// Every kind, for the token test and the WebUI's mirror.
    #[cfg(test)]
    pub const ALL: [OverlapKind; 3] = [
        OverlapKind::SameAddress,
        OverlapKind::Nested,
        OverlapKind::SameRange,
    ];
}

/// Where an overlap stands after the exclusions and acknowledgements are applied.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum OverlapStatus {
    /// Nobody has said anything about it.
    Open,
    /// An operator recorded it as deliberate, and no site has joined it since.
    Intentional,
    /// A link between two sites, or every place that made it an overlap matched a rule.
    Excluded,
}

impl OverlapStatus {
    #[cfg(test)]
    pub const ALL: [OverlapStatus; 3] = [
        OverlapStatus::Open,
        OverlapStatus::Intentional,
        OverlapStatus::Excluded,
    ];
}

/// Why an operator says a range is expected to repeat. Stored in `subnet_overlap_rules.reason`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ExclusionReason {
    /// A WAN line: the carrier's or the ONU's side of a router.
    Wan,
    /// A link used only between the two members of a redundant pair.
    Redundancy,
    /// A segment several sites share on purpose (a carrier's closed network).
    SharedLine,
    /// A management network.
    Management,
    /// Anything else; the note says what.
    Other,
}

crate::stored_enum::token_enum!(ExclusionReason, Other, "subnet_overlap_rules.reason", [
    Wan => "wan",
    Redundancy => "redundancy",
    SharedLine => "shared_line",
    Management => "management",
    Other => "other",
]);

/// What took an overlap out of the open list.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, utoipa::ToSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Exclusion {
    /// A `/30`, `/31`, `/126` or `/127` carried by exactly two devices at two addresses: the link
    /// between two sites. Built in; no rule turns it off.
    Link,
    /// An operator's rule (or the built-in CGNAT one) matched every place that made it an overlap.
    Rule { rule_id: Uuid },
}

/// Why an overlap looks expected. Offered, never acted on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, utoipa::ToSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum OverlapHint {
    /// Every place is a port whose name or description carries a WAN word (`word`).
    Wan { word: String },
    /// Every place is a port whose name or description carries a redundancy word (`word`).
    Redundancy { word: String },
    /// The same range, a different address at every site, one device per site, at three or more
    /// sites: the shape of a line the sites share.
    SharedLine,
    /// Ten or more sites carry it: the shape of a site template.
    Template,
}

/// One exclusion rule, as the pure half needs it.
#[derive(Debug, Clone)]
pub struct Rule {
    pub id: Uuid,
    /// Places inside this range match. `None` ⇒ any range.
    pub range: Option<SubnetKey>,
    /// Places on a port whose name or description carries this as whole words (case-insensitive;
    /// a word may be followed by digits) match. `None` ⇒ any port.
    pub port_text: Option<String>,
    pub enabled: bool,
}

/// One acknowledgement: the overlap `key` is deliberate across `sites`.
#[derive(Debug, Clone)]
pub struct Ack {
    pub key: String,
    pub sites: BTreeSet<SiteId>,
}

/// A port's name and description, when the interface inventory has them.
#[derive(Debug, Clone, Default)]
pub struct Port {
    pub name: Option<String>,
    pub alias: Option<String>,
}

/// One place a range was seen: a device, the port, and the address with its length.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, utoipa::ToSchema)]
pub struct Place {
    /// The site: the nearest folder of type Site above the device, else its own folder. `null` ⇒
    /// the tree root.
    pub site_id: Option<Uuid>,
    /// Filled by the caller.
    pub site_name: Option<String>,
    pub node_id: Uuid,
    /// Filled by the caller.
    pub node_name: Option<String>,
    pub ifindex: u32,
    pub if_name: Option<String>,
    pub if_alias: Option<String>,
    /// The address as configured, `address/length`.
    pub address: String,
    /// The range it forms, `network/length`.
    pub subnet: String,
    #[serde(skip)]
    #[schema(ignore)]
    pub(crate) ip: IpAddr,
}

/// One range more than one site carries.
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct Overlap {
    /// Stable identity, used to acknowledge it: `same:<range>` or `nested:<outer range>`. The same
    /// range keeps its key whether an address is shared or not, so an acknowledgement survives
    /// that changing. `within:<range>` when `outer_withheld` — a scoped caller cannot acknowledge.
    pub key: String,
    pub kind: OverlapKind,
    pub status: OverlapStatus,
    /// The range; for `nested`, the outer one — or, when `outer_withheld`, the narrowest range
    /// covering the caller's own inner ranges.
    pub subnet: String,
    /// For `nested`: the outer range is carried only at sites the caller may not see, so it is
    /// not named (ADR-014).
    pub outer_withheld: bool,
    /// For `nested`: the ranges inside it at other sites, at most [`INNER_MAX`].
    pub inner: Vec<String>,
    pub inner_count: u32,
    /// For `same_address`: the addresses configured at more than one site.
    pub shared_addresses: Vec<String>,
    /// How many distinct sites carry it (a scoped caller's hidden ones included).
    pub site_count: u32,
    /// Sites the caller may not see. Their names and devices are withheld (ADR-014).
    pub hidden_sites: u32,
    /// How many distinct devices carry it (visible ones only, after the caller's scope).
    pub node_count: u32,
    /// Up to [`PLACES_MAX`] places, ordered by site, device, then port.
    pub places: Vec<Place>,
    pub place_count: u32,
    /// Why it looks expected, when it does. A suggestion only.
    pub hint: Option<OverlapHint>,
    /// What excluded it. Empty unless `status` is `excluded`.
    pub excluded_by: Vec<Exclusion>,
    /// The acknowledgement's note, when `status` is `intentional`.
    pub note: Option<String>,
    #[serde(skip)]
    #[schema(ignore)]
    pub(crate) sites: BTreeSet<SiteId>,
}

/// Everything the comparison reads.
pub struct Input<'a> {
    pub observed: &'a [(Uuid, &'a L3Snapshot)],
    pub site_of: &'a HashMap<Uuid, SiteId>,
    pub ports: &'a HashMap<(Uuid, u32), Port>,
    pub rules: &'a [Rule],
    pub acks: &'a [Ack],
    /// The devices the caller may see; `None` ⇒ all. An overlap with no visible place is not
    /// reported, and the sites, ranges and addresses only hidden devices carry are withheld —
    /// counted in `hidden_sites`, never named (ADR-014).
    pub visible: Option<&'a HashSet<Uuid>>,
}

/// The comparison's answer, before any scope is applied.
pub struct Findings {
    /// Open first, then intentional, then excluded; inside each by kind, then by site count
    /// (most first), then by range.
    pub overlaps: Vec<Overlap>,
    /// Distinct ranges compared.
    pub subnets_checked: u32,
    /// How many overlaps each rule excluded, by rule id.
    pub rule_hits: HashMap<Uuid, u32>,
}

/// Compare every range the devices carry across sites.
#[must_use]
pub fn find(input: &Input<'_>) -> Findings {
    let places = collect_places(input);
    let subnets_checked = u32::try_from(
        places
            .iter()
            .map(|(k, _)| *k)
            .collect::<BTreeSet<_>>()
            .len(),
    )
    .unwrap_or(u32::MAX);

    // Everything, and what remains once the rules have removed the places they match. An overlap
    // present in the first and absent (or a different shape) in the second is excluded by rule.
    let all = overlaps_of(&places);
    let kept: Vec<(SubnetKey, Place)> = places
        .iter()
        .filter(|(k, p)| !input.rules.iter().any(|r| rule_matches(r, k, p)))
        .cloned()
        .collect();
    let remaining: HashMap<String, Draft> = overlaps_of(&kept)
        .into_iter()
        .map(|d| (d.key.clone(), d))
        .collect();

    let mut rule_hits: HashMap<Uuid, u32> = HashMap::new();
    let mut overlaps: Vec<Overlap> = all
        .into_iter()
        .map(|full| {
            let mut excluded_by = Vec::new();
            if full.is_link() {
                excluded_by.push(Exclusion::Link);
            }
            let draft = match remaining.get(&full.key) {
                Some(kept) if excluded_by.is_empty() => kept.clone(),
                _ => {
                    if excluded_by.is_empty() {
                        let ids: BTreeSet<Uuid> = input
                            .rules
                            .iter()
                            .filter(|r| full.places.iter().any(|(k, p)| rule_matches(r, k, p)))
                            .map(|r| r.id)
                            .collect();
                        for id in ids {
                            *rule_hits.entry(id).or_default() += 1;
                            excluded_by.push(Exclusion::Rule { rule_id: id });
                        }
                    }
                    full
                }
            };
            let ack = input.acks.iter().find(|a| a.key == draft.key);
            let status = if !excluded_by.is_empty() {
                OverlapStatus::Excluded
            } else if ack.is_some_and(|a| draft.sites.is_subset(&a.sites)) {
                OverlapStatus::Intentional
            } else {
                OverlapStatus::Open
            };
            draft.finish(status, excluded_by, input.visible)
        })
        .filter(|o| !o.places.is_empty())
        .collect();
    overlaps.sort_by(|a, b| {
        (a.status, a.kind, std::cmp::Reverse(a.site_count), &a.subnet).cmp(&(
            b.status,
            b.kind,
            std::cmp::Reverse(b.site_count),
            &b.subnet,
        ))
    });
    // Two withheld outer ranges around the same visible inner ones name the same covering range;
    // a key must still pick out one row.
    let mut seen: HashMap<String, u32> = HashMap::new();
    for o in &mut overlaps {
        let n = seen.entry(o.key.clone()).or_default();
        *n += 1;
        if *n > 1 {
            o.key = format!("{}#{n}", o.key);
        }
    }
    Findings {
        overlaps,
        subnets_checked,
        rule_hits,
    }
}

/// Whether a rule removes one place.
fn rule_matches(rule: &Rule, subnet: &SubnetKey, place: &Place) -> bool {
    if !rule.enabled {
        return false;
    }
    if rule.range.is_none() && rule.port_text.is_none() {
        return false;
    }
    if let Some(range) = &rule.range {
        if !range.contains(subnet) {
            return false;
        }
    }
    if let Some(text) = &rule.port_text {
        let on = |s: &Option<String>| s.as_deref().is_some_and(|s| carries_words(s, text));
        if !on(&place.if_name) && !on(&place.if_alias) {
            return false;
        }
    }
    true
}

/// A port name or description split into lowercase words, the way [`port_word`] reads it.
fn words_of(text: &str) -> Vec<String> {
    text.to_lowercase()
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|t| !t.is_empty())
        .map(str::to_owned)
        .collect()
}

/// Whether one word of a port's text is `word`, optionally followed by digits (`Dialer1`).
fn word_is(token: &str, word: &str) -> bool {
    token == word || token.trim_end_matches(|c: char| c.is_ascii_digit()) == word
}

/// Whether a rule's port text appears in `text` as whole words, in order — the rule a hint uses,
/// so a rule suggested from a hint matches the ports the hint named and no others: `ha` matches
/// `HA sync` and not `Port-channel1`. A needle with no ASCII letter or digit (text in another
/// script) has no words to compare and matches as plain text instead.
fn carries_words(text: &str, needle: &str) -> bool {
    let wanted = words_of(needle);
    if wanted.is_empty() {
        let needle = needle.trim().to_lowercase();
        return !needle.is_empty() && text.to_lowercase().contains(&needle);
    }
    words_of(text)
        .windows(wanted.len())
        .any(|w| w.iter().zip(&wanted).all(|(t, n)| word_is(t, n)))
}

/// Every address that forms a subnet, as a place under its device's site.
fn collect_places(input: &Input<'_>) -> Vec<(SubnetKey, Place)> {
    let mut out = Vec::new();
    for (node, snapshot) in input.observed {
        let site = input.site_of.get(node).copied().flatten();
        for addr in &snapshot.addresses {
            let Some(subnet) = addr.subnet() else {
                continue;
            };
            let port = input.ports.get(&(*node, addr.ifindex));
            out.push((
                subnet,
                Place {
                    site_id: site,
                    site_name: None,
                    node_id: *node,
                    node_name: None,
                    ifindex: addr.ifindex,
                    if_name: port.and_then(|p| p.name.clone()),
                    if_alias: port.and_then(|p| p.alias.clone()),
                    address: format!("{}/{}", addr.ip, addr.prefix_len),
                    subnet: subnet.to_string(),
                    ip: addr.ip,
                },
            ));
        }
    }
    out
}

/// An overlap before its status is decided.
#[derive(Debug, Clone)]
struct Draft {
    key: String,
    kind: OverlapKind,
    subnet: SubnetKey,
    inner: Vec<SubnetKey>,
    shared: Vec<IpAddr>,
    sites: BTreeSet<SiteId>,
    places: Vec<(SubnetKey, Place)>,
}

impl Draft {
    /// The link between two sites: one short range, two devices, two addresses.
    fn is_link(&self) -> bool {
        let short = match self.subnet.network {
            IpAddr::V4(_) => self.subnet.prefix_len >= 30,
            IpAddr::V6(_) => self.subnet.prefix_len >= 126,
        };
        if !short || self.kind == OverlapKind::Nested {
            return false;
        }
        let nodes: BTreeSet<Uuid> = self.places.iter().map(|(_, p)| p.node_id).collect();
        let ips: BTreeSet<IpAddr> = self.places.iter().map(|(_, p)| p.ip).collect();
        nodes.len() == 2 && ips.len() == 2
    }

    fn finish(
        self,
        status: OverlapStatus,
        excluded_by: Vec<Exclusion>,
        visible: Option<&HashSet<Uuid>>,
    ) -> Overlap {
        let sees = |p: &Place| visible.is_none_or(|v| v.contains(&p.node_id));
        // A hint quotes a port's words, so a scoped caller's is read from its own places only.
        let hint = if visible.is_none() {
            hint_for(&self)
        } else {
            hint_for(&Draft {
                places: self
                    .places
                    .iter()
                    .filter(|(_, p)| sees(p))
                    .cloned()
                    .collect(),
                ..self.clone()
            })
        };
        let seen_sites: BTreeSet<SiteId> = self
            .places
            .iter()
            .filter(|(_, p)| sees(p))
            .map(|(_, p)| p.site_id)
            .collect();
        let hidden_sites = count(self.sites.len() - seen_sites.len());
        let inner: Vec<SubnetKey> = self
            .inner
            .iter()
            .filter(|i| self.places.iter().any(|(k, p)| k == *i && sees(p)))
            .copied()
            .collect();
        // The outer range of a nested overlap is itself something a site carries. When no place
        // the caller sees carries it, it is withheld like any other hidden range, and the overlap
        // is named by the narrowest range covering the caller's own inner ones — built from what
        // it may see, so it discloses nothing, and the inner ranges still sit inside it.
        let outer_withheld = self.kind == OverlapKind::Nested
            && !self
                .places
                .iter()
                .any(|(k, p)| *k == self.subnet && sees(p));
        let (key, subnet) = match covering(&inner) {
            Some(cover) if outer_withheld => (format!("within:{cover}"), cover),
            _ => (self.key, self.subnet),
        };
        let shared: Vec<IpAddr> = self
            .shared
            .iter()
            .filter(|ip| self.places.iter().any(|(_, p)| p.ip == **ip && sees(p)))
            .copied()
            .collect();
        let mut places: Vec<Place> = self
            .places
            .into_iter()
            .map(|(_, p)| p)
            .filter(sees)
            .collect();
        places.sort_by(|a, b| {
            (a.site_id, a.node_id, a.ifindex, &a.address)
                .cmp(&(b.site_id, b.node_id, b.ifindex, &b.address))
        });
        let nodes: BTreeSet<Uuid> = places.iter().map(|p| p.node_id).collect();
        let place_count = count(places.len());
        places.truncate(PLACES_MAX);
        let inner_count = count(inner.len());
        Overlap {
            key,
            kind: self.kind,
            status,
            subnet: subnet.to_string(),
            outer_withheld,
            inner: inner
                .iter()
                .take(INNER_MAX)
                .map(ToString::to_string)
                .collect(),
            inner_count,
            shared_addresses: shared.iter().map(ToString::to_string).collect(),
            site_count: count(self.sites.len()),
            hidden_sites,
            node_count: count(nodes.len()),
            places,
            place_count,
            hint,
            excluded_by,
            note: None,
            sites: self.sites,
        }
    }
}

fn count(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

/// The narrowest range containing every one of `ranges`; `None` when there are none.
fn covering(ranges: &[SubnetKey]) -> Option<SubnetKey> {
    let (first, rest) = ranges.split_first()?;
    let mut cover = *first;
    for r in rest {
        while !cover.contains(r) {
            cover = subnet_key(cover.network, cover.prefix_len.checked_sub(1)?)?;
        }
    }
    Some(cover)
}

/// The overlaps a set of places forms: the same range at two sites, and one site's range inside
/// another's, grouped by the outer range.
fn overlaps_of(places: &[(SubnetKey, Place)]) -> Vec<Draft> {
    let mut by_subnet: BTreeMap<SubnetKey, Vec<&Place>> = BTreeMap::new();
    for (k, p) in places {
        by_subnet.entry(*k).or_default().push(p);
    }
    let sites_of = |ps: &[&Place]| -> BTreeSet<SiteId> { ps.iter().map(|p| p.site_id).collect() };

    let mut out = Vec::new();
    for (subnet, ps) in &by_subnet {
        let sites = sites_of(ps);
        if sites.len() < 2 {
            continue;
        }
        // An address is shared when two sites both configure it.
        let mut ip_sites: BTreeMap<IpAddr, BTreeSet<SiteId>> = BTreeMap::new();
        for p in ps {
            ip_sites.entry(p.ip).or_default().insert(p.site_id);
        }
        let shared: Vec<IpAddr> = ip_sites
            .into_iter()
            .filter(|(_, s)| s.len() > 1)
            .map(|(ip, _)| ip)
            .collect();
        out.push(Draft {
            key: format!("same:{subnet}"),
            kind: if shared.is_empty() {
                OverlapKind::SameRange
            } else {
                OverlapKind::SameAddress
            },
            subnet: *subnet,
            inner: Vec::new(),
            shared,
            sites,
            places: ps.iter().map(|p| (*subnet, (*p).clone())).collect(),
        });
    }

    // Nested: walk each range's wider ranges (at most 127) and look them up, rather than comparing
    // every pair — the pairwise version is quadratic in the number of ranges in the fleet.
    let mut nested: BTreeMap<SubnetKey, NestedDraft> = BTreeMap::new();
    for (inner, inner_places) in &by_subnet {
        for len in (1..inner.prefix_len).rev() {
            let Some(outer) = subnet_key(inner.network, len) else {
                continue;
            };
            let Some(outer_places) = by_subnet.get(&outer) else {
                continue;
            };
            let outer_sites = sites_of(outer_places);
            // A place inside the outer range counts when some site carrying the outer range is
            // not its own: the outer range at site A and the same inner range at A is one site's
            // own layout.
            let crossing: Vec<&Place> = inner_places
                .iter()
                .copied()
                .filter(|p| outer_sites.iter().any(|s| *s != p.site_id))
                .collect();
            if crossing.is_empty() {
                continue;
            }
            let entry = nested.entry(outer).or_default();
            entry.0.insert(*inner);
            entry
                .1
                .extend(crossing.into_iter().map(|p| (*inner, p.clone())));
        }
    }
    for (outer, (inner, mut places)) in nested {
        let outer_places = &by_subnet[&outer];
        places.extend(outer_places.iter().map(|p| (outer, (*p).clone())));
        let sites: BTreeSet<SiteId> = places.iter().map(|(_, p)| p.site_id).collect();
        out.push(Draft {
            key: format!("nested:{outer}"),
            kind: OverlapKind::Nested,
            subnet: outer,
            inner: inner.into_iter().collect(),
            shared: Vec::new(),
            sites,
            places,
        });
    }
    out
}

/// The inner ranges under one outer range, and every place that puts one of them at another site.
type NestedDraft = (BTreeSet<SubnetKey>, Vec<(SubnetKey, Place)>);

/// Whether a port's name or description carries `words` as a whole token (optionally followed by
/// digits). Returns the word that matched.
fn port_word<'w>(place: &Place, words: &[&'w str]) -> Option<&'w str> {
    let text = [place.if_name.as_deref(), place.if_alias.as_deref()]
        .into_iter()
        .flatten()
        .map(str::to_lowercase)
        .collect::<Vec<_>>()
        .join(" ");
    for token in words_of(&text) {
        if let Some(w) = words.iter().find(|w| word_is(&token, w)) {
            return Some(w);
        }
    }
    None
}

/// The word every place's port carries, when there is one; the most common if several.
fn common_word(places: &[(SubnetKey, Place)], words: &[&str]) -> Option<String> {
    let mut seen: BTreeMap<&str, usize> = BTreeMap::new();
    for (_, p) in places {
        let w = port_word(p, words)?;
        *seen.entry(w).or_default() += 1;
    }
    seen.into_iter()
        .max_by(|a, b| a.1.cmp(&b.1).then_with(|| b.0.cmp(a.0)))
        .map(|(w, _)| w.to_owned())
}

fn hint_for(draft: &Draft) -> Option<OverlapHint> {
    if let Some(word) = common_word(&draft.places, WAN_WORDS) {
        return Some(OverlapHint::Wan { word });
    }
    if let Some(word) = common_word(&draft.places, REDUNDANCY_WORDS) {
        return Some(OverlapHint::Redundancy { word });
    }
    if draft.kind == OverlapKind::SameRange && draft.sites.len() >= SHARED_LINE_MIN_SITES {
        let mut per_site: BTreeMap<SiteId, BTreeSet<Uuid>> = BTreeMap::new();
        for (_, p) in &draft.places {
            per_site.entry(p.site_id).or_default().insert(p.node_id);
        }
        if per_site.values().all(|n| n.len() == 1) {
            return Some(OverlapHint::SharedLine);
        }
    }
    if draft.sites.len() >= TEMPLATE_MIN_SITES {
        return Some(OverlapHint::Template);
    }
    None
}

impl Overlap {
    /// The sites this overlap spans, for recording an acknowledgement.
    pub(crate) fn site_ids(&self) -> &BTreeSet<SiteId> {
        &self.sites
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use yagra_common::{L3AddrType, L3Address, L3SourceTable};

    fn addr(ifindex: u32, ip: &str, len: u8) -> L3Address {
        L3Address {
            ifindex,
            ip: ip.parse().unwrap(),
            prefix_len: len,
            addr_type: L3AddrType::Unicast,
            source_table: L3SourceTable::IpAddressTable,
        }
    }

    struct Fleet {
        snaps: Vec<(Uuid, L3Snapshot)>,
        site_of: HashMap<Uuid, SiteId>,
        ports: HashMap<(Uuid, u32), Port>,
    }

    impl Fleet {
        fn new() -> Self {
            Self {
                snaps: Vec::new(),
                site_of: HashMap::new(),
                ports: HashMap::new(),
            }
        }
        /// A device at `site` carrying `(ifindex, address, length, port name)`.
        fn device(&mut self, site: u128, addrs: &[(u32, &str, u8, &str)]) -> Uuid {
            let node = Uuid::new_v4();
            self.site_of.insert(node, Some(Uuid::from_u128(site)));
            self.snaps.push((
                node,
                L3Snapshot::new(addrs.iter().map(|(i, ip, l, _)| addr(*i, ip, *l)).collect()),
            ));
            for (i, _, _, name) in addrs {
                if !name.is_empty() {
                    self.ports.insert(
                        (node, *i),
                        Port {
                            name: Some((*name).to_owned()),
                            alias: None,
                        },
                    );
                }
            }
            node
        }
        fn find(&self, rules: &[Rule], acks: &[Ack]) -> Findings {
            let observed: Vec<(Uuid, &L3Snapshot)> =
                self.snaps.iter().map(|(n, s)| (*n, s)).collect();
            find(&Input {
                observed: &observed,
                site_of: &self.site_of,
                ports: &self.ports,
                rules,
                acks,
                visible: None,
            })
        }
    }

    fn only<'a>(f: &'a Findings, key: &str) -> &'a Overlap {
        f.overlaps
            .iter()
            .find(|o| o.key == key)
            .unwrap_or_else(|| panic!("no overlap {key}: {:?}", keys(f)))
    }

    fn keys(f: &Findings) -> Vec<(String, OverlapStatus)> {
        f.overlaps
            .iter()
            .map(|o| (o.key.clone(), o.status))
            .collect()
    }

    #[test]
    fn the_same_gateway_at_two_sites_is_a_shared_address() {
        let mut fleet = Fleet::new();
        fleet.device(1, &[(1, "10.10.20.1", 24, "Vlan20")]);
        fleet.device(2, &[(1, "10.10.20.1", 24, "Gi0/1.20")]);
        let f = fleet.find(&[], &[]);
        let o = only(&f, "same:10.10.20.0/24");
        assert_eq!(o.kind, OverlapKind::SameAddress);
        assert_eq!(o.status, OverlapStatus::Open);
        assert_eq!(o.shared_addresses, vec!["10.10.20.1".to_owned()]);
        assert_eq!(o.site_count, 2);
        assert_eq!(o.node_count, 2);
    }

    #[test]
    fn the_same_range_at_different_addresses_is_a_same_range_overlap() {
        let mut fleet = Fleet::new();
        fleet.device(1, &[(1, "203.0.113.1", 24, "")]);
        fleet.device(2, &[(1, "203.0.113.11", 24, "")]);
        let f = fleet.find(&[], &[]);
        assert_eq!(only(&f, "same:203.0.113.0/24").kind, OverlapKind::SameRange);
    }

    #[test]
    fn a_range_inside_another_sites_range_is_nested_under_the_outer_one() {
        let mut fleet = Fleet::new();
        fleet.device(1, &[(1, "10.30.0.1", 16, "")]);
        fleet.device(2, &[(1, "10.30.8.1", 24, "")]);
        fleet.device(3, &[(1, "10.30.9.1", 24, "")]);
        let f = fleet.find(&[], &[]);
        let o = only(&f, "nested:10.30.0.0/16");
        assert_eq!(o.kind, OverlapKind::Nested);
        assert_eq!(o.inner, vec!["10.30.8.0/24", "10.30.9.0/24"]);
        assert_eq!(o.site_count, 3);
        assert_eq!(f.overlaps.len(), 1, "{:?}", keys(&f));
    }

    #[test]
    fn one_sites_own_layout_is_not_an_overlap() {
        let mut fleet = Fleet::new();
        fleet.device(1, &[(1, "10.30.0.1", 16, ""), (2, "10.30.8.1", 24, "")]);
        fleet.device(1, &[(1, "10.30.8.2", 24, "")]);
        let f = fleet.find(&[], &[]);
        assert!(f.overlaps.is_empty(), "{:?}", keys(&f));
        assert_eq!(f.subnets_checked, 2);
    }

    #[test]
    fn a_link_between_two_sites_is_excluded_and_a_short_range_reused_is_not() {
        let mut fleet = Fleet::new();
        fleet.device(1, &[(1, "172.31.0.1", 30, "")]);
        fleet.device(2, &[(1, "172.31.0.2", 30, "")]);
        // The same /30 as an HA pair's sync port at two sites: four devices, two addresses each.
        fleet.device(3, &[(1, "10.255.255.1", 30, "ha")]);
        fleet.device(3, &[(1, "10.255.255.2", 30, "ha")]);
        fleet.device(4, &[(1, "10.255.255.1", 30, "ha")]);
        fleet.device(4, &[(1, "10.255.255.2", 30, "ha")]);
        let f = fleet.find(&[], &[]);
        let link = only(&f, "same:172.31.0.0/30");
        assert_eq!(link.status, OverlapStatus::Excluded);
        assert_eq!(link.excluded_by, vec![Exclusion::Link]);
        let ha = only(&f, "same:10.255.255.0/30");
        assert_eq!(ha.status, OverlapStatus::Open);
        assert_eq!(
            ha.hint,
            Some(OverlapHint::Redundancy {
                word: "ha".to_owned()
            })
        );
    }

    #[test]
    fn a_rule_that_matches_every_place_excludes_the_overlap_and_names_itself() {
        let mut fleet = Fleet::new();
        fleet.device(1, &[(1, "198.51.100.2", 24, "ONU")]);
        fleet.device(2, &[(1, "198.51.100.5", 24, "to-onu")]);
        let rule = Rule {
            id: Uuid::from_u128(7),
            range: None,
            port_text: Some("Onu".to_owned()),
            enabled: true,
        };
        let f = fleet.find(std::slice::from_ref(&rule), &[]);
        let o = only(&f, "same:198.51.100.0/24");
        assert_eq!(o.status, OverlapStatus::Excluded);
        assert_eq!(o.excluded_by, vec![Exclusion::Rule { rule_id: rule.id }]);
        assert_eq!(f.rule_hits.get(&rule.id), Some(&1));

        let off = Rule {
            enabled: false,
            ..rule
        };
        let f = fleet.find(&[off], &[]);
        assert_eq!(only(&f, "same:198.51.100.0/24").status, OverlapStatus::Open);
    }

    #[test]
    fn a_rule_matching_one_site_of_three_leaves_the_overlap_open_without_that_place() {
        let mut fleet = Fleet::new();
        fleet.device(1, &[(1, "10.0.0.1", 24, "WAN")]);
        fleet.device(2, &[(1, "10.0.0.1", 24, "lan")]);
        fleet.device(3, &[(1, "10.0.0.1", 24, "lan")]);
        let rule = Rule {
            id: Uuid::from_u128(1),
            range: None,
            port_text: Some("wan".to_owned()),
            enabled: true,
        };
        let f = fleet.find(&[rule], &[]);
        let o = only(&f, "same:10.0.0.0/24");
        assert_eq!(o.status, OverlapStatus::Open);
        assert_eq!(o.site_count, 2, "the WAN place is gone");
    }

    #[test]
    fn a_range_rule_and_a_combined_rule_match_only_inside_their_range() {
        let mut fleet = Fleet::new();
        fleet.device(
            1,
            &[
                (1, "100.64.12.40", 22, "Dialer1"),
                (2, "10.1.1.1", 24, "Dialer2"),
            ],
        );
        fleet.device(
            2,
            &[
                (1, "100.64.13.7", 22, "Dialer1"),
                (2, "10.1.1.1", 24, "Dialer2"),
            ],
        );
        let cgnat = Rule {
            id: Uuid::from_u128(1),
            range: Some("100.64.0.0/10".parse().unwrap()),
            port_text: None,
            enabled: true,
        };
        let f = fleet.find(std::slice::from_ref(&cgnat), &[]);
        assert_eq!(
            only(&f, "same:100.64.12.0/22").status,
            OverlapStatus::Excluded
        );
        assert_eq!(only(&f, "same:10.1.1.0/24").status, OverlapStatus::Open);

        let both = Rule {
            port_text: Some("dialer".to_owned()),
            ..cgnat
        };
        let f = fleet.find(&[both], &[]);
        assert_eq!(only(&f, "same:10.1.1.0/24").status, OverlapStatus::Open);
    }

    #[test]
    fn an_acknowledgement_holds_until_a_new_site_joins() {
        let mut fleet = Fleet::new();
        fleet.device(1, &[(1, "172.16.100.1", 24, "")]);
        fleet.device(2, &[(1, "172.16.100.1", 24, "")]);
        let ack = Ack {
            key: "same:172.16.100.0/24".to_owned(),
            sites: [Some(Uuid::from_u128(1)), Some(Uuid::from_u128(2))].into(),
        };
        let f = fleet.find(&[], std::slice::from_ref(&ack));
        assert_eq!(
            only(&f, "same:172.16.100.0/24").status,
            OverlapStatus::Intentional
        );
        fleet.device(3, &[(1, "172.16.100.1", 24, "")]);
        let f = fleet.find(&[], &[ack]);
        assert_eq!(only(&f, "same:172.16.100.0/24").status, OverlapStatus::Open);
    }

    #[test]
    fn hints_name_a_wan_port_a_shared_line_and_a_template() {
        let mut fleet = Fleet::new();
        fleet.device(1, &[(1, "192.168.8.100", 24, "wwan0")]);
        fleet.device(2, &[(1, "192.168.8.100", 24, "wwan0")]);
        for site in 10..13 {
            fleet.device(site, &[(1, &format!("203.0.113.{site}"), 24, "Gi0/0/0")]);
        }
        for site in 20..30 {
            fleet.device(site, &[(1, "172.16.50.1", 24, "Vlan50")]);
        }
        let f = fleet.find(&[], &[]);
        assert_eq!(
            only(&f, "same:192.168.8.0/24").hint,
            Some(OverlapHint::Wan {
                word: "wwan".to_owned()
            })
        );
        assert_eq!(
            only(&f, "same:203.0.113.0/24").hint,
            Some(OverlapHint::SharedLine)
        );
        assert_eq!(
            only(&f, "same:172.16.50.0/24").hint,
            Some(OverlapHint::Template)
        );
    }

    /// A rule suggested from a hint carries the hint's word, so it must match the way the hint
    /// did: `ha` from `HA sync` must not take `Port-channel1` or `chassis` with it.
    #[test]
    fn a_port_rule_matches_whole_words_like_the_hint_it_came_from() {
        let mut fleet = Fleet::new();
        fleet.device(1, &[(1, "10.50.0.1", 24, "HA sync")]);
        fleet.device(2, &[(1, "10.50.0.1", 24, "ha-link2")]);
        fleet.device(3, &[(1, "203.0.113.1", 24, "Port-channel1")]);
        fleet.device(4, &[(1, "203.0.113.1", 24, "chassis mgmt")]);
        let rule = Rule {
            id: Uuid::from_u128(3),
            range: None,
            port_text: Some("ha".to_owned()),
            enabled: true,
        };
        let f = fleet.find(&[rule], &[]);
        assert_eq!(
            only(&f, "same:10.50.0.0/24").status,
            OverlapStatus::Excluded
        );
        assert_eq!(only(&f, "same:203.0.113.0/24").status, OverlapStatus::Open);
    }

    #[test]
    fn a_several_word_rule_matches_those_words_in_order() {
        let mut fleet = Fleet::new();
        fleet.device(1, &[(1, "10.70.0.1", 24, "to ISP-A")]);
        fleet.device(2, &[(1, "10.70.0.1", 24, "to isp a")]);
        fleet.device(3, &[(1, "10.80.0.1", 24, "isp to")]);
        fleet.device(4, &[(1, "10.80.0.1", 24, "display")]);
        let rule = Rule {
            id: Uuid::from_u128(4),
            range: None,
            port_text: Some("To ISP".to_owned()),
            enabled: true,
        };
        let f = fleet.find(&[rule], &[]);
        assert_eq!(
            only(&f, "same:10.70.0.0/24").status,
            OverlapStatus::Excluded
        );
        assert_eq!(only(&f, "same:10.80.0.0/24").status, OverlapStatus::Open);
    }

    #[test]
    fn two_withheld_outer_ranges_around_one_visible_range_keep_distinct_keys() {
        let mut fleet = Fleet::new();
        let mine = fleet.device(1, &[(1, "10.40.8.1", 24, "")]);
        fleet.device(2, &[(1, "10.40.0.1", 16, "")]);
        fleet.device(3, &[(1, "10.0.0.1", 8, "")]);
        let visible: HashSet<Uuid> = [mine].into();
        let observed: Vec<(Uuid, &L3Snapshot)> = fleet.snaps.iter().map(|(n, s)| (*n, s)).collect();
        let f = find(&Input {
            observed: &observed,
            site_of: &fleet.site_of,
            ports: &fleet.ports,
            rules: &[],
            acks: &[],
            visible: Some(&visible),
        });
        let ours: Vec<&Overlap> = f.overlaps.iter().filter(|o| o.outer_withheld).collect();
        assert_eq!(ours.len(), 2, "{:?}", keys(&f));
        assert_ne!(ours[0].key, ours[1].key);
        assert!(ours.iter().all(|o| o.subnet == "10.40.8.0/24"));
    }

    #[test]
    fn a_word_inside_a_longer_token_is_not_a_hint() {
        let mut fleet = Fleet::new();
        fleet.device(1, &[(1, "10.9.9.1", 24, "channel-1")]);
        fleet.device(2, &[(1, "10.9.9.1", 24, "swan")]);
        let f = fleet.find(&[], &[]);
        assert_eq!(only(&f, "same:10.9.9.0/24").hint, None);
    }

    #[test]
    fn addresses_that_form_no_subnet_are_not_compared() {
        let mut fleet = Fleet::new();
        fleet.device(
            1,
            &[
                (1, "127.0.0.1", 8, ""),
                (2, "169.254.1.1", 16, ""),
                (3, "10.0.0.1", 32, ""),
            ],
        );
        fleet.device(
            2,
            &[
                (1, "127.0.0.1", 8, ""),
                (2, "169.254.1.1", 16, ""),
                (3, "10.0.0.1", 32, ""),
            ],
        );
        let f = fleet.find(&[], &[]);
        assert!(f.overlaps.is_empty(), "{:?}", keys(&f));
        assert_eq!(f.subnets_checked, 0);
    }

    #[test]
    fn ipv6_ranges_are_compared_the_same_way() {
        let mut fleet = Fleet::new();
        fleet.device(1, &[(1, "2001:db8:1::1", 64, "")]);
        fleet.device(2, &[(1, "2001:db8:1::1", 64, "")]);
        fleet.device(3, &[(1, "2001:db8:ff::1", 127, "")]);
        fleet.device(4, &[(1, "2001:db8:ff::", 127, "")]);
        let f = fleet.find(&[], &[]);
        assert_eq!(
            only(&f, "same:2001:db8:1::/64").kind,
            OverlapKind::SameAddress
        );
        assert_eq!(
            only(&f, "same:2001:db8:ff::/127").status,
            OverlapStatus::Excluded
        );
    }

    #[test]
    fn open_overlaps_come_first_and_the_worst_kind_leads() {
        let mut fleet = Fleet::new();
        fleet.device(1, &[(1, "203.0.113.1", 24, ""), (2, "10.10.20.1", 24, "")]);
        fleet.device(2, &[(1, "203.0.113.2", 24, ""), (2, "10.10.20.1", 24, "")]);
        fleet.device(1, &[(3, "172.31.0.1", 30, "")]);
        fleet.device(2, &[(3, "172.31.0.2", 30, "")]);
        let f = fleet.find(&[], &[]);
        let order: Vec<&str> = f.overlaps.iter().map(|o| o.key.as_str()).collect();
        assert_eq!(
            order,
            vec![
                "same:10.10.20.0/24",
                "same:203.0.113.0/24",
                "same:172.31.0.0/30"
            ]
        );
    }

    #[test]
    fn places_and_inner_ranges_are_capped_but_their_counts_are_not() {
        let mut fleet = Fleet::new();
        fleet.device(1, &[(1, "10.0.0.1", 8, "")]);
        for site in 2..40u128 {
            let a = format!("10.{site}.0.1");
            let b = format!("10.{site}.1.1");
            fleet.device(site, &[(1, &a, 24, ""), (2, &b, 24, "")]);
        }
        let f = fleet.find(&[], &[]);
        let o = only(&f, "nested:10.0.0.0/8");
        assert_eq!(o.inner.len(), INNER_MAX);
        assert_eq!(o.inner_count, 76);
        assert_eq!(o.places.len(), PLACES_MAX);
        assert_eq!(o.place_count, 77);
    }

    /// A caller who sees one site of two is told the overlap exists and how many sites it cannot
    /// see — never which, never their devices, never the ranges or addresses only they carry.
    #[test]
    fn a_scoped_caller_sees_its_own_places_and_only_a_count_of_the_rest() {
        let mut fleet = Fleet::new();
        let mine = fleet.device(1, &[(1, "10.30.8.1", 24, ""), (2, "10.10.20.1", 24, "")]);
        fleet.device(2, &[(1, "10.30.0.1", 16, ""), (2, "10.10.20.1", 24, "")]);
        fleet.device(3, &[(1, "10.30.9.1", 24, "")]);
        fleet.device(4, &[(1, "10.99.0.1", 24, "")]);
        fleet.device(5, &[(1, "10.99.0.1", 24, "")]);
        let visible: HashSet<Uuid> = [mine].into();
        let observed: Vec<(Uuid, &L3Snapshot)> = fleet.snaps.iter().map(|(n, s)| (*n, s)).collect();
        let f = find(&Input {
            observed: &observed,
            site_of: &fleet.site_of,
            ports: &fleet.ports,
            rules: &[],
            acks: &[],
            visible: Some(&visible),
        });
        let same = only(&f, "same:10.10.20.0/24");
        assert_eq!(same.hidden_sites, 1);
        assert_eq!(same.site_count, 2);
        assert!(same.places.iter().all(|p| p.node_id == mine));
        assert_eq!(same.shared_addresses, vec!["10.10.20.1".to_owned()]);
        // The /16 exists only at site 2, which this caller cannot see: it is not named, in the
        // range or in the key, and the overlap is shown around the caller's own range instead.
        let nested = only(&f, "within:10.30.8.0/24");
        assert!(nested.outer_withheld);
        assert_eq!(nested.subnet, "10.30.8.0/24");
        assert_eq!(
            nested.inner,
            vec!["10.30.8.0/24"],
            "site 3's range is withheld"
        );
        assert_eq!(nested.hidden_sites, 2);
        assert!(
            f.overlaps
                .iter()
                .all(|o| !o.key.contains("10.30.0.0") && !o.subnet.contains("10.30.0.0")),
            "the hidden outer range appears nowhere: {:?}",
            keys(&f)
        );
        assert!(
            f.overlaps.iter().all(|o| o.key != "same:10.99.0.0/24"),
            "an overlap with no visible place is not reported"
        );
    }

    #[test]
    fn every_kind_status_and_reason_serializes_as_its_token() {
        for k in OverlapKind::ALL {
            assert!(serde_json::to_string(&k).unwrap().starts_with('"'));
        }
        for s in OverlapStatus::ALL {
            assert!(serde_json::to_string(&s).unwrap().starts_with('"'));
        }
        for r in ExclusionReason::ALL {
            assert_eq!(
                serde_json::to_string(r).unwrap(),
                format!("\"{}\"", r.as_str()),
                "the column and the JSON tag must be the same token"
            );
        }
    }
}
