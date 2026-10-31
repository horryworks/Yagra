// SPDX-License-Identifier: AGPL-3.0-only
//! How long a device's running configuration has gone unsaved (ADR-222).
//!
//! Cisco IOS and IOS-XE publish, in CISCO-CONFIG-MAN-MIB, the `sysUpTime` at which the running
//! configuration last changed (`ccmHistoryRunningLastChanged`) and at which the startup
//! configuration was last written (`ccmHistoryStartupLastChanged`). A running change newer than the
//! last startup write is a change a reload would lose. Core reads the three values out of one poll
//! result and adds [`METRIC`] to it, so an ordinary threshold rule pages on it — maintenance
//! windows, dwell and "never close on absence" come with that path rather than being written again.
//!
//! The value is the seconds since the **last** running change, not since the first unsaved one:
//! the MIB keeps only the latest change, so an operator still editing restarts the clock, which is
//! also the honest reading of "left unsaved".

/// The computed metric: seconds the running configuration has been left unsaved, `0` when saved.
pub const METRIC: &str = "config_unsaved_seconds";

/// `ccmHistoryRunningLastChanged`, as `sysUpTime` ticks (hundredths of a second).
pub const RUNNING_CHANGED: &str = "cisco_ccm_running_changed_ticks";
/// The OID of [`RUNNING_CHANGED`] (CISCO-CONFIG-MAN-MIB).
pub const RUNNING_CHANGED_OID: &str = "1.3.6.1.4.1.9.9.43.1.1.1.0";
/// `ccmHistoryStartupLastChanged`, as `sysUpTime` ticks.
pub const STARTUP_CHANGED: &str = "cisco_ccm_startup_changed_ticks";
/// The OID of [`STARTUP_CHANGED`] (CISCO-CONFIG-MAN-MIB).
pub const STARTUP_CHANGED_OID: &str = "1.3.6.1.4.1.9.9.43.1.1.3.0";
/// `sysUpTime.0`, which the standard SNMP template already collects under this name.
pub const UPTIME: &str = "snmp_sys_uptime_ticks";

/// A running change this soon after boot is the device loading its startup configuration, not an
/// edit. After a reload the startup history is `0` and the running history is a few ticks later,
/// so without this every rebooted device would read as unsaved.
///
/// ⚠️ Not measured on a real device yet (ADR-222): two minutes is a conservative guess at "the
/// boot-time load is over". A genuine edit inside it is missed until the next one.
pub const BOOT_LOAD_TICKS: f64 = 12_000.0;

/// `TimeTicks` is a 32-bit counter of hundredths of a second; it wraps after about 497 days.
const TICKS_WRAP: f64 = 4_294_967_296.0;

/// Seconds the running configuration has gone unsaved, from the three readings of one poll.
///
/// `0.0` when the last startup write is at or after the last running change, or when the only
/// running change is the boot-time load. ⚠️ A change and a save on opposite sides of a `sysUpTime`
/// wrap compare the wrong way round; that is one poll cycle every 497 days and is accepted.
#[must_use]
pub fn unsaved_seconds(uptime: f64, running_changed: f64, startup_changed: f64) -> f64 {
    if running_changed <= startup_changed {
        return 0.0;
    }
    if startup_changed == 0.0 && running_changed <= BOOT_LOAD_TICKS {
        return 0.0;
    }
    let elapsed = if uptime >= running_changed {
        uptime - running_changed
    } else {
        uptime + TICKS_WRAP - running_changed
    };
    elapsed / 100.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_save_after_the_last_change_reads_as_saved() {
        assert_eq!(unsaved_seconds(500_000.0, 300_000.0, 300_000.0), 0.0);
        assert_eq!(unsaved_seconds(500_000.0, 300_000.0, 400_000.0), 0.0);
    }

    #[test]
    fn a_change_after_the_last_save_counts_seconds_since_the_change() {
        assert_eq!(unsaved_seconds(500_000.0, 400_000.0, 300_000.0), 1_000.0);
    }

    #[test]
    fn the_boot_time_load_is_not_an_edit() {
        // After a reload: startup never written since boot, running loaded a moment in.
        assert_eq!(unsaved_seconds(9_000_000.0, 3_100.0, 0.0), 0.0);
        assert_eq!(unsaved_seconds(9_000_000.0, BOOT_LOAD_TICKS, 0.0), 0.0);
    }

    #[test]
    fn an_edit_after_the_boot_load_with_no_save_since_boot_is_unsaved() {
        assert_eq!(unsaved_seconds(1_000_000.0, 500_000.0, 0.0), 5_000.0);
    }

    /// The three names core looks for are the names something collects. A rename on either side
    /// would leave the metric silently never computed.
    #[test]
    fn the_inputs_are_collected_under_these_names() {
        let catalog = crate::collection::builtin_catalog();
        assert!(catalog.iter().any(|i| i.metric_name == UPTIME
            && i.oid == crate::oid::SYS_UPTIME_0
            && i.kind == crate::collection::CollectionKind::Scalar));
        let templates = crate::collection::builtin_templates();
        for (metric, oid) in [
            (RUNNING_CHANGED, RUNNING_CHANGED_OID),
            (STARTUP_CHANGED, STARTUP_CHANGED_OID),
        ] {
            assert!(
                templates
                    .iter()
                    .any(|t| t.items.iter().any(|i| i.metric_name == metric
                        && i.oid == oid
                        && i.kind == crate::collection::CollectionKind::Scalar)),
                "{metric} is collected by no built-in template"
            );
        }
    }

    #[test]
    fn a_wrapped_uptime_still_counts_forwards() {
        // The change happened 1,000 ticks before the wrap, the uptime is 2,000 ticks after it.
        let running = TICKS_WRAP - 1_000.0;
        assert_eq!(unsaved_seconds(2_000.0, running, 100_000.0), 30.0);
    }
}
