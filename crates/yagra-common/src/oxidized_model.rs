// SPDX-License-Identifier: AGPL-3.0-only
//! Which oxidized `model` a node's config is fetched with (ADR-210 stage 2).
//!
//! oxidized does not detect a device's platform: it is told the model per node, and the model
//! decides every command it types. Yagra already knows the platform as the built-in profile
//! Discovery classified the node into, so the model is looked up from that profile's name. A
//! profile with no row here has no model, and the node is not fetched until a person picks one.
//!
//! ⚠️ **Only rows someone is confident of.** A wrong model logs in and runs the wrong commands,
//! which stores an error page as the "config". A profile that holds two platforms has no row in
//! [`PROFILE_MODELS`]; it goes in [`DESCR_MODELS`] instead, where the device's own `sysDescr`
//! picks the platform (ADR-210 decision 90). The Cisco wireless controller is the first: it covers
//! both AireOS (`aireos`) and the Catalyst 9800 (IOS-XE), and only what each says tells them apart.

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
    // ADR-210 decision 89. An A10 says "Thunder Series ... ACOS 4.1.4" or "ACOS 5.2.1"; a
    // Huawei AC6508 runs VRP 5.170, which the CloudEngine's model already reads.
    ("A10 Thunder ADC", "acos"),
    ("Huawei wireless controller", "vrp"),
];

/// A profile that holds more than one platform → (a word its `sysDescr` contains, the model for
/// that platform), tried in order (ADR-210 decision 90). The word is lowercase and matched
/// case-insensitively as a substring — [`descr_has`] is the one definition, and the SQL that
/// lists nodes with a model builds the same test from this table. A `sysDescr` that contains none
/// of a profile's words has no model, exactly as a profile with no row has none.
pub const DESCR_MODELS: &[(&str, &str, &str)] = &[
    // AireOS answers its entire sysDescr as "Cisco Controller"; a Catalyst 9800 says
    // "Cisco IOS Software [...], C9800 Software (C9800_IOSXE-K9), Version 17.x".
    ("Cisco wireless controller", "cisco controller", "aireos"),
    ("Cisco wireless controller", "c9800", "ios"),
];

/// Whether `sys_descr` contains `word`, ignoring case: the match [`DESCR_MODELS`] is read with.
#[must_use]
pub fn descr_has(sys_descr: &str, word: &str) -> bool {
    sys_descr.to_lowercase().contains(word)
}

/// The model a node is fetched with when no person picked one: its profile's row, else the first
/// [`DESCR_MODELS`] row for its profile whose word its `sysDescr` contains.
#[must_use]
pub fn model_for(profile_name: &str, sys_descr: Option<&str>) -> Option<&'static str> {
    model_for_profile(profile_name).or_else(|| {
        let descr = sys_descr?;
        DESCR_MODELS
            .iter()
            .find(|(profile, word, _)| *profile == profile_name && descr_has(descr, word))
            .map(|(_, _, model)| *model)
    })
}

/// The models a person may pick for a node by hand. A subset of what oxidized 0.37 ships: the
/// platforms Yagra's classification knows of, plus `linuxgeneric` for a server. A model outside this
/// list is refused at the API edge, so the value handed to oxidized is always one of these words.
pub const MODELS: &[&str] = &[
    "acos",
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
    fn the_wireless_controller_has_no_profile_wide_model_because_two_platforms_share_it() {
        assert_eq!(model_for_profile("Cisco wireless controller"), None);
        assert_eq!(
            model_for_profile("Cisco Catalyst switch (IOS/IOS-XE)"),
            Some("ios")
        );
    }

    /// The two shapes a Cisco wireless controller answers, as measured on a deployment.
    #[test]
    fn the_wireless_controllers_sysdescr_picks_its_platform() {
        let wlc = "Cisco wireless controller";
        assert_eq!(model_for(wlc, Some("Cisco Controller")), Some("aireos"));
        assert_eq!(
            model_for(
                wlc,
                Some(
                    "Cisco IOS Software [Cupertino], C9800 Software (C9800_IOSXE-K9), \
                     Version 17.9.4a, RELEASE SOFTWARE (fc3)"
                )
            ),
            Some("ios")
        );
        assert_eq!(model_for(wlc, Some("Cisco Business Wireless")), None);
        assert_eq!(model_for(wlc, None), None);
        // The word decides only for its own profile.
        assert_eq!(model_for("Generic SNMP", Some("Cisco Controller")), None);
    }

    #[test]
    fn a_profile_with_one_platform_ignores_its_sysdescr() {
        assert_eq!(model_for("A10 Thunder ADC", None), Some("acos"));
        assert_eq!(
            model_for("Huawei wireless controller", Some("Huawei AC6508")),
            Some("vrp")
        );
    }

    #[test]
    fn every_descr_row_names_a_builtin_profile_with_no_profile_wide_model() {
        let profiles: Vec<&str> = crate::collection::builtin_profiles()
            .into_iter()
            .map(|p| p.name)
            .collect();
        for (profile, word, model) in DESCR_MODELS {
            assert!(
                profiles.contains(profile),
                "{profile} is not a built-in profile"
            );
            assert!(is_model(model), "{model} is not in MODELS");
            // The SQL lowers the sysDescr and searches for the word as written.
            assert_eq!(*word, word.to_lowercase(), "{word} must be lowercase");
            assert!(!word.is_empty());
            // A profile-wide row would win, and this one would never be read.
            assert_eq!(model_for_profile(profile), None, "{profile} has both");
        }
    }

    #[test]
    fn the_pickable_models_are_sorted_and_unique() {
        let mut sorted = MODELS.to_vec();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted, MODELS);
    }
}
