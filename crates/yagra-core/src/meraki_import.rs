// SPDX-License-Identifier: AGPL-3.0-only
//! Which Meraki devices become nodes on their own, and what every import has to find out about a
//! device before [`crate::meraki::MerakiOrgRepo::import_devices`] may write it (ADR-164 Inc.4).
//!
//! Two callers reach the one writer: the operator picking devices on the organization's page, and
//! the inventory sync picking them itself. They differ in **which devices they pick** and must not
//! differ in anything else, so everything after the pick lives here, once:
//! - [`ImportResolver::filings`] — where a device goes. The IP-range match is PostgreSQL's
//!   (`GroupRepo::match_address_prefixes`), the fold and the plan are pure
//!   ([`crate::meraki_filing`]); this only strings them together. The organization's device list
//!   calls it as well, to say where a device *would* go, so the page cannot promise one folder and
//!   the import use another.
//! - [`ImportResolver::resolve`] — the profile, the name a nameless device goes by, the filing.
//!
//! [`pick_automatic`] is the sync's half of the pick, and it is pure: the three conditions of
//! ADR-164 決定 5 are read off [`MerakiDeviceState::New`], which already means "listed, seen online,
//! never a node here", plus the network's watch flag and the organization's cap.

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::Arc;

use uuid::Uuid;

use crate::groups::GroupRepo;
use crate::meraki::MerakiImportDevice;
use crate::meraki_filing::{addresses_to_match, plan_filing, Filing};
use crate::meraki_inventory::{DeviceRecord, MerakiDeviceState};
use crate::repo::NodeRepo;

/// One device somebody — an operator or the sync — picked for import, before anything about it has
/// been resolved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportCandidate {
    pub serial: String,
    pub name: String,
    pub model: Option<String>,
    pub product_type: String,
    pub network_id: String,
    /// The network's name, when it is known. Names the folder a device falls back to.
    pub network_name: Option<String>,
    pub lan_ip: Option<IpAddr>,
}

impl From<&DeviceRecord> for ImportCandidate {
    fn from(d: &DeviceRecord) -> Self {
        Self {
            serial: d.serial.clone(),
            name: d.name.clone(),
            model: d.model.clone(),
            product_type: d.product_type.clone(),
            network_id: d.network_id.clone(),
            network_name: d.network_name.clone(),
            lan_ip: d.lan_ip,
        }
    }
}

/// What the sync picked, and what the cap made it leave.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AutoPick {
    pub chosen: Vec<ImportCandidate>,
    /// Devices that qualified and were left out because the organization is at `max_devices`.
    /// Written back to the organization's row — a cap that leaves things out silently is
    /// indistinguishable from an import that is not working.
    pub over_cap: u32,
}

/// The devices the sync imports on its own (ADR-164 決定 5). Pure.
///
/// A device qualifies when all three hold: Meraki lists it and has reported it online at least once
/// and it has never been a node here — which is exactly [`MerakiDeviceState::New`] — and its network
/// is one the organization watches.
/// - *Never online* keeps out the spares still in their boxes: the Dashboard lists them, and each
///   would raise a node-down alert the moment it became a node.
/// - *Never a node here* keeps out a device an operator deleted. Putting it back is theirs to do,
///   from the organization's page.
///
/// The cap counts every node the organization already holds, missing ones included: a node whose
/// device left the Dashboard is still a node. Devices are taken in the order given, which the
/// caller makes stable (`MerakiInventoryRepo::devices` orders by name, then serial), so which ones
/// a cap leaves out does not change from sync to sync.
///
/// An MX whose network's LAN side has not been read yet ([`DeviceRecord::lan_pending`]) **waits** —
/// its address, and so its folder, is not known, and an import files a node once and never moves
/// it (決定 6, 決定 28) — **and keeps its place**: it holds its slot under the cap, in name order,
/// until the sync that reads its network imports it. 🚨 It used to step out of the order instead,
/// which is the rule above broken: on an organization's first sync no MX network has been read, so
/// the switches and access points behind them in the order filled the whole cap, and when the MX
/// were ready there was no room left. On a lab organization of 3,170 importable devices under a cap
/// of 1,000 that works out to no MX at all (from the counts; it was caught before anyone ran it).
#[must_use]
pub fn pick_automatic(devices: &[DeviceRecord], max_devices: u32) -> AutoPick {
    let held = devices.iter().filter(|d| d.node_id.is_some()).count();
    let room = usize::try_from(max_devices)
        .unwrap_or(usize::MAX)
        .saturating_sub(held);
    let qualifying: Vec<&DeviceRecord> = devices
        .iter()
        .filter(|d| d.state == MerakiDeviceState::New && d.network_monitored)
        .collect();
    AutoPick {
        chosen: qualifying
            .iter()
            .take(room)
            .filter(|d| !d.lan_pending)
            .map(|d| ImportCandidate::from(*d))
            .collect(),
        over_cap: u32::try_from(qualifying.len().saturating_sub(room)).unwrap_or(u32::MAX),
    }
}

