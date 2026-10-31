// SPDX-License-Identifier: AGPL-3.0-only
//! The commands Yagra may send a device to save its running configuration (ADR-222).
//!
//! **The one list of what Yagra writes to a network device** (decision 2): a command that prints
//! the startup configuration, one that compares running with startup, and the save itself with the
//! answers to the questions it asks. Nothing else is ever sent, and there is no free-text command
//! anywhere. Core hands these strings to the save script inside the bundled oxidized, which has no
//! list of its own; the seeded alert rules read [`compares`] to know which models report
//! `config_unsaved_seconds` over the command line.
//!
//! ⚠️ The VRP strings are taken from Huawei's documentation, not from a device yet; the IOS ones
//! from the IOS command reference. Each is to be confirmed on a real device before it is trusted
//! (ADR-222's remaining work), and a model is added here only with the same care.

use serde::Serialize;

/// What one model is sent. Every pattern is a Ruby regular expression the script matches the
/// device's output against, so it uses only syntax both Ruby and Rust's `regex` read the same way.
#[derive(Debug, Clone, Copy, Serialize)]
pub struct SaveCommands {
    /// oxidized's model name.
    pub model: &'static str,
    /// Prints the startup configuration — the copy kept before a save replaces it.
    pub startup: &'static str,
    /// Prints how running differs from startup, for a model Yagra cannot ask over SNMP. `None`
    /// for Cisco IOS, whose `config_unsaved_seconds` comes from CISCO-CONFIG-MAN-MIB.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub compare: Option<&'static str>,
    /// Matches the compare output when the two are the same.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub same: Option<&'static str>,
    /// The save command.
    pub save: &'static str,
    /// The questions the save may ask, each with the answer to send.
    pub confirms: &'static [(&'static str, &'static str)],
    /// Matches the output of a save that succeeded.
    pub success: &'static str,
    /// Matches the output of a save the device refused, usually for want of privilege.
    pub denied: &'static str,
}

/// Every model Yagra can save. A model missing here is never sent a command.
pub const SAVE_COMMANDS: &[SaveCommands] = &[
    SaveCommands {
        model: "ios",
        startup: "show startup-config",
        compare: None,
        same: None,
        // `write memory` asks nothing; `copy running-config startup-config` asks for a file name.
        save: "write memory",
        confirms: &[],
        success: r"\[OK\]",
        denied: r"(?i)(% ?invalid input|% ?authorization failed|access denied|not authorized)",
    },
    SaveCommands {
        model: "vrp",
        startup: "display saved-configuration",
        compare: Some("compare configuration"),
        same: Some(r"(?i)the current configuration is the same as the next startup configuration"),
        save: "save",
        confirms: &[
            (r"\[Y/N\]:?\s*$", "y"),
            // Some versions ask for the file name; Enter keeps the one the device boots from.
            (r"(?i)file name.*\]:?\s*$", ""),
        ],
        success: r"(?i)successfully",
        denied: r"(?i)(error: unrecognized command|permission|insufficient|authority)",
    },
];

/// The commands for `model`, if Yagra can save it.
#[must_use]
pub fn commands_for(model: &str) -> Option<&'static SaveCommands> {
    SAVE_COMMANDS.iter().find(|c| c.model == model)
}

/// Whether Yagra checks `model` for unsaved changes over the command line.
#[must_use]
pub fn compares(model: &str) -> bool {
    commands_for(model).is_some_and(|c| c.compare.is_some())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every pattern compiles, so a typo is a failed build rather than a script that never
    /// matches and reports every save as unanswered. Rust's `regex` is stricter than Ruby's on the
    /// syntax used here, so passing it is a fair proxy.
    #[test]
    fn every_pattern_compiles() {
        let mut checked = 0;
        for c in SAVE_COMMANDS {
            let mut patterns = vec![c.success, c.denied];
            patterns.extend(c.same);
            patterns.extend(c.confirms.iter().map(|(q, _)| *q));
            for p in patterns {
                regex::Regex::new(p).unwrap_or_else(|e| panic!("{}: {p}: {e}", c.model));
                checked += 1;
            }
        }
        assert!(checked >= 7, "only {checked} patterns compiled");
    }

    /// The table names only models a person can pick, and each once.
    #[test]
    fn every_model_is_an_oxidized_model_and_appears_once() {
        let mut seen = std::collections::BTreeSet::new();
        for c in SAVE_COMMANDS {
            assert!(crate::oxidized_model::is_model(c.model), "{}", c.model);
            assert!(seen.insert(c.model), "{} appears twice", c.model);
        }
    }

    /// The script test (`scripts/oxidized-script-test.sh`) drives the real save script against a
    /// fake device with these commands. It reads them from this generated file rather than a
    /// copy of its own, so the table it tests is the table that ships.
    #[test]
    fn the_committed_save_commands_fixture_is_current() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../scripts/oxidized-script-test/commands.json");
        let mut generated =
            serde_json::to_string_pretty(SAVE_COMMANDS).expect("serialize the save commands");
        generated.push('\n');
        if std::env::var_os("UPDATE_SAVE_COMMANDS").is_some() {
            std::fs::write(&path, &generated).expect("write commands.json");
            return;
        }
        let committed = std::fs::read_to_string(&path).unwrap_or_default();
        assert_eq!(
            committed, generated,
            "scripts/oxidized-script-test/commands.json is stale. Regenerate it with:\n    \
             UPDATE_SAVE_COMMANDS=1 cargo test -p yagra-common the_committed_save_commands_fixture_is_current"
        );
    }

    /// A compare command always comes with the pattern that reads it, and the reverse.
    #[test]
    fn compare_and_same_come_together() {
        for c in SAVE_COMMANDS {
            assert_eq!(c.compare.is_some(), c.same.is_some(), "{}", c.model);
        }
    }
}
