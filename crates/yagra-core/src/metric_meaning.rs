// SPDX-License-Identifier: AGPL-3.0-only
//! What a metric measures, in one sentence — **the English source of truth** (ADR-079 decision 4).
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
//! the database. One source, and it is this file (ADR-079 decision 5).
//!
//! Gauges only, by construction. A counter can carry no threshold rule — a fixed bound cannot be
//! evaluated against a monotonic value (ADR-012) — so the picker never offers one and the rule
//! table never shows one; a sentence written for `if_hc_in_octets` is a sentence nobody can reach.

/// Which of Yagra's own probes a check metric comes from (ADR-046 Inc.8).
///
/// The node Overview files its generic cards by this: the ICMP pair under the ICMP section, the
/// `snmp_*` checks beside the standard-MIB readings, and a URL / DNS / Meraki monitor's leftovers
/// under a heading of their own. It is written down here rather than inferred from the name
/// prefix, for the reason Inc.6 decision J gave for units: a rule from the spelling reads as a
/// measurement and is a coincidence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckFamily {
    Icmp,
    Snmp,
    Url,
    Dns,
    Meraki,
    /// A wireless AP's numbers, as its controller reports them (ADR-064).
    Wlan,
}

impl CheckFamily {
    /// Every family, in the order the Overview sections them.
    ///
    /// Test vocabulary: production code reads the family *table*, never the variant list, so
    /// this is `cfg(test)` rather than dead code with an `allow` on it.
    #[cfg(test)]
    pub const ALL: [CheckFamily; 6] = [
        CheckFamily::Icmp,
        CheckFamily::Snmp,
        CheckFamily::Url,
        CheckFamily::Dns,
        CheckFamily::Meraki,
        CheckFamily::Wlan,
    ];

    /// The token the generated catalog carries (`web/src/api/metricCatalog.json`), and the key
    /// the WebUI builds its section heading from (`nodes:overview.family.<token>`).
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            CheckFamily::Icmp => "icmp",
            CheckFamily::Snmp => "snmp",
            CheckFamily::Url => "url",
            CheckFamily::Dns => "dns",
            CheckFamily::Meraki => "meraki",
            CheckFamily::Wlan => "wlan",
        }
    }
}

/// Metrics Yagra's own checks emit — the reachability probes and the URL / DNS / Meraki monitors
/// — each with the probe it belongs to.
///
/// Hand-written because these names are constants scattered across `yagra-common`,
/// `yagra-transport` and the poller with no collection catalogue behind them, and they have no
/// `mib_catalog` row either — which is exactly why they need listing: nothing else knows they
/// exist. This list used to live only in `web/src/lib/metricMeaning.ts`, whose doc comment said
/// there was "no catalog on the Rust side" to generate from. There is now, and since Inc.8 the
/// WebUI's copy is *derived* from the generated catalog rather than kept by hand.
///
/// `__liveness__` is the one row with no family: it is the liveness rule's sentinel, not a
/// series, so the catalog generator skips it and no Overview card can ever carry it.
pub const CHECK_FAMILIES: [(&str, Option<CheckFamily>); 40] = [
    ("__liveness__", None),
    ("icmp_rtt_ms", Some(CheckFamily::Icmp)),
    ("icmp_loss_pct", Some(CheckFamily::Icmp)),
    ("snmp_up", Some(CheckFamily::Snmp)),
    ("snmp_walk_complete", Some(CheckFamily::Snmp)),
    ("snmp_neighbor_count", Some(CheckFamily::Snmp)),
    ("snmp_l3_address_count", Some(CheckFamily::Snmp)),
    ("snmp_routing_adjacency_count", Some(CheckFamily::Snmp)),
    ("snmp_arp_entry_count", Some(CheckFamily::Snmp)),
    ("http_up", Some(CheckFamily::Url)),
    ("http_status_code", Some(CheckFamily::Url)),
    ("http_response_time_ms", Some(CheckFamily::Url)),
    ("http_body_match", Some(CheckFamily::Url)),
    ("ssl_cert_days_to_expiry", Some(CheckFamily::Url)),
    ("dns_up", Some(CheckFamily::Dns)),
    ("dns_resolve_ms", Some(CheckFamily::Dns)),
    ("dns_answer_count", Some(CheckFamily::Dns)),
    ("dns_chain_length", Some(CheckFamily::Dns)),
    ("meraki_device_up", Some(CheckFamily::Meraki)),
    // An MX's WAN uplinks, one row each (ADR-164 decision 23 and 24).
    ("meraki_uplink_sent_bps", Some(CheckFamily::Meraki)),
    ("meraki_uplink_recv_bps", Some(CheckFamily::Meraki)),
    ("meraki_uplink_status", Some(CheckFamily::Meraki)),
    ("meraki_uplink_failed", Some(CheckFamily::Meraki)),
    ("meraki_uplink_loss_pct", Some(CheckFamily::Meraki)),
    ("meraki_uplink_latency_ms", Some(CheckFamily::Meraki)),
    // An MX's Auto VPN, node level (ADR-164 decision 25).
    ("meraki_vpn_hubs_reachable", Some(CheckFamily::Meraki)),
    ("meraki_vpn_hubs_unreachable", Some(CheckFamily::Meraki)),
    ("meraki_vpn_hubs_unreachable_pct", Some(CheckFamily::Meraki)),
    ("meraki_vpn_spokes_unreachable", Some(CheckFamily::Meraki)),
    // A switch port's traffic, one row per port (ADR-167). Its status and speed use the SNMP names,
    // which the built-in catalog carries, so they are not listed here.
    ("meraki_port_in_bps", Some(CheckFamily::Meraki)),
    ("meraki_port_out_bps", Some(CheckFamily::Meraki)),
    // An imported AP's numbers, published by core from its controller's AP walk (ADR-064 B2) —
    // collected by no template of the AP's own, which is why they are listed here.
    ("wlan_ap_up", Some(CheckFamily::Wlan)),
    ("wlan_ap_client_count", Some(CheckFamily::Wlan)),
    ("wlan_ap_cpu_pct", Some(CheckFamily::Wlan)),
    ("wlan_ap_mem_pct", Some(CheckFamily::Wlan)),
    ("wlan_ap_temp_c", Some(CheckFamily::Wlan)),
    ("wlan_ap_cpu_temp_c", Some(CheckFamily::Wlan)),
    ("wlan_ap_power_state", Some(CheckFamily::Wlan)),
    // A Meraki access point's two readings no controller walk has (ADR-168), published by the
    // poller from the Dashboard. The MR's other radio readings share the walked radios' names,
    // which the built-in radio template declares, so they are not listed here.
    ("wlan_ap_ssid_count", Some(CheckFamily::Wlan)),
    ("wlan_radio_non_wifi_util_pct", Some(CheckFamily::Wlan)),
];

/// The names in [`CHECK_FAMILIES`], in the same order.
///
/// It has two readers: [`metric_source`], which reports where a number comes from, and the test
/// that makes [`METRIC_MEANINGS`] checkable — without the list, a sentence for a metric nothing
/// collects would look identical to a sentence for one that does. Derived from the family table
/// so the two cannot disagree about a name.
pub const CHECK_METRICS: [&str; 40] = check_names(&CHECK_FAMILIES);