/// Candidates with everything the writer needs, and whether there was anything to match against.
pub struct ResolvedImport {
    pub devices: Vec<MerakiImportDevice>,
    /// Whether any folder carries an IP range at all. False means an "unmatched" says nothing about
    /// the device: there was nothing for its address to match.
    pub ranges_configured: bool,
}

/// Resolves picked devices into what the writer takes. One per process, shared by the API and the
/// sync.
pub struct ImportResolver {
    groups: Arc<GroupRepo>,
    repo: Arc<NodeRepo>,
}

impl ImportResolver {
    #[must_use]
    pub fn new(groups: Arc<GroupRepo>, repo: Arc<NodeRepo>) -> Self {
        Self { groups, repo }
    }

    /// One [`Filing`] per address, in order, and whether any folder has an IP range.
    ///
    /// 🚨 **A failed range read is an error, never "no range matched".** Falling back to the network
    /// folders would turn "the database could not be read" into a filing decision, and put a site's
    /// devices in the wrong place with a success to show for it.
    ///
    /// Unscoped on purpose: every caller has already refused a folder-scoped account (the API), or
    /// is the system itself (the sync), so there is no scope to narrow by.
    pub async fn filings(
        &self,
        addresses: &[Option<IpAddr>],
        file_by_prefix: bool,
    ) -> anyhow::Result<(Vec<Filing>, bool)> {
        let ranges_configured = self.groups.any_prefixes(None).await?;
        let fold = if file_by_prefix && ranges_configured {
            let asked = addresses_to_match(addresses);
            let hits = self.groups.match_address_prefixes(&asked, None).await?;
            crate::groups::fold_prefix_matches(&asked, hits)
        } else {
            // Nothing to ask, or nothing to ask against: an empty answer, which `plan_filing` reads
            // as "no range holds it" for every address.
            crate::groups::fold_prefix_matches::<IpAddr>(&[], Vec::new())
        };
        Ok((
            plan_filing(addresses, &fold, file_by_prefix),
            ranges_configured,
        ))
    }

    /// Which of `addresses` lie inside some folder's IP range — what picks an MX's address out of
    /// its VLANs (決定 28). One statement for all of them, the one [`Self::filings`] asks.
    ///
    /// 🚨 **A failed read is an error, never "none of them".** An empty answer makes every MX take
    /// its lowest VLAN, and the next sync that reads the ranges moves them back: a database hiccup
    /// would rewrite hundreds of node addresses twice.
    pub async fn in_a_range(
        &self,
        addresses: &[IpAddr],
    ) -> anyhow::Result<std::collections::HashSet<IpAddr>> {
        if addresses.is_empty() || !self.groups.any_prefixes(None).await? {
            return Ok(std::collections::HashSet::new());
        }
        Ok(self
            .groups
            .match_address_prefixes(addresses, None)
            .await?
            .into_iter()
            .map(|hit| hit.key)
            .collect())
    }

