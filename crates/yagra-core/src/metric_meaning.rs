// SPDX-License-Identifier: AGPL-3.0-only
//! What a metric measures, in one sentence — **the English source of truth** (ADR-079 決定 4).
//!
//! These sentences lived only in `web/src/locales/{en,ja}/metrics.json`, which made them something
//! the WebUI knew and `/mcp` did not: the alert-rule table has a "What it measures" column, and an
//! MCP client reading the same ruleset got a bare metric name. That is a read-parity gap in the
//! sense ADR-042 means it — a question the screen answers and the tool cannot — even though no REST
//! route was missing, because there was no route at all.
//!
//! **English is canonical and lives here; Japanese is a translation and stays in web.**
//! `web/src/locales/en/metricMeanings.json` is *generated* from this table
//! (`the_committed_en_metric_meanings_are_current`), so the two cannot drift — the mirror is
//! deleted rather than guarded (`extensibility.md` §2). `web/src/locales/ja/metricMeanings.json`
//! is hand-written, and the existing EN⟷JA parity gate is what demands a sentence for a new metric.
//!
//! ⚠️ **Why the sentences are not in `mib_catalog.description`**, which exists and is null on every
//! row: filling it would need a corrective migration for the seeded rows (a stable seed id's
//! `ON CONFLICT` shadows the stale one), and would leave the meaning owned by *both* this table and
//! the database. One source, and it is this file (ADR-079 決定 5).
//!
//! Gauges only, by construction. A counter can carry no threshold rule — a fixed bound cannot be
//! evaluated against a monotonic value (ADR-012) — so the picker never offers one and the rule
//! table never shows one; a sentence written for `if_hc_in_octets` is a sentence nobody can reach.

/// Metrics Yagra's own checks emit — the reachability probes and the URL / DNS / Meraki monitors.
///
/// Hand-written because these names are constants scattered across `yagra-common`,
/// `yagra-transport` and the poller with no collection catalogue behind them, and they have no
/// `mib_catalog` row either — which is exactly why they need listing: nothing else knows they
/// exist. This list used to live only in `web/src/lib/metricMeaning.ts`, whose doc comment said
/// there was "no catalog on the Rust side" to generate from. There is now.
///
/// It has two readers: [`metric_source`], which reports where a number comes from, and the test
/// that makes [`METRIC_MEANINGS`] checkable — without the list, a sentence for a metric nothing
/// collects would look identical to a sentence for one that does.
pub const CHECK_METRICS: [&str; 18] = [
    "__liveness__",
    "icmp_rtt_ms",
    "icmp_loss_pct",
    "snmp_up",
    "snmp_neighbor_count",
    "snmp_l3_address_count",
    "snmp_routing_adjacency_count",
    "snmp_arp_entry_count",
    "http_up",
    "http_status_code",
    "http_response_time_ms",
    "http_body_match",
    "ssl_cert_days_to_expiry",
    "dns_up",
    "dns_resolve_ms",
    "dns_answer_count",
    "dns_chain_length",
    "meraki_device_up",
];

