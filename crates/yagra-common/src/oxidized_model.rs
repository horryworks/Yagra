// SPDX-License-Identifier: AGPL-3.0-only
//! Which oxidized `model` a node's config is fetched with (ADR-210 stage 2).
//!
//! oxidized does not detect a device's platform: it is told the model per node, and the model
//! decides every command it types. Yagra already knows the platform as the built-in profile
//! Discovery classified the node into, so the model is looked up from that profile's name. A
//! profile with no row here has no model, and the node is not fetched until a person picks one.
//!
//! ⚠️ **Only rows someone is confident of.** A wrong model logs in and runs the wrong commands,
//! which stores an error page as the "config". The Cisco wireless controller profile is absent on
//! purpose: it covers both AireOS (`aireos`) and the Catalyst 9800 (IOS-XE), which nothing here can
//! tell apart.

/// Built-in profile name → oxidized model. Every name must be one of
/// [`crate::collection::builtin_profiles`]; a test holds that.
pub const PROFILE_MODELS: &[(&str, &str)] = &[
    ("Cisco IOS/IOS-XE router", "ios"),
    ("Cisco Catalyst switch (IOS/IOS-XE)", "ios"),
    ("Cisco Nexus switch (NX-OS)", "nxos"),
    ("Cisco IOS-XR router", "iosxr"),
    ("Cisco ASA firewall", "asa"),
    ("Juniper SRX firewall", "junos"),
    ("Juniper MX router", "junos"),
    ("Juniper EX/QFX switch", "junos"),
    ("Huawei NE/AR router", "vrp"),
    ("Huawei CloudEngine switch", "vrp"),
    ("Huawei USG firewall", "vrp"),
    ("Arista EOS switch", "eos"),
    ("Fortinet FortiGate", "fortios"),
    ("Palo Alto PAN-OS firewall", "panos"),
    ("MikroTik RouterOS", "routeros"),
];

/// The models a person may pick for a node by hand. A subset of what oxidized 0.37 ships: the
/// platforms Yagra's classification knows of, plus `linuxgeneric` for a server. A model outside this
/// list is refused at the API edge, so the value handed to oxidized is always one of these words.
pub const MODELS: &[&str] = &[
    "aireos",
    "aoscx",
    "asa",
    "awplus",
    "comware",
    "eos",
    "fortios",
    "ios",
    "iosxr",
    "junos",
    "linuxgeneric",
    "necix",
    "nxos",
    "panos",
    "procurve",
    "routeros",
    "vrp",
    "yamaha",
];

/// The model a built-in profile is fetched with, or `None` when no row is confident of one.
#[must_use]
pub fn model_for_profile(profile_name: &str) -> Option<&'static str> {
    PROFILE_MODELS
        .iter()
        .find(|(name, _)| *name == profile_name)
        .map(|(_, model)| *model)
}

/// Whether `model` is one a person may pick.
#[must_use]
pub fn is_model(model: &str) -> bool {
    MODELS.contains(&model)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_row_names_a_builtin_profile_and_a_pickable_model() {
        let profiles: Vec<&str> = crate::collection::builtin_profiles()
            .into_iter()
            .map(|p| p.name)
            .collect();
        for (profile, model) in PROFILE_MODELS {
            assert!(
                profiles.contains(profile),
                "{profile} is not a built-in profile"
            );
            assert!(is_model(model), "{model} is not in MODELS");
        }
    }

    #[test]
    fn the_wireless_controller_has_no_model_because_two_platforms_share_it() {
        assert_eq!(model_for_profile("Cisco wireless controller"), None);
        assert_eq!(
            model_for_profile("Cisco Catalyst switch (IOS/IOS-XE)"),
            Some("ios")
        );
    }

    #[test]
    fn the_pickable_models_are_sorted_and_unique() {
        let mut sorted = MODELS.to_vec();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted, MODELS);
    }
}
