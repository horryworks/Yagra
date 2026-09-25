// SPDX-License-Identifier: AGPL-3.0-only
//! Where an imported Meraki device is filed (ADR-164): the folder whose IP range holds its address,
//! or — for everything else — the organization's own `Organization ▸ Network` folder.
//!
//! Pure on purpose. [`crate::groups::GroupRepo::match_address_prefixes`] decides which ranges
//! contain an address and [`crate::groups::fold_prefix_matches`] decides whether one folder claims
//! it or two do; what is left is the part specific to Meraki — which devices are asked about at all,
//! and what each of the four non-answers means — and that is the part a wrong line in would file a
//! device into a site nobody chose.
//!
//! Until this module a Meraki device went under its network's folder unconditionally, so a
//! deployment whose folders mirror its sites (by hand, or through the NetBox sync) held every
//! Meraki device twice over: once where the site's other equipment lives, in the operator's head,
//! and once in a parallel tree the import built.

use std::collections::HashMap;
use std::net::IpAddr;

use serde::Serialize;
use uuid::Uuid;

use crate::groups::PrefixFold;

/// What the IP-range match said about one device.
///
/// 🚨 **Five, and four of them file the device in the same place.** They are kept apart because they
/// are different facts about the deployment: `Ambiguous` means two folders carry overlapping ranges
/// (a thing to go and fix), `Unmatched` means no range covers the address (add one, or nothing to
/// do), `NoAddress` means Meraki reported none (nothing a range could ever fix) and `NotAsked` means
/// the operator switched the match off. Folding them into "fell back" loses the actionable one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Filing {
    /// Exactly one folder's range claims the address. `prefix` is the range that did it.
    Matched { folder: Uuid, prefix: String },
    /// Two or more folders claim it at the same prefix length. Never resolved automatically
    /// (ADR-124 決定 5).
    Ambiguous { folders: usize },
    /// No folder's range contains the address.
    Unmatched,
    /// Meraki reported no usable address for the device.
    NoAddress,
    /// Filing by IP range was switched off for this import.
    NotAsked,
}

impl Filing {
    /// The folder the match chose, if it chose one. `None` means the network folder.
    #[must_use]
    pub fn folder(&self) -> Option<Uuid> {
        match self {
            Self::Matched { folder, .. } => Some(*folder),
            Self::Ambiguous { .. } | Self::Unmatched | Self::NoAddress | Self::NotAsked => None,
        }
    }

    /// Which of the five this is, without what it carries — what the device list says beside a
    /// device that is not a node yet.
    #[must_use]
    pub fn reason(&self) -> FilingReason {
        match self {
            Self::Matched { .. } => FilingReason::Matched,
            Self::Ambiguous { .. } => FilingReason::Ambiguous,
            Self::Unmatched => FilingReason::Unmatched,
            Self::NoAddress => FilingReason::NoAddress,
            Self::NotAsked => FilingReason::NotAsked,
        }
    }
}

/// Why a device would be filed where it would be. Serialized as the snake_case token; never stored.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum FilingReason {
    /// Exactly one folder's IP range holds the address: it goes there.
    Matched,
    /// Two or more folders claim the address equally: it goes under the network's folder.
    Ambiguous,
    /// No folder's IP range holds the address: it goes under the network's folder.
    Unmatched,
    /// Meraki reports no address for it: it goes under the network's folder.
    NoAddress,
    /// The organization does not file by IP range: it goes under the network's folder.
    NotAsked,
    /// An MX whose network's LAN side has not been read yet (ADR-164 決定 28): its address is not
    /// known, so no import — automatic or by hand — takes it until a sync has read it (決定 39).
    /// Only the device list says this; an import never files a device under it.
    LanPending,
}

#[cfg(test)]
impl FilingReason {
    /// Every reason. Test-only, like `MerakiDeviceState::ALL`: nothing in production iterates them.
    const ALL: [Self; 6] = [
        Self::Matched,
        Self::Ambiguous,
        Self::Unmatched,
        Self::NoAddress,
        Self::NotAsked,
        Self::LanPending,
    ];
}