    /// Resolve each candidate's filing, profile and display name.
    pub async fn resolve(
        &self,
        candidates: Vec<ImportCandidate>,
        file_by_prefix: bool,
    ) -> anyhow::Result<ResolvedImport> {
        let addresses: Vec<Option<IpAddr>> = candidates.iter().map(|c| c.lan_ip).collect();
        let (filings, ranges_configured) = self.filings(&addresses, file_by_prefix).await?;

        // One lookup per product type, not per device: a batch names a handful of types and may
        // name a thousand devices, most of which the writer will skip as already imported.
        let mut profiles: HashMap<String, Option<Uuid>> = HashMap::new();
        let mut devices = Vec::with_capacity(candidates.len());
        for (c, filing) in candidates.into_iter().zip(filings) {
            let profile_id = match profiles.get(&c.product_type) {
                Some(known) => *known,
                None => {
                    let found = self.profile_for(&c.product_type).await;
                    profiles.insert(c.product_type.clone(), found);
                    found
                }
            };
            // A device with no name is identified by its serial, which is always present. The rule is
            // `node_name_for`'s and not written here: the sync recognises a later rename by
            // comparing a node's name with what this produced (ADR-164 決定 14).
            let name = crate::meraki_inventory::node_name_for(&c.name, &c.serial);
            let network_name = c
                .network_name
                .filter(|n| !n.trim().is_empty())
                .unwrap_or_else(|| c.network_id.clone());
            devices.push(MerakiImportDevice {
                serial: c.serial,
                name,
                model: c.model,
                product_type: c.product_type,
                network_id: c.network_id,
                network_name,
                lan_ip: c.lan_ip,
                profile_id,
                filing,
            });
        }
        Ok(ResolvedImport {
            devices,
            ranges_configured,
        })
    }