/// One sentence per metric, sorted by metric name.
///
/// Sorted, and pinned sorted by a test: the generated locale file is written in this order, so an
/// out-of-order row would surface as a spurious diff on every unrelated regeneration.
pub const METRIC_MEANINGS: [(&str, &str); 97] = [
    ("__liveness__", "Did the node answer its checks at all. Carries no bounds — a node either responded or it did not — so only the breach count applies. It is the only rule covering a monitor Yagra never pings (a URL, a DNS name, a Meraki device), and the only one whose alerts roll up under a failed parent instead of paging once per affected node."),
    ("asa_current_connections", "Connections currently held by the ASA, one row per connection statistic the firewall reports (CISCO-FIREWALL-MIB)."),
    ("bgp_peer_admin_status", "Whether the BGP session is administratively started. 1 = stop, 2 = start. A peer down while this reads 2 is an unplanned outage."),
    ("bgp_peer_state", "BGP session state. 1 = idle, 2 = connect, 3 = active, 4 = opensent, 5 = openconfirm, 6 = established. Only 6 is a working session, so the usual rule is below 6."),
    ("cisco_cemp_mem_free", "Free bytes in an enhanced memory pool (cempMemPoolFree). Newer IOS-XE and IOS-XR answer here rather than on the older pool table."),
    ("cisco_cemp_mem_used", "Used bytes in an enhanced memory pool (cempMemPoolUsed). Pair it with the free value to read a percentage."),
    ("cisco_cpu_5min", "CPU utilisation averaged over the last five minutes, in percent (cpmCPUTotal5minRev). One row per CPU on a multi-CPU chassis."),
    ("cisco_cpu_mem_free", "Memory free to the CPU, in kilobytes (cpmCPUMemoryFree)."),
    ("cisco_cpu_mem_used", "Memory in use by the CPU, in kilobytes (cpmCPUMemoryUsed)."),
    ("cisco_env_temp", "A chassis temperature sensor reading in degrees Celsius (ciscoEnvMonTemperatureStatusValue). One row per sensor."),
    ("cisco_fan_state", "Fan health. 1 = normal, 2 = warning, 3 = critical, 4 = shutdown, 5 = not present, 6 = not functioning."),
    ("cisco_fantray_state", "Fan-tray health on chassis switches. 1 = unknown, 2 = up, 3 = down, 4 = warning."),
    ("cisco_fru_power_state", "Power state of a field-replaceable unit. 2 = on; every other value is a form of off or failed (3 = off by admin, 4 = denied, 5 = off on power, 6 = off on temperature, 7 = off on fan failure, 8 = failed)."),
    ("cisco_ike_active_tunnels", "IKE (phase 1) tunnels currently established on this device."),
    ("cisco_ipsec_active_tunnels", "IPsec (phase 2) tunnels currently established on this device."),
    ("cisco_mem_free", "Free bytes in a memory pool (ciscoMemoryPoolFree). The classic IOS pool table; newer platforms answer on the cemp metrics instead."),
    ("cisco_mem_used", "Used bytes in a memory pool (ciscoMemoryPoolUsed)."),
    ("cisco_psu_state", "Power-supply health. 1 = normal, 2 = warning, 3 = critical, 4 = shutdown, 5 = not present, 6 = not functioning."),
    ("cisco_ra_sessions", "Remote-access VPN sessions currently connected (AnyConnect / SSL VPN)."),
    ("cisco_ra_users", "Distinct users currently connected over remote-access VPN. Lower than the session count when one user has several sessions."),
    ("cisco_temp_c", "Chassis temperature in degrees Celsius, from the Cisco sensor table. Only the sensors that reach no port — per-port optical readings are reported as light level instead (ADR-070)."),
    ("dns_answer_count", "How many answers the final response carried."),
    ("dns_chain_length", "How many hops the resolution chain took (CNAME → … → A/AAAA)."),
    ("dns_resolve_ms", "How long the resolution took, in milliseconds."),
    ("dns_up", "Did the name resolve. 1 = yes, 0 = any failure — NXDOMAIN, SERVFAIL, REFUSED, timeout or a CNAME loop."),
    ("ent_sensor_value", "A physical sensor reading from ENTITY-SENSOR-MIB. ⚠️ The unit differs per row — temperature, voltage, current, RPM and dBm all arrive on this one metric — so a single threshold across all rows is rarely meaningful."),
    ("fortinet_cpu_usage", "CPU utilisation of the FortiGate, in percent."),
    ("fortinet_mem_usage", "Memory utilisation of the FortiGate, in percent. Above roughly 80 the unit enters conserve mode and starts dropping sessions."),
    ("fortinet_sessions", "Sessions currently held in the FortiGate session table."),
    ("fortinet_sslvpn_users", "Users currently logged in over SSL VPN. One row per virtual domain."),
    ("fortinet_vpn_tunnels_up", "IPsec tunnels currently up. Alert below the number you expect to be permanently established."),
    ("hr_processor_load", "Per-CPU load over the last minute, in percent (hrProcessorLoad). One row per processor; the node-level view takes the highest."),
    ("hr_storage_size", "Total size of a storage area, in allocation units (hrStorageSize). ⚠️ Not bytes — multiply by the unit size the device reports for that row."),
    ("hr_storage_used", "Used portion of a storage area, in allocation units (hrStorageUsed). Same unit caveat as the size."),
    ("hr_system_processes", "Processes currently running on the host (hrSystemProcesses)."),
    ("http_body_match", "Did the response body match the rule configured on the monitor. 1 = matched, 0 = did not."),
    ("http_response_time_ms", "How long the monitor’s request took, in milliseconds."),
    ("http_status_code", "The HTTP status code the monitor received."),
    ("http_up", "Did the URL answer, with the status the monitor expects. 1 = yes, 0 = unreachable or the wrong status."),
    ("huawei_cpu_usage", "CPU utilisation in percent (hwEntityCpuUsage). One row per board or entity that has a CPU."),
    ("huawei_mem_free", "Free memory in bytes, from HUAWEI-MEMORY-MIB. Pair it with the total to read a percentage."),
    ("huawei_mem_total", "Installed memory in bytes, from HUAWEI-MEMORY-MIB."),
    ("huawei_mem_usage", "Memory utilisation in percent (hwEntityMemUsage) — the device’s own figure, so it needs no arithmetic."),
    ("huawei_temp", "Entity temperature in degrees Celsius (hwEntityTemperature). One row per board or sensor."),
    ("huawei_usg_half_open_sessions", "Half-open sessions on the firewall — connections that started a handshake and never finished it. A rising count is the usual signature of a SYN flood or a dead peer."),
    ("huawei_usg_icmp_sessions", "ICMP sessions currently held by the firewall."),
    ("huawei_usg_session_setup_rate", "New sessions the firewall is establishing per second."),
    ("huawei_usg_tcp_sessions", "TCP sessions currently held by the firewall."),
    ("huawei_usg_total_sessions", "All sessions the firewall currently holds, across every protocol."),
    ("huawei_usg_udp_sessions", "UDP sessions currently held by the firewall."),
    ("icmp_loss_pct", "Share of ICMP probes that got no reply, 0–100. A partial loss means the link is degraded but the node is still answering. 100% means it has stopped answering altogether, and that is what the Reachability rule raises a critical for — so this rule is about degradation, not about an outage."),
    ("icmp_rtt_ms", "Round-trip time of the ICMP probe, in milliseconds."),
    ("if_admin_status", "Whether the port is administratively enabled. 1 = up, 2 = down, 3 = testing. A port down while this reads 1 is unplanned; a port down while this reads 2 was shut deliberately."),
    ("if_high_speed", "The port’s nominal bandwidth in Mbps (ifHighSpeed). 0 on a port that reports no speed, which is why it is worth alerting on: a link that renegotiated to 100 on a gigabit port shows up here."),
    ("if_in_bps", "Inbound traffic in bits per second. Computed from the octet counter at evaluation time, not collected, so it has no chart of its own; the Interfaces tab draws the same figure. Unlike the percentage it needs no denominator, so it is the only way to alert on a port whose speed the device does not report."),
    ("if_in_util_pct", "Inbound traffic as a percentage of the port’s own speed. Computed from the octet counter and the speed the device reports — it is not collected, so it has no chart of its own; the Interfaces tab draws the same figure. A port whose speed is unknown cannot be evaluated and never fires."),
    ("if_oper_status", "Whether the port is actually passing traffic. 1 = up, 2 = down, 3 = testing, 4 = unknown, 5 = dormant, 6 = not present, 7 = lower layer down."),
    ("if_out_bps", "Outbound traffic in bits per second. Computed from the octet counter at evaluation time, not collected, so it has no chart of its own; the Interfaces tab draws the same figure. Unlike the percentage it needs no denominator, so it is the only way to alert on a port whose speed the device does not report."),
    ("if_out_util_pct", "Outbound traffic as a percentage of the port’s own speed. Separate from the inbound figure on purpose: a link is asymmetric more often than not, and a rule on one direction says nothing about the other."),
    ("if_rx_power_dbm", "Optical receive power of the transceiver, in dBm. Typically −20 to 0; falling toward the receiver’s sensitivity floor is the early sign of a dirty or failing fibre."),
    ("if_tx_power_dbm", "Optical transmit power of the transceiver, in dBm. A falling value points at the transceiver itself rather than the fibre."),
    ("juniper_buffer_util", "Buffer-pool utilisation in percent (jnxOperatingBuffer). One row per operating subject — routing engine, FPC, PIC."),
    ("juniper_cpu_1min", "CPU utilisation in percent (jnxOperatingCPU). ⚠️ Despite the metric name this is the instantaneous value, not a one-minute average, so it is spikier than a load figure."),
    ("juniper_temp", "Temperature in degrees Celsius (jnxOperatingTemp). One row per operating subject."),
    ("meraki_device_up", "Does the Meraki dashboard report the device online. 1 = online, 0 = offline."),
    ("mikrotik_cpu_temp", "Temperature reported by RouterOS (mtxrHlTemperature). ⚠️ Some models report degrees Celsius and others tenths of a degree — read the live value once before choosing a bound."),
    ("mikrotik_voltage", "Input voltage reported by RouterOS (mtxrHlVoltage). ⚠️ Usually tenths of a volt, so 240 means 24.0 V — confirm against the live value."),
    ("nxos_cpu_util", "CPU utilisation of the supervisor, in percent (NX-OS)."),
    ("nxos_mem_util", "Memory utilisation of the supervisor, in percent (NX-OS)."),
    ("panos_gp_active_tunnels", "GlobalProtect tunnels currently connected to this gateway."),
    ("panos_session_util_pct", "Session table utilisation in percent — the firewall’s own figure, so it already accounts for the platform’s limit."),
    ("panos_sessions_active", "Sessions currently active on the firewall."),
    ("poe_power_capacity_w", "Power the PoE supply can deliver, in watts (pethMainPsePower). One row per PoE group."),
    ("poe_power_consumed_w", "Power the connected devices are drawing, in watts (pethMainPseConsumptionPower). Compare it with the capacity to see how much budget is left."),
    ("prt_alert_severity", "Severity of a printer alert. 1 = other, 3 = critical, 4 = warning, 5 = warning (binary change). One row per outstanding alert."),
    ("prt_marker_life_count", "Pages the print engine has produced over its life (prtMarkerLifeCount). Useful for consumable planning, not for spotting a fault."),
    ("snmp_arp_entry_count", "How many ARP entries the last walk found."),
    ("snmp_l3_address_count", "How many L3 addresses the last walk found."),
    ("snmp_neighbor_count", "How many CDP/LLDP neighbours the last walk found."),
    ("snmp_routing_adjacency_count", "How many OSPF/BGP adjacencies the last walk found."),
    ("snmp_sys_uptime_ticks", "Time since the SNMP agent last restarted, in hundredths of a second (sysUpTime). ⚠️ Divide by 100 for seconds — one day is 8,640,000. It wraps after about 497 days."),
    ("snmp_up", "Did the SNMP agent answer this poll. 1 = at least one value came back, 0 = nothing came back or the request failed."),
    ("ssl_cert_days_to_expiry", "Days until the TLS certificate expires; negative once it already has."),
    ("tcp_curr_estab", "TCP connections currently in the established state (tcpCurrEstab)."),
    ("ucd_cpu_idle_pct", "CPU idle time in percent (ssCpuIdle). This is the inverse of load, so alert *below* a bound, not above."),
    ("ucd_disk_used_pct", "Disk usage in percent (dskPercent). One row per filesystem configured for monitoring in snmpd.conf."),
    ("ucd_load_15min", "Fifteen-minute load average × 100 (laLoadInt). ⚠️ 100 means a load of 1.00 — set the bound in hundredths."),
    ("ucd_load_1min", "One-minute load average × 100 (laLoadInt). ⚠️ 100 means a load of 1.00 — set the bound in hundredths."),
    ("ucd_load_5min", "Five-minute load average × 100 (laLoadInt). ⚠️ 100 means a load of 1.00 — set the bound in hundredths."),
    ("ucd_mem_avail_kb", "Free physical memory in kilobytes (memAvailReal). On Linux this excludes the page cache, so it reads lower than \"available\"."),
    ("ucd_mem_total_kb", "Installed physical memory in kilobytes (memTotalReal)."),
    ("ucd_swap_avail_kb", "Free swap in kilobytes (memAvailSwap). A host that has started consuming swap is usually already in trouble."),
    ("ucd_swap_total_kb", "Configured swap in kilobytes (memTotalSwap)."),
    ("ups_battery_status", "Battery condition. 1 = unknown, 2 = normal, 3 = low, 4 = depleted. Anything above 2 needs attention."),
    ("ups_charge_remaining_pct", "Estimated battery charge remaining, in percent."),
    ("ups_minutes_remaining", "Estimated run time left on battery, in minutes. Meaningful only while the UPS is actually on battery."),
    ("ups_output_load_pct", "Output load as a percentage of the UPS’s rated capacity. One row per output line."),
];