/// The addresses worth asking the match about: each usable one, once.
///
/// The unspecified address never reaches here — [`crate::meraki_inventory::usable_address`] drops it
/// where the address is parsed — because the importer writes `0.0.0.0` on a node that has none, and
/// a folder carrying `0.0.0.0/0` would otherwise collect every address-less device.
#[must_use]
pub fn addresses_to_match(addresses: &[Option<IpAddr>]) -> Vec<IpAddr> {
    let mut out: Vec<IpAddr> = Vec::new();
    for addr in addresses.iter().flatten() {
        if !out.contains(addr) {
            out.push(*addr);
        }
    }
    out
}

/// One [`Filing`] per device, in the order the devices were given.
///
/// `fold` is the answer for [`addresses_to_match`]. An address the fold does not mention at all is
/// `Unmatched` — the same fail-closed reading [`crate::groups::fold_prefix_matches`] gives a key the
/// query dropped.
#[must_use]
pub fn plan_filing(
    addresses: &[Option<IpAddr>],
    fold: &PrefixFold<IpAddr>,
    file_by_prefix: bool,
) -> Vec<Filing> {
    if !file_by_prefix {
        return addresses.iter().map(|_| Filing::NotAsked).collect();
    }
    let matched: HashMap<IpAddr, (Uuid, &str)> = fold
        .matched
        .iter()
        .map(|(addr, folder, prefix)| (*addr, (*folder, prefix.as_str())))
        .collect();
    let ambiguous: HashMap<IpAddr, usize> = fold
        .ambiguous
        .iter()
        .map(|(addr, folders)| (*addr, folders.len()))
        .collect();
    addresses
        .iter()
        .map(|addr| match addr {
            None => Filing::NoAddress,
            Some(a) => match (matched.get(a), ambiguous.get(a)) {
                (Some((folder, prefix)), _) => Filing::Matched {
                    folder: *folder,
                    prefix: (*prefix).to_owned(),
                },
                (None, Some(folders)) => Filing::Ambiguous { folders: *folders },
                (None, None) => Filing::Unmatched,
            },
        })
        .collect()
}

/// How the devices an import **created** were filed. Skipped devices are not counted.
///
/// The four add up to the number imported, except when the match was switched off — then all four
/// are zero, because nothing was asked and "unmatched" would be a claim about ranges nobody read.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, utoipa::ToSchema)]
pub struct MerakiFiled {
    /// Filed into the one folder whose IP range holds the device's address.
    pub matched: u32,
    /// Two or more folders claimed the address equally; filed under the network folder.
    pub ambiguous: u32,
    /// No folder's range holds the address; filed under the network folder.
    pub unmatched: u32,
    /// Meraki reported no address; filed under the network folder.
    pub no_address: u32,
}