    /// The profile a Meraki device of this product type is monitored with: the built-in Meraki-API
    /// profile for the type, else its category's profile, so an unrecognized product type still gets
    /// monitored rather than nothing. A failed read is `None` — the node is created without a
    /// profile and an operator can set one; refusing the import over it would be the worse trade.
    async fn profile_for(&self, product_type: &str) -> Option<Uuid> {
        let by_name = match yagra_common::api_profile_name_for_product_type(product_type) {
            Some(name) => self.repo.profile_id_for_name(name).await.unwrap_or(None),
            None => None,
        };
        match by_name {
            Some(p) => Some(p),
            None => self
                .repo
                .profile_id_for_category(
                    yagra_common::category_for_product_type(product_type).as_str(),
                )
                .await
                .unwrap_or(None),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::DateTime;

    fn record(serial: &str, state: MerakiDeviceState, network_monitored: bool) -> DeviceRecord {
        let has_node = matches!(
            state,
            MerakiDeviceState::Monitored | MerakiDeviceState::Missing
        );
        DeviceRecord {
            serial: serial.into(),
            name: format!("dev-{serial}"),
            model: Some("MR46".into()),
            product_type: "wireless".into(),
            network_id: "N_1".into(),
            network_name: Some("HQ".into()),
            network_monitored,
            lan_ip: Some("10.0.0.7".parse().expect("ip")),
            lan_pending: false,
            state,
            node_id: has_node.then(|| Uuid::from_u128(1)),
            node_group_id: None,
            first_seen_at: DateTime::from_timestamp(1_800_000_000, 0).expect("in range"),
            missing_since: None,
            ha_role: None,
        }
    }

    fn serials(pick: &AutoPick) -> Vec<&str> {
        pick.chosen.iter().map(|c| c.serial.as_str()).collect()
    }

    /// ADR-164 決定 5, every condition in both directions: each device below differs from the one
    /// that qualifies in exactly one fact.
    #[test]
    fn only_a_new_device_in_a_watched_network_is_picked() {
        use MerakiDeviceState as S;
        let devices = [
            record("QUALIFIES", S::New, true),
            record("unwatched-network", S::New, false),
            record("never-online", S::NeverOnline, true),
            record("deleted-here", S::Deleted, true),
            record("already-a-node", S::Monitored, true),
            record("node-gone-from-meraki", S::Missing, true),
        ];
        let pick = pick_automatic(&devices, 1000);
        assert_eq!(serials(&pick), ["QUALIFIES"]);
        assert_eq!(pick.over_cap, 0);
    }

    /// ADR-164 決定 28: an MX whose network's LAN side has not been read yet has no address, and an
    /// import would file it by none and never move it. It waits — **in its place**: the devices
    /// behind it in the order do not take its slot under the cap, or on an organization's first
    /// sync they would fill the cap before any MX could go in.
    #[test]
    fn an_mx_waiting_for_its_network_keeps_its_place_under_the_cap() {
        use MerakiDeviceState as S;
        let mut waiting = record("mx-unread", S::New, true);
        waiting.product_type = "appliance".into();
        waiting.lan_ip = None;
        waiting.lan_pending = true;
        let devices = [
            waiting,
            record("ap-1", S::New, true),
            record("ap-2", S::New, true),
        ];
        // Room for all: the MX waits, the rest go in.
        assert_eq!(serials(&pick_automatic(&devices, 1000)), ["ap-1", "ap-2"]);
        // Room for two: the MX holds the first slot, so only one access point goes in, and the one
        // behind it is the one the cap leaves out — the same one it will leave out once the MX is in.
        let two = pick_automatic(&devices, 2);
        assert_eq!(serials(&two), ["ap-1"]);
        assert_eq!(two.over_cap, 1);

        // Once the network has been read, the same MX goes into the slot it held.
        let mut read = devices.clone();
        read[0].lan_pending = false;
        let after = pick_automatic(&read, 2);
        assert_eq!(serials(&after), ["mx-unread", "ap-1"]);
        assert_eq!(after.over_cap, 1);
    }

    /// The cap counts the nodes the organization already holds, stops the import at the limit, and
    /// says how many it left — in the order given, so the same ones are left out every sync.
    #[test]
    fn the_cap_counts_held_nodes_and_reports_what_it_left_out() {
        use MerakiDeviceState as S;
        let devices = [
            record("held-1", S::Monitored, true),
            record("held-2", S::Missing, true),
            record("a", S::New, true),
            record("b", S::New, true),
            record("c", S::New, true),
        ];
        let pick = pick_automatic(&devices, 3);
        assert_eq!(
            serials(&pick),
            ["a"],
            "two nodes are held; one place is left"
        );
        assert_eq!(pick.over_cap, 2);

        // At or past the cap nothing is picked and everything that qualified is reported.
        let full = pick_automatic(&devices, 2);
        assert!(full.chosen.is_empty());
        assert_eq!(full.over_cap, 3);
        let past = pick_automatic(&devices, 1);
        assert!(past.chosen.is_empty());
        assert_eq!(past.over_cap, 3);

        // Room for everything: nothing is left out.
        assert_eq!(pick_automatic(&devices, 5).over_cap, 0);
    }

    /// A device that does not qualify is not "over the cap" — the number on the page must be
    /// devices raising the cap would bring in, and nothing else.
    #[test]
    fn a_device_that_does_not_qualify_is_never_counted_as_over_the_cap() {
        use MerakiDeviceState as S;
        let devices = [
            record("held", S::Monitored, true),
            record("never-online", S::NeverOnline, true),
            record("unwatched", S::New, false),
        ];
        let pick = pick_automatic(&devices, 1);
        assert!(pick.chosen.is_empty());
        assert_eq!(pick.over_cap, 0);
    }
}