/// Where a metric comes from — the one fact about it that changes how it can be *used*.
///
/// A model that does not know `if_in_util_pct` is derived will ask `query_metrics` for it and get
/// nothing, because it exists in no time series at all: it is computed at evaluation time from a
/// counter rate and the port's own speed (ADR-012/ADR-076). The same distinction is what the
/// WebUI's metric picker groups by, so it is a property of the vocabulary rather than a WebUI
/// concern.
#[must_use]
pub fn metric_source(metric: &str) -> &'static str {
    if CHECK_METRICS.contains(&metric) {
        // Emitted by one of Yagra's own probes rather than read off a device.
        "check"
    } else if crate::interface_util::DERIVED_INTERFACE_METRICS.contains(&metric) {
        // Computed per port at evaluation time; queryable through no series.
        "derived"
    } else {
        // Collected by a metric set — `get_config(kind=mib_catalog)` has its OID.
        "collected"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    /// `binary_search_by_key` is only a lookup if the table is ordered, and an out-of-order row
    /// does not fail to compile — it just stops being findable, for that one metric, silently.
    #[test]
    fn the_table_is_sorted_and_holds_each_metric_once() {
        let names: Vec<&str> = METRIC_MEANINGS.iter().map(|(n, _)| *n).collect();
        let mut sorted = names.clone();
        sorted.sort_unstable();
        assert_eq!(
            names, sorted,
            "METRIC_MEANINGS must stay sorted by metric name"
        );
        assert_eq!(
            names.iter().collect::<BTreeSet<_>>().len(),
            names.len(),
            "a metric appears twice; the second sentence is unreachable"
        );
        // Sortedness is not decoration: the generated locale file is written in this order, so an
        // out-of-order row would show up as a spurious diff on every unrelated regeneration.
    }

    /// Every metric is filed under one of the three sources, and each source has members.
    ///
    /// The floor matters more than the mapping: a `metric_source` that answered `"collected"` for
    /// everything would be indistinguishable from a correct one on any single example, and
    /// `"collected"` is the arm that needs no list to reach.
    #[test]
    fn every_metric_is_filed_under_a_source_and_every_source_is_used() {
        let mut counts: std::collections::BTreeMap<&str, usize> = std::collections::BTreeMap::new();
        for (name, _) in METRIC_MEANINGS {
            *counts.entry(metric_source(name)).or_insert(0) += 1;
        }
        assert_eq!(
            counts.keys().copied().collect::<Vec<_>>(),
            ["check", "collected", "derived"],
            "a source with no members means the list behind it drifted out of the table"
        );
        assert_eq!(counts["check"], CHECK_METRICS.len());
        assert_eq!(
            counts["derived"],
            crate::interface_util::DERIVED_INTERFACE_METRICS.len()
        );
        // `__liveness__` is the one an MCP client is most likely to meet first, and the one whose
        // source is least guessable from its name.
        assert_eq!(metric_source(crate::alerts::LIVENESS), "check");
        assert_eq!(metric_source("if_in_util_pct"), "derived");
        assert_eq!(metric_source("cisco_cpu_5min"), "collected");
    }

    /// Every metric an operator can put a threshold rule on has a sentence, **and nothing else does**.
    ///
    /// Equality in both directions on purpose. A missing sentence leaves a rule the WebUI renders
    /// with an em dash and an MCP client cannot explain at all; an extra one is a sentence nobody
    /// can reach, which is what rots first because nothing reads it. The set is *derived* — from
    /// the collection catalogue, the check list and the derived-metric list — so adding a template
    /// in Rust fails here until the sentence is written, which is the whole reason this table sits
    /// beside the catalogue rather than in a locale file.
    ///
    /// Gauges only: see the module doc for why a counter is deliberately unexplained.
    #[test]
    fn every_metric_a_rule_can_name_has_a_sentence_and_no_others_do() {
        let mut expected: BTreeSet<&str> = crate::mib::builtin_mib_rows()
            .into_iter()
            .filter(|(item, _)| item.metric_kind == yagra_common::MetricKind::Gauge)
            .map(|(item, _)| {
                // Leaked so the set can borrow uniformly; this is a test, and the alternative is a
                // second owned collection that says nothing extra.
                &*Box::leak(item.metric_name.into_boxed_str())
            })
            .collect();
        expected.extend(CHECK_METRICS);
        expected.extend(crate::interface_util::DERIVED_INTERFACE_METRICS);

        // A floor, so "the catalogue query stopped matching" cannot pass as "everything is
        // explained": an empty expectation would make the comparison below vacuous.
        assert!(
            expected.len() > 50,
            "only {} explainable metrics found — the catalogue walk drifted",
            expected.len()
        );

        let have: BTreeSet<&str> = METRIC_MEANINGS.iter().map(|(n, _)| *n).collect();
        let missing: Vec<&&str> = expected.difference(&have).collect();
        let orphaned: Vec<&&str> = have.difference(&expected).collect();
        assert!(
            missing.is_empty(),
            "these metrics can carry a threshold rule but have no sentence: {missing:?}"
        );
        assert!(
            orphaned.is_empty(),
            "these sentences name nothing Yagra collects or derives: {orphaned:?}"
        );
    }

    /// The generated English locale file, exactly as it is committed.
    ///
    /// A flat `{metric: sentence}` object rather than the old `{"meaning": {…}}` nesting: it is a
    /// whole i18n namespace now, so the wrapper key would be a level every lookup pays for and
    /// nothing uses.
    fn en_locale_json() -> String {
        let map: serde_json::Map<String, serde_json::Value> = METRIC_MEANINGS
            .iter()
            .map(|(name, sentence)| ((*name).to_owned(), serde_json::Value::from(*sentence)))
            .collect();
        let mut out = serde_json::to_string_pretty(&map).expect("serialize metric meanings");
        out.push('\n');
        out
    }

    #[test]
    fn the_committed_en_metric_meanings_are_current() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../web/src/locales/en/metricMeanings.json");
        let generated = en_locale_json();

        if std::env::var_os("UPDATE_METRIC_MEANINGS").is_some() {
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir).expect("create web/src/locales/en");
            }
            std::fs::write(&path, &generated).expect("write metricMeanings.json");
            return;
        }

        let committed = std::fs::read_to_string(&path).unwrap_or_default();
        assert_eq!(
            committed, generated,
            "web/src/locales/en/metricMeanings.json is stale. Regenerate it with:\n    \
             UPDATE_METRIC_MEANINGS=1 cargo test -p yagra-core the_committed_en_metric_meanings_are_current\n\
             then write the Japanese sentence for any new metric in \
             web/src/locales/ja/metricMeanings.json (npm run i18n:check will name it)."
        );
    }
}