impl MerakiFiled {
    /// Count one created device.
    pub fn count(&mut self, filing: &Filing) {
        match filing {
            Filing::Matched { .. } => self.matched += 1,
            Filing::Ambiguous { .. } => self.ambiguous += 1,
            Filing::Unmatched => self.unmatched += 1,
            Filing::NoAddress => self.no_address += 1,
            Filing::NotAsked => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(s: &str) -> IpAddr {
        s.parse().expect("test address")
    }

    fn fold(
        matched: &[(&str, u128, &str)],
        ambiguous: &[(&str, usize)],
        unmatched: &[&str],
    ) -> PrefixFold<IpAddr> {
        PrefixFold {
            matched: matched
                .iter()
                .map(|(a, g, p)| (ip(a), Uuid::from_u128(*g), (*p).to_owned()))
                .collect(),
            ambiguous: ambiguous
                .iter()
                .map(|(a, n)| (ip(a), (0..*n).map(|i| Uuid::from_u128(i as u128)).collect()))
                .collect(),
            unmatched: unmatched.iter().map(|a| ip(a)).collect(),
        }
    }

    #[test]
    fn each_usable_address_is_asked_about_once() {
        let asked = addresses_to_match(&[
            Some(ip("10.1.0.5")),
            None,
            Some(ip("10.1.0.5")),
            Some(ip("2001:db8::1")),
        ]);
        assert_eq!(asked, vec![ip("10.1.0.5"), ip("2001:db8::1")]);
    }

    #[test]
    fn every_device_gets_the_answer_for_its_own_address_in_order() {
        let f = fold(
            &[("10.1.0.5", 7, "10.1.0.0/24")],
            &[("10.2.0.5", 2)],
            &["10.3.0.5"],
        );
        let plan = plan_filing(
            &[
                Some(ip("10.3.0.5")),
                Some(ip("10.1.0.5")),
                None,
                Some(ip("10.2.0.5")),
                Some(ip("10.1.0.5")),
            ],
            &f,
            true,
        );
        let matched = Filing::Matched {
            folder: Uuid::from_u128(7),
            prefix: "10.1.0.0/24".to_owned(),
        };
        assert_eq!(
            plan,
            vec![
                Filing::Unmatched,
                matched.clone(),
                Filing::NoAddress,
                Filing::Ambiguous { folders: 2 },
                matched,
            ]
        );
    }

    #[test]
    fn only_a_single_claim_names_a_folder() {
        assert_eq!(
            Filing::Matched {
                folder: Uuid::from_u128(9),
                prefix: "10.0.0.0/8".to_owned()
            }
            .folder(),
            Some(Uuid::from_u128(9))
        );
        for f in [
            Filing::Ambiguous { folders: 2 },
            Filing::Unmatched,
            Filing::NoAddress,
            Filing::NotAsked,
        ] {
            assert_eq!(f.folder(), None, "{f:?} must fall to the network folder");
        }
    }

    #[test]
    fn an_address_the_fold_never_mentions_is_unmatched_not_matched() {
        let plan = plan_filing(&[Some(ip("192.0.2.1"))], &fold(&[], &[], &[]), true);
        assert_eq!(plan, vec![Filing::Unmatched]);
    }

    #[test]
    fn switched_off_nothing_is_matched_even_when_a_range_would_claim_it() {
        let f = fold(&[("10.1.0.5", 7, "10.1.0.0/24")], &[], &[]);
        let plan = plan_filing(&[Some(ip("10.1.0.5")), None], &f, false);
        assert_eq!(plan, vec![Filing::NotAsked, Filing::NotAsked]);
        let mut filed = MerakiFiled::default();
        for p in &plan {
            filed.count(p);
        }
        assert_eq!(
            filed,
            MerakiFiled::default(),
            "nothing was asked, so nothing is claimed"
        );
    }

    #[test]
    fn the_counts_add_up_to_what_was_created() {
        let mut filed = MerakiFiled::default();
        for f in [
            Filing::Matched {
                folder: Uuid::from_u128(1),
                prefix: "10.0.0.0/8".to_owned(),
            },
            Filing::Unmatched,
            Filing::Unmatched,
            Filing::NoAddress,
            Filing::Ambiguous { folders: 3 },
        ] {
            filed.count(&f);
        }
        assert_eq!(
            filed,
            MerakiFiled {
                matched: 1,
                ambiguous: 1,
                unmatched: 2,
                no_address: 1
            }
        );
    }

    /// The WebUI keys a sentence on each token (`meraki.devices.filing.<token>`), so the spelling
    /// is a contract. Only a match carries a folder; the others all mean the network's folder, or
    /// (`lan_pending`, ADR-164 決定 39) no import yet.
    #[test]
    fn every_reason_has_the_token_the_webui_keys_on_and_only_a_match_names_a_folder() {
        let tokens: Vec<String> = FilingReason::ALL
            .iter()
            .map(|r| serde_json::to_value(r).expect("serialize"))
            .map(|v| v.as_str().expect("a string token").to_owned())
            .collect();
        assert_eq!(
            tokens,
            [
                "matched",
                "ambiguous",
                "unmatched",
                "no_address",
                "not_asked",
                "lan_pending"
            ]
        );
        for f in [
            Filing::Matched {
                folder: Uuid::from_u128(1),
                prefix: "10.0.0.0/8".to_owned(),
            },
            Filing::Ambiguous { folders: 2 },
            Filing::Unmatched,
            Filing::NoAddress,
            Filing::NotAsked,
        ] {
            assert_eq!(
                f.folder().is_some(),
                f.reason() == FilingReason::Matched,
                "{f:?}"
            );
        }
    }
}