const fn check_names(rows: &[(&'static str, Option<CheckFamily>); 40]) -> [&'static str; 40] {
    let mut out = [""; 40];
    let mut i = 0;
    while i < rows.len() {
        out[i] = rows[i].0;
        i += 1;
    }
    out
}

/// The probe `metric` comes from, or `None` for a collected metric, a derived one, or the
/// liveness sentinel.
#[must_use]
pub fn check_family(metric: &str) -> Option<CheckFamily> {
    CHECK_FAMILIES
        .iter()
        .find(|(name, _)| *name == metric)
        .and_then(|(_, family)| *family)
}

/// What a metric's number *is* — the fact that turns `0` into `0%` and `2` into `2 ms` (ADR-046
/// Inc.7).
///
/// **This is not the `unit` column ADR-046 decision 6 declined, and it does not reopen it.** That
/// decision refused a column on `CollectionItem` / `mib_catalog` for two reasons that are both
/// still true: swapping a built-in catalogue row needs a range-delete migration
/// (`builtin-catalog-reseed`), and a custom item would make the unit operator free-text. A `const`
/// table beside the sentences is neither — it is the road ADR-079 already built for exactly this
/// problem, when `mib_catalog.description` turned out to be unfillable for the same reason.
///
/// **Nor is it inferred from the name.** Inc.6 decision J refused a `_pct` / `_ms` suffix rule and
/// the counter-example it named is still here: `huawei_cpu_usage` and `huawei_mem_usage` are both
/// percentages with no suffix at all, so the rule would miss precisely the vendor the lab runs.
/// There is no rule. All 117 rows were written by hand.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MetricUnit {
    /// Appended to the value as-is, and the same in every language: `%`, `ms`, `°C`, `W`, `dBm`,
    /// `Mbps`, `bps`, `/s`.
    Symbol(&'static str),
    /// A thing being counted, named in English here and **rendered from the locale** by the WebUI
    /// (`format:unit.*`) — `users`, `tunnels`, `sessions`, `days`. Anything language-dependent goes
    /// here rather than in [`MetricUnit::Symbol`], which is why `days` and `minutes` are counted
    /// nouns and not symbols.
    Counted(&'static str),
    /// **The stored number is not the number to show.** The payload names the unit the value is
    /// stored in, because that is what a threshold bound has to be written in
    /// (`alerts/rules.rs::lowest_bound`); the WebUI's `scalarValueFormat` owns the displayed string
    /// end to end and ignores any suffix.
    ///
    /// 🚨 **The consequence is that the screen and the rule speak different units** — the card says
    /// `15.6 GB` where the rule takes `16000000`, and `1.00` where the rule takes `100`. That was
    /// already true of `snmp_sys_uptime_ticks` on its own; Inc.7 makes it true of 17. Deliberate,
    /// and deferred with its unblocking condition in `backlog.md` rather than left unsaid.
    Scaled(&'static str),
    /// Deliberately none, and a decision rather than an omission — which is why this is an enum
    /// arm and not `Option::None`.
    ///
    /// Two populations. **The value is a code, not a quantity** (19): `1` = up, `2` = normal,
    /// `0`/`1` for the `*_up` probes. A unit would be wrong; what these want is a *label*, which is
    /// a different mechanism and is listed as not-done on ADR-046 Inc.7. **The unit differs per row
    /// or per model** (4): `ent_sensor_value` carries temperature, voltage, current, RPM and dBm on
    /// one metric; `mikrotik_cpu_temp` is °C on some models and tenths on others; `mikrotik_voltage`
    /// is usually tenths of a volt; `hr_storage_size` is in allocation units whose size the device
    /// reports per row. Every one of those four says so in its own sentence above.
    None,
}

impl MetricUnit {
    /// The unit the **stored** number is in, or `None` when it has none.
    ///
    /// ⚠️ **Stored, not displayed**, and the two differ for [`MetricUnit::Scaled`]. This is the one
    /// an API client wants: `query_metrics` returns the stored value, and a threshold bound is
    /// written in the stored unit (`alerts/rules.rs::lowest_bound`). The WebUI's card is the only
    /// surface that shows the scaled form, and it does its own formatting.
    #[must_use]
    pub fn stored(self) -> Option<&'static str> {
        match self {
            MetricUnit::Symbol(u) | MetricUnit::Counted(u) | MetricUnit::Scaled(u) => Some(u),
            MetricUnit::None => Option::None,
        }
    }

    /// The tag the WebUI and API clients branch on: `symbol` / `counted` / `scaled`, or `None`.
    ///
    /// Separate from [`Self::stored`] because the payloads are not distinguishable by looking at
    /// them — `%` and `kilobytes` are both just strings, and appending one to a number is right
    /// while appending the other is wrong.
    #[must_use]
    pub fn kind(self) -> Option<&'static str> {
        match self {
            MetricUnit::Symbol(_) => Some("symbol"),
            MetricUnit::Counted(_) => Some("counted"),
            MetricUnit::Scaled(_) => Some("scaled"),
            MetricUnit::None => Option::None,
        }
    }
}

/// What an alert on a metric is called — the words an operator sees instead of `snmp_up` (ADR-196).
///
/// Two shapes, because the two kinds of metric need different sentences, not the same sentence with
/// a different noun:
///
/// - **`Flag`** — a 0/1 answer (`snmp_up`, `http_up`). The name states the fault itself, and the
///   condition and the observed value are left out wherever it is shown: "SNMP not responding" says
///   everything, while "below 0.5 (was 0)" beside it is the part an operator had to decode.
/// - **`Value`** — a number (`cisco_cpu_5min`). The name is a noun, and the condition and the value
///   follow it as before: "CPU usage (5 min) above 80 (was 92)".
///
/// A metric whose value is a *code* (`bgp_peer_state`, `if_oper_status`) is a `Value` for now: the
/// name is readable, the code is not decoded. Decoding is a different mechanism and not in ADR-196.
///
/// ⚠️ `snmp_up` is the one name that is knowingly broader than its fault: 0 also covers an agent
/// that answers but implements none of the scalar OIDs assigned to the node (ADR-075). The name
/// stays short and the meaning sentence says so.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AlertName {
    /// A 0/1 metric; the payload names the fault.
    Flag(&'static str),
    /// A numeric metric; the payload is a noun the condition follows.
    Value(&'static str),
}

impl AlertName {
    /// The English name.
    #[must_use]
    pub const fn text(self) -> &'static str {
        match self {
            AlertName::Flag(t) | AlertName::Value(t) => t,
        }
    }

    /// `true` for a 0/1 metric, whose condition and value are not shown beside the name.
    #[must_use]
    pub const fn is_flag(self) -> bool {
        matches!(self, AlertName::Flag(_))
    }
}

/// Names for the alerts that come from no threshold rule, so have no row in [`METRIC_MEANINGS`]
/// (whose test forbids a sentence for a metric no rule can name). Their `metric` strings are owned
/// by the modules that raise them; the event-rule prefix is handled by [`alert_title`] itself.
pub const NON_RULE_ALERT_NAMES: [(&str, AlertName); 2] = [
    (
        crate::pool_coverage::COVERAGE_METRIC,
        AlertName::Value("Too few pollers in pool"),
    ),
    (
        crate::meraki::health::COLLECT_METRIC,
        AlertName::Flag("Meraki Dashboard collection failing"),
    ),
];

/// The name an alert on `metric` carries, or `None` for a metric Yagra has no name for (an event
/// rule's alert, or a metric a newer core introduced).
#[must_use]
pub fn alert_name(metric: &str) -> Option<AlertName> {
    METRIC_MEANINGS
        .binary_search_by_key(&metric, |(name, _, _, _)| name)
        .ok()
        .map(|i| METRIC_MEANINGS[i].3)
        .or_else(|| {
            NON_RULE_ALERT_NAMES
                .iter()
                .find(|(name, _)| *name == metric)
                .map(|(_, n)| *n)
        })
}

/// The English title of an alert on `metric`: its [`AlertName`], `Event rule: <name>` for an
/// event-rule alert, and the metric string itself when Yagra has no name for it — never empty for a
/// non-empty metric, so a caller can always print it.
#[must_use]
pub fn alert_title(metric: &str) -> String {
    if let Some(rule) = metric.strip_prefix(crate::events::EVENT_METRIC_PREFIX) {
        return format!("Event rule: {rule}");
    }
    alert_name(metric).map_or_else(|| metric.to_owned(), |n| n.text().to_owned())
}

/// An alert's English title, or `None` for an alert that recorded no metric (ADR-196). What REST,
/// MCP and the notification facts all put in `title`.
#[must_use]
pub fn alert_title_of(metric: &str) -> Option<String> {
    (!metric.is_empty()).then(|| alert_title(metric))
}

/// One sentence per metric, and what its number is, sorted by metric name.
///
/// Sorted, and pinned sorted by a test: the generated locale file is written in this order, so an
/// out-of-order row would surface as a spurious diff on every unrelated regeneration.
///
/// The unit rides in the same tuple on purpose: `every_metric_a_rule_can_name_has_a_sentence_and_no_others_do`
/// already pins this table to the collection catalogue in **both** directions, so a new metric now
/// fails to compile until someone decides its unit. That guarantee is bought, not built — there is
/// no separate check for units and there should not be one.
pub const METRIC_MEANINGS: [(&str, &str, MetricUnit, AlertName); 154] = [
    ("__liveness__", "Did the node answer its checks at all. Carries no bounds — a node either responded or it did not — so only the breach count applies. It is the only rule covering a monitor Yagra never pings (a URL, a DNS name, a Meraki device), and the only one whose alerts roll up under a failed parent instead of paging once per affected node.", MetricUnit::None, AlertName::Flag("Node not responding")),
    ("asa_current_connections", "Connections currently held by the ASA, one row per connection statistic the firewall reports (CISCO-FIREWALL-MIB).", MetricUnit::Counted("connections"), AlertName::Value("Firewall connections")),
    ("bgp_peer_admin_status", "Whether the BGP session is administratively started. 1 = stop, 2 = start. A peer down while this reads 2 is an unplanned outage.", MetricUnit::None, AlertName::Value("BGP session admin state")),
    ("bgp_peer_state", "BGP session state. 1 = idle, 2 = connect, 3 = active, 4 = opensent, 5 = openconfirm, 6 = established. Only 6 is a working session, so the usual rule is below 6.", MetricUnit::None, AlertName::Value("BGP session state")),
    ("cisco_cemp_mem_free", "Free bytes in an enhanced memory pool (cempMemPoolFree). Newer IOS-XE and IOS-XR answer here rather than on the older pool table.", MetricUnit::Scaled("bytes"), AlertName::Value("Memory pool free (enhanced)")),
    ("cisco_cemp_mem_used", "Used bytes in an enhanced memory pool (cempMemPoolUsed). Pair it with the free value to read a percentage.", MetricUnit::Scaled("bytes"), AlertName::Value("Memory pool used (enhanced)")),
    ("cisco_cemp_mem_used_pct", "Percentage of an enhanced memory pool in use. Computed from cempMemPoolUsed and cempMemPoolFree — Cisco reports the two halves and no total, so this figure exists in no series and cannot be charted. One value per pool, and each pool alerts on its own.", MetricUnit::Symbol("%"), AlertName::Value("Memory pool usage (enhanced)")),
    ("cisco_cpu_5min", "CPU utilisation averaged over the last five minutes, in percent (cpmCPUTotal5minRev). One row per CPU on a multi-CPU chassis.", MetricUnit::Symbol("%"), AlertName::Value("CPU usage (5 min)")),
    ("cisco_cpu_mem_free", "Memory free to the CPU, in kilobytes (cpmCPUMemoryFree).", MetricUnit::Scaled("kilobytes"), AlertName::Value("CPU memory free")),
    ("cisco_cpu_mem_used", "Memory in use by the CPU, in kilobytes (cpmCPUMemoryUsed).", MetricUnit::Scaled("kilobytes"), AlertName::Value("CPU memory used")),
    ("cisco_cpu_mem_used_pct", "Percentage of a per-CPU memory pool in use, computed from cpmCPUMemoryUsed and cpmCPUMemoryFree. Not collected — see cisco_cemp_mem_used_pct for why.", MetricUnit::Symbol("%"), AlertName::Value("CPU memory usage")),
    ("cisco_env_temp", "A chassis temperature sensor reading in degrees Celsius (ciscoEnvMonTemperatureStatusValue). One row per sensor.", MetricUnit::Symbol("°C"), AlertName::Value("Temperature")),
    ("cisco_fan_state", "Fan health. 1 = normal, 2 = warning, 3 = critical, 4 = shutdown, 5 = not present, 6 = not functioning.", MetricUnit::None, AlertName::Value("Fan state")),
    ("cisco_fantray_state", "Fan-tray health on chassis switches. 1 = unknown, 2 = up, 3 = down, 4 = warning.", MetricUnit::None, AlertName::Value("Fan tray state")),
    ("cisco_fru_power_state", "Power state of a field-replaceable unit. 2 = on; every other value is a form of off or failed (3 = off by admin, 4 = denied, 5 = off on power, 6 = off on temperature, 7 = off on fan failure, 8 = failed).", MetricUnit::None, AlertName::Value("Module power state")),
    ("cisco_ike_active_tunnels", "IKE (phase 1) tunnels currently established on this device.", MetricUnit::Counted("tunnels"), AlertName::Value("IKE tunnels up")),
    ("cisco_ipsec_active_tunnels", "IPsec (phase 2) tunnels currently established on this device.", MetricUnit::Counted("tunnels"), AlertName::Value("IPsec tunnels up")),
    ("cisco_mem_free", "Free bytes in a memory pool (ciscoMemoryPoolFree). The classic IOS pool table; newer platforms answer on the cemp metrics instead.", MetricUnit::Scaled("bytes"), AlertName::Value("Memory pool free")),
    ("cisco_mem_used", "Used bytes in a memory pool (ciscoMemoryPoolUsed).", MetricUnit::Scaled("bytes"), AlertName::Value("Memory pool used")),
    ("cisco_mem_used_pct", "Percentage of a memory pool in use, computed from ciscoMemoryPoolUsed and ciscoMemoryPoolFree. The older pool table; newer IOS-XE and IOS-XR answer on cisco_cemp_mem_used_pct instead. Not collected.", MetricUnit::Symbol("%"), AlertName::Value("Memory pool usage")),
    ("cisco_psu_state", "Power-supply health. 1 = normal, 2 = warning, 3 = critical, 4 = shutdown, 5 = not present, 6 = not functioning.", MetricUnit::None, AlertName::Value("Power supply state")),
    ("cisco_ra_sessions", "Remote-access VPN sessions currently connected (AnyConnect / SSL VPN).", MetricUnit::Counted("sessions"), AlertName::Value("Remote-access VPN sessions")),
    ("cisco_ra_users", "Distinct users currently connected over remote-access VPN. Lower than the session count when one user has several sessions.", MetricUnit::Counted("users"), AlertName::Value("Remote-access VPN users")),
    ("cisco_temp_c", "Chassis temperature in degrees Celsius, from the Cisco sensor table. Only the sensors that reach no port — per-port optical readings are reported as light level instead (ADR-070).", MetricUnit::Symbol("°C"), AlertName::Value("Chassis temperature")),
    ("dns_answer_count", "How many answers the final response carried.", MetricUnit::Counted("answers"), AlertName::Value("DNS answers")),
    ("dns_chain_length", "How many hops the resolution chain took (CNAME → … → A/AAAA).", MetricUnit::Counted("hops"), AlertName::Value("DNS resolution hops")),
    ("dns_resolve_ms", "How long the resolution took, in milliseconds.", MetricUnit::Symbol("ms"), AlertName::Value("DNS resolution time")),
    ("dns_up", "Did the name resolve. 1 = yes, 0 = any failure — NXDOMAIN, SERVFAIL, REFUSED, timeout or a CNAME loop.", MetricUnit::None, AlertName::Flag("Name not resolving")),
    ("ent_sensor_value", "A physical sensor reading from ENTITY-SENSOR-MIB. ⚠️ The unit differs per row — temperature, voltage, current, RPM and dBm all arrive on this one metric — so a single threshold across all rows is rarely meaningful.", MetricUnit::None, AlertName::Value("Sensor reading")),
    ("fortinet_cpu_usage", "CPU utilisation of the FortiGate, in percent.", MetricUnit::Symbol("%"), AlertName::Value("CPU usage")),
    ("fortinet_mem_usage", "Memory utilisation of the FortiGate, in percent. Above roughly 80 the unit enters conserve mode and starts dropping sessions.", MetricUnit::Symbol("%"), AlertName::Value("Memory usage")),
    ("fortinet_sessions", "Sessions currently held in the FortiGate session table.", MetricUnit::Counted("sessions"), AlertName::Value("Firewall sessions")),
    ("fortinet_sslvpn_users", "Users currently logged in over SSL VPN. One row per virtual domain.", MetricUnit::Counted("users"), AlertName::Value("SSL VPN users")),
    ("fortinet_vpn_tunnels_up", "IPsec tunnels currently up. Alert below the number you expect to be permanently established.", MetricUnit::Counted("tunnels"), AlertName::Value("IPsec tunnels up")),
    ("hr_processor_load", "Per-CPU load over the last minute, in percent (hrProcessorLoad). One row per processor; the node-level view takes the highest.", MetricUnit::Symbol("%"), AlertName::Value("CPU load")),
    ("hr_storage_size", "Total size of a storage area, in allocation units (hrStorageSize). ⚠️ Not bytes — multiply by the unit size the device reports for that row.", MetricUnit::None, AlertName::Value("Storage size")),
    ("hr_storage_used", "Used portion of a storage area, in allocation units (hrStorageUsed). Same unit caveat as the size.", MetricUnit::None, AlertName::Value("Storage used")),
    ("hr_storage_used_pct", "Percentage of a storage area in use, computed from hrStorageUsed and hrStorageSize. ⚠️ hrStorageTable holds filesystems, physical memory and swap together, so a node has several of these, and each alerts on its own. Both halves are read from the same row — dividing one row by another would report the wrong disk. Not collected, so it has no chart.", MetricUnit::Symbol("%"), AlertName::Value("Storage usage")),
    ("hr_system_processes", "Processes currently running on the host (hrSystemProcesses).", MetricUnit::Counted("processes"), AlertName::Value("Running processes")),
    ("http_body_match", "Did the response body match the rule configured on the monitor. 1 = matched, 0 = did not.", MetricUnit::None, AlertName::Flag("Response body did not match")),
    ("http_response_time_ms", "How long the monitor’s request took, in milliseconds.", MetricUnit::Symbol("ms"), AlertName::Value("URL response time")),
    ("http_status_code", "The HTTP status code the monitor received.", MetricUnit::None, AlertName::Value("HTTP status code")),
    ("http_up", "Did the URL answer, with the status the monitor expects. 1 = yes, 0 = unreachable or the wrong status.", MetricUnit::None, AlertName::Flag("URL not responding as expected")),
    ("huawei_cpu_usage", "CPU utilisation in percent (hwEntityCpuUsage). One row per board or entity that has a CPU.", MetricUnit::Symbol("%"), AlertName::Value("CPU usage")),
    ("huawei_mem_free", "Free memory in bytes, from HUAWEI-MEMORY-MIB. Pair it with the total to read a percentage.", MetricUnit::Scaled("bytes"), AlertName::Value("Free memory")),
    ("huawei_mem_total", "Installed memory in bytes, from HUAWEI-MEMORY-MIB.", MetricUnit::Scaled("bytes"), AlertName::Value("Installed memory")),
    ("huawei_mem_usage", "Memory utilisation in percent (hwEntityMemUsage) — the device’s own figure, so it needs no arithmetic.", MetricUnit::Symbol("%"), AlertName::Value("Memory usage")),
    ("huawei_mem_used_pct", "Percentage of installed memory in use, computed from the total and free readings in HUAWEI-MEMORY-MIB. Not collected. On a device that also reports huawei_mem_usage the two should agree; that one is the vendor’s own per-board figure, this one is whole-box.", MetricUnit::Symbol("%"), AlertName::Value("Memory usage (computed)")),
    ("huawei_temp", "Entity temperature in degrees Celsius (hwEntityTemperature). One row per board or sensor.", MetricUnit::Symbol("°C"), AlertName::Value("Temperature")),
    ("huawei_usg_half_open_sessions", "Half-open sessions on the firewall — connections that started a handshake and never finished it. A rising count is the usual signature of a SYN flood or a dead peer.", MetricUnit::Counted("sessions"), AlertName::Value("Half-open sessions")),
    ("huawei_usg_icmp_sessions", "ICMP sessions currently held by the firewall.", MetricUnit::Counted("sessions"), AlertName::Value("ICMP sessions")),
    ("huawei_usg_session_setup_rate", "New sessions the firewall is establishing per second.", MetricUnit::Symbol("/s"), AlertName::Value("New sessions per second")),
    ("huawei_usg_tcp_sessions", "TCP sessions currently held by the firewall.", MetricUnit::Counted("sessions"), AlertName::Value("TCP sessions")),
    ("huawei_usg_total_sessions", "All sessions the firewall currently holds, across every protocol.", MetricUnit::Counted("sessions"), AlertName::Value("Firewall sessions")),
    ("huawei_usg_udp_sessions", "UDP sessions currently held by the firewall.", MetricUnit::Counted("sessions"), AlertName::Value("UDP sessions")),
    ("icmp_loss_pct", "Share of ICMP probes that got no reply, 0–100. A partial loss means the link is degraded but the node is still answering. 100% means it has stopped answering altogether, and that is what the Reachability rule raises a critical for — so this rule is about degradation, not about an outage.", MetricUnit::Symbol("%"), AlertName::Value("Ping packet loss")),
    ("icmp_rtt_ms", "Round-trip time of the ICMP probe, in milliseconds.", MetricUnit::Symbol("ms"), AlertName::Value("Ping response time")),
    ("if_admin_status", "Whether the port is administratively enabled. 1 = up, 2 = down, 3 = testing. A port down while this reads 1 is unplanned; a port down while this reads 2 was shut deliberately.", MetricUnit::None, AlertName::Value("Port admin state")),
    ("if_high_speed", "The port’s nominal bandwidth in Mbps (ifHighSpeed). 0 on a port that reports no speed, which is why it is worth alerting on: a link that renegotiated to 100 on a gigabit port shows up here.", MetricUnit::Symbol("Mbps"), AlertName::Value("Port speed")),
    ("if_in_bps", "Inbound traffic in bits per second. Computed from the octet counter at evaluation time, not collected, so it has no chart of its own; the Interfaces tab draws the same figure. Unlike the percentage it needs no denominator, so it is the only way to alert on a port whose speed the device does not report.", MetricUnit::Symbol("bps"), AlertName::Value("Inbound traffic")),
    ("if_in_util_pct", "Inbound traffic as a percentage of the port’s own speed. Computed from the octet counter and the speed the device reports — it is not collected, so it has no chart of its own; the Interfaces tab draws the same figure. A port whose speed is unknown cannot be evaluated and never fires.", MetricUnit::Symbol("%"), AlertName::Value("Inbound utilization")),
    ("if_oper_status", "Whether the port is actually passing traffic. 1 = up, 2 = down, 3 = testing, 4 = unknown, 5 = dormant, 6 = not present, 7 = lower layer down.", MetricUnit::None, AlertName::Value("Port link state")),
    ("if_out_bps", "Outbound traffic in bits per second. Computed from the octet counter at evaluation time, not collected, so it has no chart of its own; the Interfaces tab draws the same figure. Unlike the percentage it needs no denominator, so it is the only way to alert on a port whose speed the device does not report.", MetricUnit::Symbol("bps"), AlertName::Value("Outbound traffic")),
    ("if_out_util_pct", "Outbound traffic as a percentage of the port’s own speed. Separate from the inbound figure on purpose: a link is asymmetric more often than not, and a rule on one direction says nothing about the other.", MetricUnit::Symbol("%"), AlertName::Value("Outbound utilization")),
    ("if_rx_power_dbm", "Optical receive power of the transceiver, in dBm. Typically −20 to 0; falling toward the receiver’s sensitivity floor is the early sign of a dirty or failing fibre.", MetricUnit::Symbol("dBm"), AlertName::Value("Optical receive power")),
    ("if_tx_power_dbm", "Optical transmit power of the transceiver, in dBm. A falling value points at the transceiver itself rather than the fibre.", MetricUnit::Symbol("dBm"), AlertName::Value("Optical transmit power")),
    ("juniper_buffer_util", "Buffer-pool utilisation in percent (jnxOperatingBuffer). One row per operating subject — routing engine, FPC, PIC.", MetricUnit::Symbol("%"), AlertName::Value("Buffer usage")),
    ("juniper_cpu_1min", "CPU utilisation in percent (jnxOperatingCPU). ⚠️ Despite the metric name this is the instantaneous value, not a one-minute average, so it is spikier than a load figure.", MetricUnit::Symbol("%"), AlertName::Value("CPU usage")),
    ("juniper_temp", "Temperature in degrees Celsius (jnxOperatingTemp). One row per operating subject.", MetricUnit::Symbol("°C"), AlertName::Value("Temperature")),
    ("meraki_device_up", "Does the Meraki dashboard report the device online. 1 = online, 0 = offline.", MetricUnit::None, AlertName::Flag("Device offline in Meraki Dashboard")),
    ("meraki_port_in_bps", "Traffic one port of a Meraki switch received, in bits per second — the average over a five-minute bucket the Dashboard had finished filling, so the line runs twelve to seventeen minutes behind the port. One row per port; the Interfaces tab and its charts read it in place of an octet counter.", MetricUnit::Symbol("bps"), AlertName::Value("Switch port inbound traffic")),
    ("meraki_port_out_bps", "Traffic one port of a Meraki switch sent, in bits per second — the average over a five-minute bucket the Dashboard had finished filling, so the line runs twelve to seventeen minutes behind the port. One row per port; the Interfaces tab and its charts read it in place of an octet counter.", MetricUnit::Symbol("bps"), AlertName::Value("Switch port outbound traffic")),
    ("meraki_uplink_failed", "Does the Meraki dashboard report this WAN uplink of an MX failed. 1 = failed, 0 = any other status — including not connected, which on a real organization was almost always a port with no line behind it. One row per uplink.", MetricUnit::None, AlertName::Flag("WAN uplink failed")),
    ("meraki_uplink_latency_ms", "Latency of one WAN uplink of a Meraki MX to the Dashboard's test address, in milliseconds — the latest minute the Dashboard reported. One row per uplink.", MetricUnit::Symbol("ms"), AlertName::Value("WAN uplink latency")),
    ("meraki_uplink_loss_pct", "Packet loss of one WAN uplink of a Meraki MX to the Dashboard's test address, in percent — the latest minute the Dashboard reported. One row per uplink.", MetricUnit::Symbol("%"), AlertName::Value("WAN uplink packet loss")),
    ("meraki_uplink_recv_bps", "Receive rate of one WAN uplink of a Meraki MX (WAN1, WAN2 or cellular), in bits per second — the average over the traffic collect's interval, not a peak. One row per uplink. The Dashboard reports it for MX appliances only.", MetricUnit::Symbol("bps"), AlertName::Value("WAN uplink receive rate")),
    ("meraki_uplink_sent_bps", "Send rate of one WAN uplink of a Meraki MX (WAN1, WAN2 or cellular), in bits per second — the average over the traffic collect's interval, not a peak. One row per uplink. The Dashboard reports it for MX appliances only.", MetricUnit::Symbol("bps"), AlertName::Value("WAN uplink send rate")),
    ("meraki_uplink_status", "Status of one WAN uplink of a Meraki MX: 2 = active, 1 = ready (standing by), 0 = not connected or connecting, −1 = failed. Before this build a failed uplink was also stored as 0. One row per uplink.", MetricUnit::None, AlertName::Value("WAN uplink status")),
    ("meraki_vpn_hubs_reachable", "How many Auto VPN hubs this Meraki MX reaches. A spoke counts its hubs; a hub counts the other hubs. Reported only while the MX itself is up, and a hub that is itself down is not counted either way.", MetricUnit::None, AlertName::Value("Auto VPN hubs reachable")),
    ("meraki_vpn_hubs_unreachable", "How many Auto VPN hubs this Meraki MX does not reach, counted as for meraki_vpn_hubs_reachable — a hub that is itself down is left out, because it raises its own alert.", MetricUnit::None, AlertName::Value("Auto VPN hubs unreachable")),
    ("meraki_vpn_hubs_unreachable_pct", "The share of this Meraki MX's counted Auto VPN hubs it does not reach, 0–100. Any value above 0 means the site lost a hub (its redundancy); 100 means it reaches none. Absent when no hub was counted.", MetricUnit::Symbol("%"), AlertName::Value("Auto VPN hubs unreachable (%)")),
    ("meraki_vpn_spokes_unreachable", "On an Auto VPN hub, how many of its spokes it does not reach. A spoke that is itself down is left out — it raises its own alert.", MetricUnit::None, AlertName::Value("Auto VPN spokes unreachable")),
    ("mikrotik_cpu_temp", "Temperature reported by RouterOS (mtxrHlTemperature). ⚠️ Some models report degrees Celsius and others tenths of a degree — read the live value once before choosing a bound.", MetricUnit::None, AlertName::Value("Temperature")),
    ("mikrotik_voltage", "Input voltage reported by RouterOS (mtxrHlVoltage). ⚠️ Usually tenths of a volt, so 240 means 24.0 V — confirm against the live value.", MetricUnit::None, AlertName::Value("Input voltage")),
    ("nxos_cpu_util", "CPU utilisation of the supervisor, in percent (NX-OS).", MetricUnit::Symbol("%"), AlertName::Value("Supervisor CPU usage")),
    ("nxos_mem_util", "Memory utilisation of the supervisor, in percent (NX-OS).", MetricUnit::Symbol("%"), AlertName::Value("Supervisor memory usage")),
    ("panos_gp_active_tunnels", "GlobalProtect tunnels currently connected to this gateway.", MetricUnit::Counted("tunnels"), AlertName::Value("GlobalProtect tunnels")),
    ("panos_session_util_pct", "Session table utilisation in percent — the firewall’s own figure, so it already accounts for the platform’s limit.", MetricUnit::Symbol("%"), AlertName::Value("Session table usage")),
    ("panos_sessions_active", "Sessions currently active on the firewall.", MetricUnit::Counted("sessions"), AlertName::Value("Active sessions")),
    ("poe_power_capacity_w", "Power the PoE supply can deliver, in watts (pethMainPsePower). One row per PoE group.", MetricUnit::Symbol("W"), AlertName::Value("PoE power capacity")),
    ("poe_power_consumed_w", "Power the connected devices are drawing, in watts (pethMainPseConsumptionPower). Compare it with the capacity to see how much budget is left.", MetricUnit::Symbol("W"), AlertName::Value("PoE power drawn")),
    ("poe_power_used_pct", "Percentage of a PoE group’s power budget drawn by attached devices, computed from pethMainPseConsumptionPower over pethMainPsePower. Not collected. No default rule ships for it: what counts as over-subscribed is a site decision, not a device fault.", MetricUnit::Symbol("%"), AlertName::Value("PoE budget used")),
    ("prt_alert_severity", "Severity of a printer alert. 1 = other, 3 = critical, 4 = warning, 5 = warning (binary change). One row per outstanding alert.", MetricUnit::None, AlertName::Value("Printer alert severity")),
    ("prt_marker_life_count", "Pages the print engine has produced over its life (prtMarkerLifeCount). Useful for consumable planning, not for spotting a fault.", MetricUnit::Counted("pages"), AlertName::Value("Pages printed (lifetime)")),
    ("snmp_arp_entry_count", "How many ARP entries the last walk found.", MetricUnit::Counted("entries"), AlertName::Value("ARP entries")),
    ("snmp_l3_address_count", "How many L3 addresses the last walk found.", MetricUnit::Counted("addresses"), AlertName::Value("L3 addresses")),
    ("snmp_neighbor_count", "How many CDP/LLDP neighbours the last walk found.", MetricUnit::Counted("neighbours"), AlertName::Value("CDP/LLDP neighbours")),
    ("snmp_routing_adjacency_count", "How many OSPF/BGP adjacencies the last walk found.", MetricUnit::Counted("adjacencies"), AlertName::Value("OSPF/BGP adjacencies")),
    ("snmp_sys_uptime_ticks", "Time since the SNMP agent last restarted, in hundredths of a second (sysUpTime). ⚠️ Divide by 100 for seconds — one day is 8,640,000. It wraps after about 497 days.", MetricUnit::Scaled("hundredths of a second"), AlertName::Value("SNMP agent uptime")),
    ("snmp_up", "Did the SNMP agent answer this poll. 1 = at least one value came back, 0 = nothing came back or the request failed. ⚠️ 0 also covers an agent that answers but implements none of the scalar OIDs assigned to this node, so an interface walk that still returns rows means the agent is alive.", MetricUnit::None, AlertName::Flag("SNMP not responding")),
    ("snmp_walk_complete", "Did this node's interface walk get to ask for every metric it is configured to collect. 1 = yes, 0 = the walk ran out of time first and the remaining columns were never requested. ⚠️ 0 does not mean the device is down — it usually means the device answers SNMP too slowly for the number of ports it has, and the metrics that were never asked for will simply be missing.", MetricUnit::None, AlertName::Flag("Interface walk cut short")),
    ("ssl_cert_days_to_expiry", "Days until the TLS certificate expires; negative once it already has.", MetricUnit::Counted("days"), AlertName::Value("Days until certificate expiry")),
    ("tcp_curr_estab", "TCP connections currently in the established state (tcpCurrEstab).", MetricUnit::Counted("connections"), AlertName::Value("Established TCP connections")),
    ("ucd_cpu_idle_pct", "CPU idle time in percent (ssCpuIdle). This is the inverse of load, so alert *below* a bound, not above.", MetricUnit::Symbol("%"), AlertName::Value("CPU idle")),
    ("ucd_cpu_used_pct", "CPU in use, in percent — 100 minus ssCpuIdle. Not collected; it exists so a rule can read the usual way round (alert *above* a bound) instead of below one.", MetricUnit::Symbol("%"), AlertName::Value("CPU usage")),
    ("ucd_disk_used_pct", "Disk usage in percent (dskPercent). One row per filesystem configured for monitoring in snmpd.conf.", MetricUnit::Symbol("%"), AlertName::Value("Disk usage")),
    ("ucd_load_15min", "Fifteen-minute load average × 100 (laLoadInt). ⚠️ 100 means a load of 1.00 — set the bound in hundredths.", MetricUnit::Scaled("hundredths of a load average"), AlertName::Value("Load average (15 min)")),
    ("ucd_load_1min", "One-minute load average × 100 (laLoadInt). ⚠️ 100 means a load of 1.00 — set the bound in hundredths.", MetricUnit::Scaled("hundredths of a load average"), AlertName::Value("Load average (1 min)")),
    ("ucd_load_5min", "Five-minute load average × 100 (laLoadInt). ⚠️ 100 means a load of 1.00 — set the bound in hundredths.", MetricUnit::Scaled("hundredths of a load average"), AlertName::Value("Load average (5 min)")),
    ("ucd_load_per_core", "One-minute load average divided by the number of processors the host reports. ⚠️ In hundredths, like laLoadInt itself: **100 means one runnable task per processor** and 200 means twice that. Not collected — the divisor is how many hr_processor_load rows the node has, and a host that reports none is skipped rather than divided by one.", MetricUnit::Scaled("hundredths of a load average"), AlertName::Value("Load per core")),
    ("ucd_mem_avail_kb", "Free physical memory in kilobytes (memAvailReal). On Linux this excludes the page cache, so it reads lower than \"available\".", MetricUnit::Scaled("kilobytes"), AlertName::Value("Available memory")),
    ("ucd_mem_total_kb", "Installed physical memory in kilobytes (memTotalReal).", MetricUnit::Scaled("kilobytes"), AlertName::Value("Installed memory")),
    ("ucd_mem_used_pct", "Percentage of physical memory in use, computed from memTotalReal and memAvailReal. ⚠️ On Linux memAvailReal excludes the page cache, so this reads higher than free(1)’s \"available\" column. Not collected.", MetricUnit::Symbol("%"), AlertName::Value("Memory usage")),
    ("ucd_swap_avail_kb", "Free swap in kilobytes (memAvailSwap). A host that has started consuming swap is usually already in trouble.", MetricUnit::Scaled("kilobytes"), AlertName::Value("Available swap")),
    ("ucd_swap_total_kb", "Configured swap in kilobytes (memTotalSwap).", MetricUnit::Scaled("kilobytes"), AlertName::Value("Configured swap")),
    ("ucd_swap_used_pct", "Percentage of swap in use, computed from memTotalSwap and memAvailSwap. A host that has started consuming swap is usually already in trouble, so a low bound is reasonable here. Not collected.", MetricUnit::Symbol("%"), AlertName::Value("Swap usage")),
    ("ups_battery_status", "Battery condition. 1 = unknown, 2 = normal, 3 = low, 4 = depleted. Anything above 2 needs attention.", MetricUnit::None, AlertName::Value("UPS battery condition")),
    ("ups_charge_remaining_pct", "Estimated battery charge remaining, in percent.", MetricUnit::Symbol("%"), AlertName::Value("UPS battery charge")),
    ("ups_minutes_remaining", "Estimated run time left on battery, in minutes. Meaningful only while the UPS is actually on battery.", MetricUnit::Counted("minutes"), AlertName::Value("UPS runtime remaining")),
    ("ups_output_load_pct","Output load as a percentage of the UPS’s rated capacity. One row per output line.", MetricUnit::Symbol("%"), AlertName::Value("UPS output load")),
    ("wlan_ap_client_count", "Wireless clients online through this access point, as the controller serving it reports — or, for a Meraki access point, as the Meraki Dashboard reports. A controller's access point has it only while the controller reports the AP in service; a Meraki one that is down reads 0.", MetricUnit::Counted("clients"), AlertName::Value("AP clients")),
    ("wlan_ap_cpu_pct", "CPU in use on this access point, in percent, as the controller serving it reports. An HA standby's view (always 0) is never recorded.", MetricUnit::Symbol("%"), AlertName::Value("AP CPU usage")),
    ("wlan_ap_cpu_temp_c", "Temperature of this access point's CPU in degrees Celsius, as the controller serving it reports. A different sensor from the operating temperature, and the one most models actually have. Absent rather than 0 when the controller reports no reading.", MetricUnit::Symbol("°C"), AlertName::Value("AP CPU temperature")),
    ("wlan_ap_mem_pct", "Memory in use on this access point, in percent, as the controller serving it reports. An HA standby's view (always 0) is never recorded.", MetricUnit::Symbol("%"), AlertName::Value("AP memory usage")),
    ("wlan_ap_power_state", "How this access point is being powered, as the controller reports it: 1 normal, 2 insufficient, 3 limited. Published only while the controller is serving the access point, so a down access point has no value here — that is what the up/down metric says. Values 2 and 3 have not been observed on the hardware this was measured against.", MetricUnit::None, AlertName::Value("AP power state")),
    ("wlan_ap_ssid_count", "How many SSIDs this access point is broadcasting: enabled, and on air on at least one of its radios. A Meraki access point only, read from the Dashboard every twenty minutes — and only while its radios are being measured: the Dashboard keeps answering a stopped access point's last configuration as broadcasting, so a stopped one has no value rather than a stale one.", MetricUnit::None, AlertName::Value("SSIDs on air")),
    ("wlan_ap_temp_c", "Operating temperature of this access point in degrees Celsius, as the controller serving it reports. Absent for a model with no sensor rather than recorded as 0 — most access points have none, so the reading to watch is usually the CPU temperature instead.", MetricUnit::Symbol("°C"), AlertName::Value("AP temperature")),
    ("wlan_ap_up", "Does the controller serving this access point report it in service. 1 = in service; 0 = the controller reports it down, not joined or failing. When no controller reports the AP at all, nothing is recorded — the value stops arriving rather than dropping to 0. The exception is a Cisco controller, whose table drops an AP it has lost rather than listing it as down: an AP it served that is missing from a complete read of that table is recorded as 0, except in the first 15 minutes after the controller starts.", MetricUnit::None, AlertName::Flag("AP out of service")),
    ("wlan_ap_walk_complete", "Did the wireless controller's access-point table read to its end on this poll. 1 = yes; 0 = a column did not answer, so no AP list was published and the stored list was left as it was. Stuck at 0 means the AP list has stopped refreshing.", MetricUnit::None, AlertName::Flag("AP table read cut short")),
    ("wlan_controller_ap_capacity", "The most access points the controller's platform supports — the model's ceiling, not the number of licences bought. Compare it with the joined count to see how close the controller is to full. Only a Cisco controller publishes it: AireOS and the Catalyst 9800 keep it in different objects, and whichever the controller answers is used. A Huawei controller's licence count is a separate reading.", MetricUnit::Counted("access points"), AlertName::Value("AP capacity")),
    ("wlan_controller_ap_license", "Access points the wireless controller is licensed to manage. Compare it with the configured count to see how much licence headroom is left. An HA standby reports the same licence.", MetricUnit::Counted("access points"), AlertName::Value("AP licences")),
    ("wlan_controller_ap_normal_pct", "Share of the controller's configured access points that are working normally, in percent. Below 100 means at least one AP is down, not yet joined, or failing its configuration. An HA standby reports the active controller's figure, so a pair raises one condition twice.", MetricUnit::Symbol("%"), AlertName::Value("APs working normally")),
    ("wlan_controller_aps_configured", "Access points configured on the wireless controller, whether or not they are currently joined.", MetricUnit::Counted("access points"), AlertName::Value("APs configured")),
    ("wlan_controller_aps_joined", "Access points currently joined to the wireless controller. An HA standby reports the active controller's count, so do not add the two members of a pair together.", MetricUnit::Counted("access points"), AlertName::Value("APs joined")),
    ("wlan_controller_aps_missing", "How many of the access points this controller serves are not joined to it right now: the ones it last served that its table no longer lists, and the ones it lists as not associated. Only a Cisco controller publishes it, and only for a complete read of its table taken 15 minutes or more after it started. An access point that moved to a controller Yagra does not monitor counts as missing, and one retired for good stops counting when its node is deleted.", MetricUnit::Counted("access points"), AlertName::Value("APs not joined")),
    ("wlan_controller_clients", "Wireless clients currently online through the controller, on every band. A Huawei controller reports it directly; a Cisco controller's is the sum of its SSID table's client counts, including a WLAN whose name could not be read. An HA standby reports the active controller's count, so do not add the two members of a pair together.", MetricUnit::Counted("clients"), AlertName::Value("Wireless clients")),
    ("wlan_controller_clients_2g4", "Wireless clients currently online on the 2.4 GHz band. A Huawei controller reports it directly, and an HA standby reports the active controller's count. A Cisco controller's is the sum over every radio working in the band, so a radio whose band cannot be told is left out and the three bands need not add up to the overall count.", MetricUnit::Counted("clients"), AlertName::Value("Wireless clients (2.4 GHz)")),
    ("wlan_controller_clients_5g", "Wireless clients currently online on the 5 GHz band. A Huawei controller reports it directly, and an HA standby reports the active controller's count. A Cisco controller's is the sum over every radio working in the band, so a radio whose band cannot be told is left out and the three bands need not add up to the overall count.", MetricUnit::Counted("clients"), AlertName::Value("Wireless clients (5 GHz)")),
    ("wlan_controller_clients_6g", "Wireless clients currently online on the 6 GHz band. Zero on a controller whose access points have no 6 GHz radio. A Huawei controller reports it directly, and an HA standby reports the active controller's count. A Cisco controller's is the sum over every radio working in the band, so a radio whose band cannot be told is left out and the three bands need not add up to the overall count.", MetricUnit::Counted("clients"), AlertName::Value("Wireless clients (6 GHz)")),
    ("wlan_controller_ssid_count", "How many SSIDs this wireless controller is broadcasting. Published only when the SSID table was read to its end and every WLAN in it had a name, so it never falls just because a read failed. An HA standby is configured with the same SSIDs as the controller it backs up, so both members of a pair report the same number.", MetricUnit::None, AlertName::Value("SSIDs broadcasting")),
    ("wlan_radio_channel", "The channel this radio is working on. An identifier rather than a measurement — read it as a history, where a step means the controller (or, for a Meraki access point, the Dashboard) moved the radio, and never as something to put a threshold on. One series per radio of the access point, each radio being a slot: 2.4 GHz is 1, 5 GHz is 2, 6 GHz is 3. A Meraki access point's is read every twenty minutes.", MetricUnit::None, AlertName::Value("Radio channel")),
    ("wlan_radio_channel_util_pct", "How much of this radio’s channel is in use, in percent, as the controller measured it — or, for a Meraki access point, the Dashboard’s average over the last five minutes. The number to watch for a busy area: it counts everything on the channel, including neighbouring networks and energy that is not Wi-Fi, not only this access point’s own traffic.", MetricUnit::Symbol("%"), AlertName::Value("Channel utilization")),
    ("wlan_radio_client_count", "Wireless clients currently online through this radio of this access point. The access point’s own client count is the sum across its radios.", MetricUnit::None, AlertName::Value("Radio clients")),
    ("wlan_radio_client_signal_dbm", "The average signal strength of the clients on this radio, in dBm — a negative number, closer to zero being stronger. Absent rather than 0 when the radio has no clients to average.", MetricUnit::Symbol("dBm"), AlertName::Value("Client signal strength")),
    ("wlan_radio_interference_pct", "How much of this radio’s channel is lost to interference, in percent, as the controller measured it. High interference with low utilization points at something that is not Wi-Fi.", MetricUnit::Symbol("%"), AlertName::Value("Channel interference")),
    ("wlan_radio_noise_dbm", "The noise floor this radio measures, in dBm — a negative number, and the more negative the quieter. Absent rather than 0 when the controller reports no figure, because a noise floor of 0 dBm would read as a radio being drowned.", MetricUnit::Symbol("dBm"), AlertName::Value("Noise floor")),
    ("wlan_radio_non_wifi_util_pct", "The part of this radio’s channel utilization taken by energy that is not Wi-Fi — a microwave oven, Bluetooth, radar — in percent, averaged over the last five minutes. Included in the channel utilization, never added to it. A Meraki access point only: a Huawei controller’s interference ratio counts other Wi-Fi as well, so it keeps its own name.", MetricUnit::Symbol("%"), AlertName::Value("Non-Wi-Fi interference")),
    ("wlan_radio_tx_power_dbm", "The power this radio is actually transmitting at, in dBm. A controller — or the Meraki Dashboard — running automatic power control lowers it where access points overlap, so a value well below the others is usually a decision rather than a fault. Absent when no figure is reported. A Meraki access point's is read every twenty minutes.", MetricUnit::Symbol("dBm"), AlertName::Value("Radio transmit power")),
    ("wlan_ssid_ap_count", "How many access points are broadcasting this SSID, as the controller reports. One row per SSID, named by the SSID itself.", MetricUnit::None, AlertName::Value("APs broadcasting this SSID")),
    ("wlan_ssid_clients", "Wireless clients currently online on this SSID: the controller’s own count where it keeps one with no band split (Cisco), otherwise added up over the bands the controller answered for. One row per SSID, named by the SSID itself. An HA standby reports the active controller’s numbers, so do not add the two controllers together.", MetricUnit::None, AlertName::Value("SSID clients")),
    ("wlan_ssid_clients_2g4", "Wireless clients currently online on this SSID over 2.4 GHz. One row per SSID, named by the SSID itself.", MetricUnit::None, AlertName::Value("SSID clients (2.4 GHz)")),
    ("wlan_ssid_clients_5g", "Wireless clients currently online on this SSID over 5 GHz. One row per SSID, named by the SSID itself.", MetricUnit::None, AlertName::Value("SSID clients (5 GHz)")),
    ("wlan_ssid_clients_6g", "Wireless clients currently online on this SSID over 6 GHz. Zero on a controller whose access points have no 6 GHz radios. One row per SSID, named by the SSID itself.", MetricUnit::None, AlertName::Value("SSID clients (6 GHz)")),
    ("wlan_ssid_walk_complete", "Did the controller answer every column of its SSID table on this poll (1) or not (0). Unlike the access-point walk, an incomplete read still publishes the SSIDs it did get — each one is an independent reading rather than a member of a list that replaces the stored one — but it will not say how many SSIDs there are.", MetricUnit::None, AlertName::Flag("SSID table read incomplete")),
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
    } else if crate::interface_util::DERIVED_INTERFACE_METRICS.contains(&metric)
        || crate::derived::derived_node_metric(metric).is_some()
    {
        // Computed at evaluation time — per port (ADR-076) or per node (ADR-105); queryable
        // through no series either way.
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
        let names: Vec<&str> = METRIC_MEANINGS.iter().map(|(n, _, _, _)| *n).collect();
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
        for (name, _, _, _) in METRIC_MEANINGS {
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
                + crate::derived::DERIVED_NODE_METRICS.len()
        );
        // `__liveness__` is the one an MCP client is most likely to meet first, and the one whose
        // source is least guessable from its name.
        assert_eq!(metric_source(crate::alerts::LIVENESS), "check");
        assert_eq!(metric_source("if_in_util_pct"), "derived");
        assert_eq!(metric_source("cisco_cpu_5min"), "collected");
    }

    /// The family table is the Overview's sectioning, so three things must hold: only the liveness
    /// sentinel is unfiled (it is a rule token, not a series), every family has a member — a family
    /// nothing emits is a heading no node can ever show — and the derived name list is the table's
    /// names in the table's order.
    #[test]
    fn only_the_liveness_sentinel_has_no_family_and_every_family_has_a_member() {
        let unfiled: Vec<&str> = CHECK_FAMILIES
            .iter()
            .filter(|(_, f)| f.is_none())
            .map(|(n, _)| *n)
            .collect();
        assert_eq!(unfiled, [crate::alerts::LIVENESS]);
        for family in CheckFamily::ALL {
            assert!(
                CHECK_FAMILIES.iter().any(|(_, f)| *f == Some(family)),
                "family {family:?} has no check metric"
            );
        }
        let tokens: BTreeSet<&str> = CheckFamily::ALL.iter().map(|f| f.as_str()).collect();
        assert_eq!(
            tokens.len(),
            CheckFamily::ALL.len(),
            "two families share a token"
        );
        assert_eq!(check_family("icmp_loss_pct"), Some(CheckFamily::Icmp));
        assert_eq!(check_family("meraki_device_up"), Some(CheckFamily::Meraki));
        assert_eq!(
            check_family("snmp_sys_uptime_ticks"),
            None,
            "collected, not a check"
        );
        assert_eq!(check_family(crate::alerts::LIVENESS), None);
        let names: Vec<&str> = CHECK_FAMILIES.iter().map(|(n, _)| *n).collect();
        assert_eq!(names, CHECK_METRICS);
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
        expected.extend(crate::derived::DERIVED_NODE_METRICS.iter().map(|d| d.name));

        // A floor, so "the catalogue query stopped matching" cannot pass as "everything is
        // explained": an empty expectation would make the comparison below vacuous.
        assert!(
            expected.len() > 50,
            "only {} explainable metrics found — the catalogue walk drifted",
            expected.len()
        );

        let have: BTreeSet<&str> = METRIC_MEANINGS.iter().map(|(n, _, _, _)| *n).collect();
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
            .map(|(name, sentence, _, _)| ((*name).to_owned(), serde_json::Value::from(*sentence)))
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

    /// The units, grouped by kind, exactly as they are committed for the WebUI (ADR-046 Inc.7).
    ///
    /// Three flat maps rather than one map of pairs: `serde_json` pretty-prints an array across
    /// four lines, so a per-metric tuple would turn a 117-line file into 430 and make every diff
    /// unreadable. Grouped, each metric is one line and the WebUI gets the three lookups it
    /// actually wants without re-deriving them.
    ///
    /// `MetricUnit::None` is **omitted** rather than emitted as null: "no suffix" and "not in the
    /// table" mean the same thing to a renderer, and a null per row would be 22 lines saying
    /// nothing. The *decision* that a metric has no unit is pinned in Rust, where it belongs —
    /// this file is a rendering input, not the record.
    fn units_locale_json() -> String {
        let mut by_kind: std::collections::BTreeMap<
            &str,
            serde_json::Map<String, serde_json::Value>,
        > = std::collections::BTreeMap::new();
        for (name, _, unit, _) in METRIC_MEANINGS {
            let (Some(kind), Some(stored)) = (unit.kind(), unit.stored()) else {
                continue;
            };
            by_kind
                .entry(kind)
                .or_default()
                .insert(name.to_owned(), serde_json::Value::from(stored));
        }
        let map: serde_json::Map<String, serde_json::Value> = by_kind
            .into_iter()
            .map(|(k, v)| (k.to_owned(), serde_json::Value::Object(v)))
            .collect();
        let mut out = serde_json::to_string_pretty(&map).expect("serialize metric units");
        out.push('\n');
        out
    }

    #[test]
    fn the_committed_metric_units_are_current() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../web/src/api/metricUnits.json");
        let generated = units_locale_json();

        if std::env::var_os("UPDATE_METRIC_MEANINGS").is_some() {
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir).expect("create web/src/api");
            }
            std::fs::write(&path, &generated).expect("write metricUnits.json");
            return;
        }

        let committed = std::fs::read_to_string(&path).unwrap_or_default();
        assert_eq!(
            committed, generated,
            "web/src/api/metricUnits.json is stale. Regenerate it with:\n    \
             UPDATE_METRIC_MEANINGS=1 cargo test -p yagra-core the_committed_metric_units_are_current\n\
             then add the Japanese noun for any new counted unit in web/src/locales/ja/format.json \
             (npm run i18n:check will name it)."
        );
    }

    /// Every alert name, metric and non-rule alike, sorted by metric — the English half of the
    /// `alertNames` i18n namespace (ADR-196). The Japanese half is hand-written, and the EN/JA
    /// parity gate is what demands a translation for a new metric.
    fn alert_names_json() -> String {
        let map: serde_json::Map<String, serde_json::Value> = METRIC_MEANINGS
            .iter()
            .map(|(name, _, _, n)| (*name, *n))
            .chain(NON_RULE_ALERT_NAMES)
            .map(|(name, n)| (name.to_owned(), serde_json::Value::from(n.text())))
            .collect();
        let mut out = serde_json::to_string_pretty(&map).expect("serialize alert names");
        out.push('\n');
        out
    }

    /// The metrics whose name is a fault rather than a noun, so the WebUI leaves the condition and
    /// value off. Language-free, so it is generated beside the units rather than into a locale.
    fn alert_flags_json() -> String {
        let mut flags: Vec<&str> = METRIC_MEANINGS
            .iter()
            .map(|(name, _, _, n)| (*name, *n))
            .chain(NON_RULE_ALERT_NAMES)
            .filter(|(_, n)| n.is_flag())
            .map(|(name, _)| name)
            .collect();
        flags.sort_unstable();
        let mut out = serde_json::to_string_pretty(&flags).expect("serialize alert flags");
        out.push('\n');
        out
    }

    #[test]
    fn the_committed_alert_names_are_current() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../web/src");
        let files = [
            (root.join("locales/en/alertNames.json"), alert_names_json()),
            (root.join("api/alertNameFlags.json"), alert_flags_json()),
        ];
        if std::env::var_os("UPDATE_METRIC_MEANINGS").is_some() {
            for (path, generated) in &files {
                std::fs::write(path, generated).expect("write a generated alert-name file");
            }
            return;
        }
        for (path, generated) in &files {
            let committed = std::fs::read_to_string(path).unwrap_or_default();
            assert_eq!(
                &committed,
                generated,
                "{} is stale. Regenerate it with:\n    \
                 UPDATE_METRIC_MEANINGS=1 cargo test -p yagra-core the_committed_alert_names_are_current\n\
                 then write the Japanese name for any new metric in \
                 web/src/locales/ja/alertNames.json (npm run i18n:check will name it).",
                path.display()
            );
        }
    }

    /// The names are only worth having if each says something on its own: none empty, none a
    /// metric name in disguise, and a flag never sits on a metric with a unit — a unit means the
    /// number is a quantity, and hiding a quantity's value would hide the fault's size.
    #[test]
    fn every_alert_name_reads_as_words_and_flags_carry_no_unit() {
        let mut flags = 0;
        for (metric, _, unit, name) in METRIC_MEANINGS {
            let text = name.text();
            assert!(!text.trim().is_empty(), "{metric}: empty alert name");
            assert!(
                !text.contains('_'),
                "{metric}: the alert name {text:?} reads like a metric name"
            );
            if name.is_flag() {
                flags += 1;
                assert_eq!(
                    unit,
                    MetricUnit::None,
                    "{metric}: a flag hides its value, so it cannot carry a unit"
                );
            }
        }
        // A floor in both directions: no flags would mean the shape was dropped, all flags would
        // mean every condition disappeared from every alert.
        assert!(
            (5..30).contains(&flags),
            "{flags} flag-shaped names — the Flag/Value split drifted"
        );
    }

    #[test]
    fn an_alert_title_is_its_name_and_falls_back_to_the_metric() {
        assert_eq!(alert_title("snmp_up"), "SNMP not responding");
        assert_eq!(alert_title(crate::alerts::LIVENESS), "Node not responding");
        assert_eq!(alert_title("cisco_cpu_5min"), "CPU usage (5 min)");
        assert_eq!(
            alert_title(crate::pool_coverage::COVERAGE_METRIC),
            "Too few pollers in pool"
        );
        assert_eq!(
            alert_title(crate::meraki::health::COLLECT_METRIC),
            "Meraki Dashboard collection failing"
        );
        assert_eq!(
            alert_title(&format!("{}BGP flap", crate::events::EVENT_METRIC_PREFIX)),
            "Event rule: BGP flap"
        );
        // A metric a newer core introduced still prints something truthful.
        assert_eq!(alert_title("vendor_x_new_gauge"), "vendor_x_new_gauge");
        assert!(alert_name("snmp_up").is_some_and(AlertName::is_flag));
        assert!(alert_name("icmp_loss_pct").is_some_and(|n| !n.is_flag()));
        // Every row is findable by the binary search, not only the three above.
        for (metric, _, _, name) in METRIC_MEANINGS {
            assert_eq!(alert_name(metric), Some(name), "{metric} is not findable");
        }
    }

    /// Every kind has members, and the two populations that must not drift are counted.
    ///
    /// The floor is the point, not the mapping. A `kind()` that answered `None` for everything
    /// would make `units_locale_json` emit an empty file, and an empty file compares equal to a
    /// committed empty file forever — the check would pass while the WebUI showed no units at all.
    /// So assert what was *classified*, in every arm.
    #[test]
    fn every_unit_kind_has_members_and_the_scaled_set_is_the_size_it_should_be() {
        let mut counts: std::collections::BTreeMap<&str, usize> = std::collections::BTreeMap::new();
        for (_, _, unit, _) in METRIC_MEANINGS {
            *counts.entry(unit.kind().unwrap_or("none")).or_insert(0) += 1;
        }
        assert_eq!(
            counts.keys().copied().collect::<Vec<_>>(),
            ["counted", "none", "scaled", "symbol"],
            "a unit kind with no members means the table drifted away from the enum"
        );
        assert_eq!(counts.values().sum::<usize>(), METRIC_MEANINGS.len());

        // 17 is the number the WebUI's `scalarValueFormat` has to match, and the number ADR-046
        // Inc.7 accepted as the cost of converting values (1 → 17). If this moves, the screen and
        // the threshold rules disagree about one more metric than the ADR says they do.
        assert_eq!(counts["scaled"], 17, "the Scaled population changed");
        // Every scaled metric names its stored unit in words, never a symbol — the payload is what
        // a threshold author has to type in, and `B` would read as the displayed `15.6 GB`.
        for (name, _, unit, _) in METRIC_MEANINGS {
            if let MetricUnit::Scaled(stored) = unit {
                assert!(
                    stored.len() > 4 && !stored.contains('%'),
                    "{name}: a Scaled unit spells its stored unit out ({stored:?})"
                );
            }
        }
    }
}
