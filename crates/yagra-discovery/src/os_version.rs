// SPDX-License-Identifier: AGPL-3.0-only
//! Where a device keeps its OS version, and reading it out of what came back (ADR-138).
//!
//! Pure: the poller holds the SNMP session, this module decides. Three questions, one function each:
//!
//!  - [`oids_to_read`] — having read `sysDescr` and `sysObjectID`, which further instance OIDs this
//!    device's version may be in (often none: many vendors put it in `sysDescr`);
//!  - [`resolve`] — given those values, the version string, or `None`;
//!  - [`sanitize`] — the one cap every version passes, applied on **both** sides of the bus.
//!
//! ## 🚨 The table is copied from LibreNMS, not written from memory
//!
//! LibreNMS keeps, per OS, where its version is (`resources/definitions/os_discovery/*.yaml` and
//! `LibreNMS/OS/*.php`) — and, better, a recorded device answer for each (`tests/snmpsim/*.snmprec`)
//! with the version LibreNMS derived from it (`tests/data/*.json`). Every row below names the file it
//! was copied from, and `every_librenms_fixture_resolves_to_the_version_librenms_derived` holds the
//! table to those answers — 179 recordings, taken at LibreNMS `5b81b470` (2026-09-13). What is
//! copied is OIDs, short patterns and version strings, which are facts about devices, never code.
//!
//! ## Row order is the precedence, and it is load-bearing
//!
//! The first row whose [`Match`]es fit is the only one consulted. A row that recognises a device by
//! `sysDescr` text (NX-OS, ASA, IOS XR) therefore sits **above** the broader row that would also
//! claim it. Inside a row the sources are tried in order and the first non-empty answer wins, which
//! is LibreNMS's order written the other way round: its YAML engine sets the version from `sysDescr`
//! and then lets a non-empty OID overwrite it (`LibreNMS/OS/Traits/YamlOSDiscovery.php`), so here the
//! OID comes first.
//!
//! ## What "empty" means
//!
//! PHP's `empty()`, because that is what decided the answers the fixtures record: `""` and `"0"`
//! are both absent. And `sysDescr` is compared the way LibreNMS stores it — surrounding `"` and a
//! trailing CR/LF removed — which matters in practice: the lab's simulated devices replay those
//! recordings byte for byte, quote included.
//!
//! ## What was left out, deliberately
//!
//! Anything that walks *past* the object it names. LibreNMS appends a Huawei patch version with a
//! GETNEXT on `hwPatchVersion`, and on a device with no patch table that returns whatever object comes
//! next — its own recordings show `… [unlocked]`. Also left out: an index spelled from a serial
//! number (Ruckus SmartZone), Windows build-number naming, and APC's composed display. ADR-138
//! decision 5 has the list.
//!
//! ⚠️ **An ENTITY-MIB row index means something different per vendor** (a Catalyst stack keeps its
//! version at 1001, IOS-XE at 1000, NX-OS at 22). A source here names the exact instance LibreNMS
//! reads, and nothing guesses one (ADR-138 decision 4).

use regex::Regex;
use std::collections::HashMap;
use std::sync::LazyLock;

/// `sysDescr.0`. Read, with [`OID_SYS_OBJECT_ID`], by every identity probe before anything else.
pub const OID_SYS_DESCR: &str = "1.3.6.1.2.1.1.1.0";
/// `sysObjectID.0` — the enterprise OID a device names itself with.
pub const OID_SYS_OBJECT_ID: &str = "1.3.6.1.2.1.1.2.0";
/// The longest version this product keeps. A longer one is cut, never refused: a device that
/// reports a paragraph still has a version worth showing, and refusing would read as "unknown".
pub const OS_VERSION_MAX_CHARS: usize = 128;

/// The further OIDs one device's version may be in, split by how the poller has to read them: the
/// SNMP string readers drop integers, and the numeric GET drops strings.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Reads {
    pub strings: Vec<&'static str>,
    pub integers: Vec<&'static str>,
}

impl Reads {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.strings.is_empty() && self.integers.is_empty()
    }
}

/// What came back for a [`Reads`], keyed by instance OID. A value the device did not return is
/// simply absent.
#[derive(Debug, Default, Clone)]
pub struct Answers {
    pub strings: HashMap<String, String>,
    pub integers: HashMap<String, i64>,
}

/// One condition a row recognises a device by.
#[derive(Debug, Clone, Copy)]
enum Match {
    /// `sysObjectID` starts with this — LibreNMS's prefix verbatim, without its leading dot. Some end
    /// in `.` and some do not; that is LibreNMS's choice and it is kept, because the fixtures were
    /// detected with it.
    ObjectId(&'static str),
    /// `sysDescr` contains this text (case-sensitive, as LibreNMS's detection strings are).
    Descr(&'static str),
    /// `sysDescr` starts with this text (LibreNMS's `^…` detection patterns that are plain text).
    DescrStarts(&'static str),
    /// `sysDescr` matches this pattern — for the detection patterns that are not plain text.
    DescrPattern(&'static str),
    /// Both at once: `sysObjectID` starts with the prefix **and** `sysDescr` matches the pattern
    /// (LibreNMS's discovery groups that list the two together).
    ObjectIdAndDescrPattern(&'static str, &'static str),
}

/// One value inside a [`Source::Joined`].
#[derive(Debug, Clone, Copy)]
enum Part {
    Str(&'static str),
    Int(&'static str),
}

/// One place a row's version may be.
#[derive(Debug, Clone, Copy)]
enum Source {
    /// The string at an instance OID, as it is.
    Str(&'static str),
    /// The string at an instance OID, cut by `pattern` and rebuilt from `template` (`${version}`).
    StrCut(&'static str, &'static str, &'static str),
    /// The string at `oid`, but only when the integer at `gate` equals `equals` — Cisco IOS takes
    /// `entPhysicalSoftwareRev.1` only when row 1 is the chassis (`entPhysicalContainedIn.1 == 0`).
    StrGated {
        oid: &'static str,
        gate: &'static str,
        equals: i64,
    },
    /// A pattern over `sysDescr`, rebuilt from `template`.
    Descr(&'static str, &'static str),
    /// As [`Source::Descr`], keeping only the first line of what it built (Cisco IOS pattern 4).
    DescrFirstLine(&'static str, &'static str),
    /// Several instance OIDs put into `template` by position (`{0}.{1}.{2}`) — used when at least
    /// one of them is non-empty, exactly as LibreNMS's `{{ … }}` templates are.
    Joined(&'static [Part], &'static str),
    /// Huawei VRP: `Version x` from `sysDescr`, followed by ` (Vnnn…)` when `sysDescr` names a
    /// release (`LibreNMS/OS/Vrp.php`). Its own variant because it composes two patterns and is
    /// **nothing** when the first misses, even if the second hits.
    HuaweiVrp,
}

const VRP_VERSION: &str = r"Version (\S+)";
const VRP_RELEASE: &str = r"\((?<hardware>[^)]+) (?<version>V[0-9]{3}R[0-9]{3}[0-9A-Z]+)";

/// One OS family: how it is recognised, where its version is, and which LibreNMS file said so.
// `os` and `origin` are read by the tests only — which row a fixture must land on, and provenance
// kept as data so the test demanding every row name its source cannot be satisfied by a comment
// someone forgot to write. Neither is ever a runtime decision.
#[cfg_attr(not(test), allow(dead_code))]
struct Row {
    /// LibreNMS's names for the OS families this row covers, which is how the fixtures are named.
    os: &'static [&'static str],
    /// The LibreNMS file(s) this row was copied from.
    origin: &'static str,
    /// The row applies when any of these match…
    when: &'static [Match],
    /// …and none of these do (LibreNMS's `_except`).
    unless: &'static [Match],
    sources: &'static [Source],
}

/// Enterprise-prefix shorthand, so a row reads as the part that differs.
macro_rules! ent {
    ($suffix:literal) => {
        concat!("1.3.6.1.4.1.", $suffix)
    };
}

/// `entPhysicalSoftwareRev.<index>` — see the module doc before adding one.
macro_rules! ent_software_rev {
    ($index:literal) => {
        concat!("1.3.6.1.2.1.47.1.1.1.1.10.", $index)
    };
}

/// Cisco's four `sysDescr` patterns (`LibreNMS/OS/Shared/Cisco.php:112-121`), tried in order.
const CISCO_IOS_1: &str =
    r"^Cisco IOS Software, .+? Software \([^\-]+-([^\-]+)-\w\),.+?Version ([^, ]+)";
const CISCO_IOS_2: &str = r"Cisco Internetwork Operating System Software\s+IOS \(tm\) [^ ]+ Software \([^\-]+-([^\-]+)-\w\),.+?Version ([^, ]+)";
const CISCO_IOS_3: &str =
    r"^Cisco IOS Software \[([^\]]+)\],.+Software \(([^\)]+)\), Version ([^, ]+)";
// PHP writes `(\, )?`; the escaped comma is a plain comma in PCRE and is spelled plainly here.
const CISCO_IOS_4: &str = r"^Cisco IOS Software.*?, .+? Software(, )?([\s\w\d]+)? \([^\-]+-([\w\d]+)-\w\), Version ([^,]+)";

static ROWS: &[Row] = &[
    Row {
        os: &["nxos"],
        origin: "resources/definitions/os_discovery/nxos.yaml",
        when: &[Match::Descr("NX-OS(tm)"), Match::Descr("Cisco NX-OS")],
        unless: &[Match::Descr("Cisco NX-OS(tm) ucs")],
        // No `sysDescr` fallback: LibreNMS never parses "Version x" for NX-OS, and four of its six
        // NX-OS recordings have no version at all.
        sources: &[
            Source::Str(ent_software_rev!("22")),
            Source::Str(ent_software_rev!("24")),
        ],
    },
    Row {
        os: &["asa"],
        origin: "resources/definitions/os_discovery/asa.yaml",
        when: &[
            Match::DescrStarts("Cisco Adaptive Security Appliance"),
            Match::DescrStarts("Cisco Industrial Security Appliance"),
        ],
        unless: &[],
        sources: &[
            Source::Str(ent_software_rev!("1")),
            Source::Str(ent_software_rev!("4")),
            Source::Descr(r"Version (?<version>.*)", "${version}"),
        ],
    },
    Row {
        os: &["ftd"],
        origin: "resources/definitions/os_discovery/ftd.yaml",
        when: &[
            Match::ObjectId(ent!("9.1.1902")),
            Match::ObjectId(ent!("9.1.2313")),
            Match::ObjectId(ent!("9.1.2315")),
            Match::ObjectId(ent!("9.1.2319")),
            Match::ObjectId(ent!("9.1.2404")),
            Match::ObjectId(ent!("9.1.2405")),
            Match::ObjectId(ent!("9.1.2406")),
            Match::ObjectId(ent!("9.1.2407")),
            Match::ObjectId(ent!("9.1.2409")),
            Match::ObjectId(ent!("9.1.2483")),
            Match::ObjectId(ent!("9.1.2662")),
            Match::ObjectId(ent!("9.1.2663")),
            Match::ObjectId(ent!("9.1.2991")),
            Match::ObjectId(ent!("9.1.2774")),
            Match::ObjectId(ent!("9.1.2775")),
            Match::ObjectId(ent!("9.1.2295")),
            Match::ObjectId(ent!("9.1.2294")),
            Match::ObjectId(ent!("9.1.2778")),
            Match::ObjectId(ent!("9.1.2870")),
            Match::ObjectId(ent!("9.1.3041")),
            Match::ObjectId(ent!("9.1.3053")),
            Match::ObjectId(ent!("9.1.3054")),
            Match::ObjectId(ent!("9.1.3055")),
            Match::ObjectId(ent!("9.1.3056")),
            Match::ObjectId(ent!("9.1.3057")),
            Match::ObjectId(ent!("9.1.3166")),
            Match::ObjectId(ent!("9.1.3257")),
            Match::ObjectId(ent!("9.1.3305")),
            Match::ObjectId(ent!("9.1.3043")),
        ],
        unless: &[],
        sources: &[
            Source::Str(ent_software_rev!("1")),
            Source::Str(ent_software_rev!("4")),
            Source::Str(ent_software_rev!("10")),
            Source::Descr(r"Version (?<version>[^,]+)", "${version}"),
        ],
    },
    Row {
        os: &["iosxr"],
        origin: "LibreNMS/OS/Iosxr.php",
        when: &[Match::Descr("IOS XR")],
        unless: &[],
        sources: &[
            Source::Str(ent_software_rev!("1")),
            Source::Descr(
                r"^Cisco IOS XR Software \(Cisco ([^\)]+)\),\s+Version ([^\[]+)\[([^\]]+)\]",
                "${2}",
            ),
            Source::Descr(
                r"^Cisco IOS XR Software \(([^\)]+)\),\s+Version\s+([^\s]+)",
                "${2}",
            ),
        ],
    },
    Row {
        os: &["ciscowlc"],
        origin: "resources/definitions/os_discovery/ciscowlc.yaml",
        when: &[
            Match::Descr("Cisco Controller"),
            Match::Descr("Cisco Business Wireless"),
        ],
        unless: &[],
        sources: &[Source::Str(ent_software_rev!("1"))],
    },
    Row {
        os: &["ios", "iosxe"],
        origin: "LibreNMS/OS/Shared/Cisco.php (discoverOS) + os_detection/ios.yaml, iosxe.yaml",
        when: &[
            Match::Descr("Cisco Internetwork Operating System Software"),
            Match::Descr("IOS (tm)"),
            Match::Descr("Cisco IOS Software"),
            Match::Descr("Global Site Selector"),
            Match::Descr("IOS-XE"),
            Match::Descr("IOSXE"),
            Match::Descr("LINUX_IOSD"),
            Match::Descr("CAT3K_CAA"),
            Match::ObjectId(ent!("9.1.2330")),
            Match::ObjectId(ent!("9.1.2331")),
            Match::ObjectId(ent!("9.1.2332")),
            Match::ObjectId(ent!("9.1.2333")),
            Match::ObjectId(ent!("9.1.2683")),
            Match::ObjectId(ent!("9.1.2684")),
            Match::ObjectId(ent!("9.1.2685")),
            Match::ObjectId(ent!("9.1.2686")),
            Match::ObjectId(ent!("9.1.2687")),
        ],
        unless: &[],
        sources: &[
            Source::StrGated {
                oid: ent_software_rev!("1"),
                gate: "1.3.6.1.2.1.47.1.1.1.1.4.1",
                equals: 0,
            },
            Source::Descr(CISCO_IOS_1, "${2}"),
            Source::Descr(CISCO_IOS_2, "${2}"),
            // The image name and the version, e.g. `X86_64_LINUX_IOSD-UNIVERSALK9-M 17.12.3`.
            Source::Descr(CISCO_IOS_3, "${2} ${3}"),
            Source::DescrFirstLine(CISCO_IOS_4, "${4}"),
        ],
    },
    Row {
        os: &["junos"],
        origin: "LibreNMS/OS/Junos.php",
        when: &[
            Match::ObjectId(ent!("2636")),
            Match::Descr("kernel JUNOS"),
            Match::Descr("kernel Junos"),
        ],
        unless: &[],
        sources: &[
            // jnxVirtualChassisMemberSWVersion.0 — member 0 of a virtual chassis.
            Source::Str(ent!("2636.3.40.1.4.1.1.1.5.0")),
            // hrSWInstalledName.1 on Junos Evolved, .2 on Junos.
            Source::StrCut("1.3.6.1.2.1.25.6.3.1.2.1", r"^junos-evo.*?(\d+\.\d+.*)$", "${1}"),
            Source::StrCut("1.3.6.1.2.1.25.6.3.1.2.2", r"^JUNOS.*\[([^\]]+)\]", "${1}"),
            Source::Descr(
                r"Juniper Networks, Inc. (?<hardware>\S+) .* kernel JUNOS (?<version>[^, ]+)[, ]",
                "${version}",
            ),
        ],
    },
    Row {
        os: &["vrp"],
        origin: "resources/definitions/os_discovery/vrp.yaml + LibreNMS/OS/Vrp.php (without the patch GETNEXT)",
        when: &[
            Match::Descr("VRP (R) Software"),
            Match::Descr("VRP Software Version"),
            Match::Descr("Software Version VRP"),
            Match::Descr("Versatile Routing Platform Software"),
        ],
        unless: &[],
        sources: &[Source::HuaweiVrp],
    },
    Row {
        os: &["fortigate"],
        origin: "resources/definitions/os_discovery/fortigate.yaml (FORTINET-FORTIGATE-MIB::fgSysVersion.0)",
        when: &[
            Match::ObjectId(ent!("12356.15")),
            Match::ObjectId(ent!("12356.101.1")),
        ],
        unless: &[],
        sources: &[Source::Str(ent!("12356.101.4.1.1.0"))],
    },
    Row {
        os: &["arista_eos"],
        origin: "resources/definitions/os_discovery/arista_eos.yaml",
        when: &[Match::ObjectId(ent!("30065.1"))],
        unless: &[],
        sources: &[Source::Descr(
            r" version (?<version>.+) running on .+ (?<hardware>\S+)$",
            "${version}",
        )],
    },
    Row {
        os: &["routeros"],
        origin: "resources/definitions/os_discovery/routeros.yaml (MIKROTIK-MIB::mtxrLicVersion.0)",
        when: &[Match::ObjectId(ent!("14988.1"))],
        unless: &[],
        sources: &[Source::Str(ent!("14988.1.1.4.4.0"))],
    },
    Row {
        os: &["panos"],
        origin: "resources/definitions/os_discovery/panos.yaml (PAN-COMMON-MIB::panSysSwVersion.0)",
        when: &[Match::Descr("Palo Alto Networks")],
        unless: &[],
        sources: &[Source::Str(ent!("25461.2.1.2.1.1.0"))],
    },
    Row {
        os: &["gaia"],
        origin: "resources/definitions/os_discovery/gaia.yaml (CHECKPOINT-MIB::svnVersion.0)",
        // LibreNMS also detects Gaia on net-snmp's `8072.3.2.10` behind an extra GET; that form
        // is not copied (a generic Linux host is out of scope, ADR-138 decision 6).
        when: &[
            Match::ObjectId(ent!("2620.1.6.123.1")),
            Match::ObjectId(ent!("2620.1.1")),
        ],
        unless: &[],
        sources: &[Source::Str(ent!("2620.1.6.4.1.0"))],
    },
    Row {
        os: &["f5"],
        origin: "resources/definitions/os_discovery/f5.yaml (F5-BIGIP-SYSTEM-MIB::sysProductVersion.0)",
        when: &[Match::ObjectId(ent!("3375.2.1"))],
        unless: &[],
        sources: &[Source::Str(ent!("3375.2.1.4.2.0"))],
    },
    Row {
        os: &["netscaler"],
        origin: "resources/definitions/os_discovery/netscaler.yaml (NS-ROOT-MIB::sysBuildVersion.0)",
        when: &[
            Match::ObjectId(ent!("5951.1")),
            Match::ObjectId(ent!("5951.6")),
        ],
        unless: &[],
        sources: &[Source::StrCut(
            ent!("5951.4.1.1.1.0"),
            r"NetScaler (?<version>[^:]+): (?<features>[^,]+),",
            "${version}",
        )],
    },
    Row {
        os: &["acos"],
        origin: "resources/definitions/os_discovery/acos.yaml (A10-AX-MIB::axSysPrimaryVersionOnDisk.0)",
        when: &[Match::ObjectId(ent!("22610.1.3"))],
        unless: &[],
        // ⚠️ "Primary version on disk" — what the device will boot, not necessarily what it runs.
        sources: &[
            Source::Str(ent!("22610.2.4.1.1.1.0")),
            Source::Descr(r"(?<hardware>\S+( TPS)?), ACOS (?<version>[^,]+)", "${version}"),
        ],
    },
    Row {
        os: &["arubaos-cx"],
        origin: "resources/definitions/os_discovery/arubaos-cx.yaml",
        when: &[Match::ObjectId(ent!("47196.4.1.1.1"))],
        unless: &[],
        // LibreNMS applies all three patterns and the last match wins; here the same answer is
        // spelled first-match-wins by listing them in reverse.
        sources: &[
            Source::Str(ent_software_rev!("1")),
            Source::Str(ent_software_rev!("101001")),
            Source::Descr(r"(?<version>\D{2}\.\d{2}\.\d{2}\.\d{4})", "${version}"),
            Source::Descr(
                r" (?<hardware>\d{4,}) (?<version>\D{2}\.\d{2}\.\d{2}\.\d{4})",
                "${version}",
            ),
            Source::Descr(
                r" (?<hardware>\d{4,}.*) Swch (?<version>\D{2}\.\d{2}\.\d{2}\.\d{4})",
                "${version}",
            ),
        ],
    },
    Row {
        os: &["arubaos"],
        origin: "resources/definitions/os_discovery/arubaos.yaml",
        // Above PowerConnect and DNOS on purpose: Dell's W-series controllers run ArubaOS under
        // Dell's `674.10895.` enterprise number, and LibreNMS recognises them by `sysDescr`.
        when: &[
            Match::ObjectId(ent!("14823.")),
            Match::ObjectId(ent!("6486.800.1.1.2.2.2.")),
            Match::Descr("ArubaOS"),
        ],
        unless: &[
            Match::ObjectId(ent!("14823.1.2")),
            Match::ObjectId(ent!("14823.1.6")),
        ],
        sources: &[Source::Descr(
            r"(\(MODEL: (?<hardware>.+)\),)? Version (?<version>\S+)",
            "${version}",
        )],
    },
    Row {
        os: &["comware"],
        origin: "resources/definitions/os_discovery/comware.yaml",
        when: &[Match::ObjectId(ent!("25506."))],
        unless: &[],
        sources: &[Source::Descr(
            r"Version (?<version>[0-9.]+).*(Release|ESS) (?<features>[R0-9P]+).*[\n ](HPE |HPE FF |HP |H3C )(?<hardware>.*)[\r ][\n ]",
            "${version}",
        )],
    },
    Row {
        os: &["dnos"],
        origin: "resources/definitions/os_discovery/dnos.yaml",
        when: &[Match::ObjectId(ent!("6027.1."))],
        unless: &[
            Match::Descr("Force10 Operating System"),
            Match::Descr("Force10 Networks Real Time Operating System Softw"),
        ],
        sources: &[
            Source::Str(ent!("674.10895.3000.1.2.100.4.0")),
            Source::Str(ent!("6027.3.26.1.3.4.1.10.1")),
            Source::Descr(r"Software Version: (?<version>\S+)", "${version}"),
        ],
    },
    Row {
        os: &["powerconnect"],
        origin: "resources/definitions/os_discovery/powerconnect.yaml",
        when: &[Match::ObjectId(ent!("674.10895"))],
        unless: &[],
        sources: &[
            Source::Str(ent!("674.10895.3000.1.2.100.4.0")),
            Source::Descr(
                r"(?<hardware>(Power[Cc]onnect |Dell Networking |Dell EMC Networking )?[A-Z]?\d{2,}[A-Z\-]*)(, (?<version>\d+\.[\d.]+))?",
                "${version}",
            ),
        ],
    },
    Row {
        os: &["xos"],
        origin: "resources/definitions/os_discovery/xos.yaml",
        when: &[Match::ObjectId(ent!("1916.2."))],
        unless: &[],
        sources: &[Source::Descr(
            r"(\((?<hardware>[^)]+)\))? version (?<version>[\d.]+) (?<features>\S+)",
            "${version}",
        )],
    },
    Row {
        os: &["ironware"],
        origin: "resources/definitions/os_discovery/ironware.yaml",
        when: &[Match::Descr("IronWare")],
        unless: &[],
        sources: &[
            Source::Str(ent!("1991.1.1.2.1.11.0")),
            Source::Descr(r"IronWare Version V(?<version>.*) Compiled on", "${version}"),
        ],
    },
    Row {
        os: &["ruckuswireless", "ruckuswireless-unleashed"],
        origin: "resources/definitions/os_discovery/ruckuswireless.yaml + ruckuswireless-unleashed.yaml",
        // One row for two families, because they cannot be told apart by what an identity probe
        // reads first: both use `25053.3.1.5.15`, and LibreNMS separates them with an extra GET
        // (`…1.2.1.1.1.1.9.0` starts with "ZD"). Each answers only its own MIB, so trying the
        // ZoneDirector objects and then the Unleashed ones lands on the same answer.
        when: &[
            Match::ObjectId(ent!("25053.3.1.5")),
            Match::DescrPattern(r"^Ruckus Wireless R\d{3}"),
        ],
        unless: &[],
        sources: &[
            Source::Joined(
                &[
                    Part::Str(ent!("25053.1.2.1.1.1.1.18.0")),
                    Part::Str(ent!("25053.1.2.1.1.1.1.20.0")),
                ],
                "{0} ({1})",
            ),
            Source::Joined(
                &[
                    Part::Str(ent!("25053.1.15.1.1.1.1.18.0")),
                    Part::Str(ent!("25053.1.15.1.1.1.1.20.0")),
                ],
                "{0} ({1})",
            ),
        ],
    },
    Row {
        os: &["timos"],
        origin: "resources/definitions/os_discovery/timos.yaml (TIMETRA-SYSTEM-MIB sgiSw*)",
        when: &[Match::ObjectId(ent!("6527."))],
        unless: &[],
        // Major and minor are Gauge32, which is why this row needs the numeric reader.
        sources: &[Source::Joined(
            &[
                Part::Int(ent!("6527.3.1.2.1.1.5.0")),
                Part::Int(ent!("6527.3.1.2.1.1.6.0")),
                Part::Str(ent!("6527.3.1.2.1.1.7.0")),
            ],
            "{0}.{1}.{2}",
        )],
    },
    Row {
        os: &["aos6"],
        origin: "resources/definitions/os_discovery/aos6.yaml",
        when: &[Match::ObjectId(ent!("6486.800.1.1.2.1."))],
        unless: &[],
        sources: &[Source::Descr(
            r"(?<hardware>OS\S*)? ?(?<version>\d+\.\d+\.\S*)",
            "${version}",
        )],
    },
    Row {
        os: &["aos7"],
        origin: "resources/definitions/os_discovery/aos7.yaml",
        when: &[Match::ObjectId(ent!("6486.801."))],
        unless: &[],
        sources: &[Source::Descr(
            r"(?<hardware>OS\S+)? ?(?<version>\d+\.\d+\.\S+)",
            "${version}",
        )],
    },
    Row {
        os: &["edgeos"],
        origin: "resources/definitions/os_discovery/edgeos.yaml",
        when: &[Match::ObjectId(ent!("41112.1.5"))],
        unless: &[],
        sources: &[
            Source::Str(ent!("41112.1.5.1.3.0")),
            Source::Descr(r"v(?<version>\d+\.\d+\.\d+)", "${version}"),
        ],
    },
    Row {
        os: &["zynos"],
        origin: "LibreNMS/OS/Shared/Zyxel.php (sysSwVersionString.0, before \" | \")",
        when: &[Match::ObjectId(ent!("890"))],
        unless: &[],
        sources: &[Source::StrCut(
            ent!("890.1.15.3.1.6.0"),
            r"^(?<version>.*?)(?: \| |$)",
            "${version}",
        )],
    },
    Row {
        os: &["netgear"],
        origin: "resources/definitions/os_discovery/netgear.yaml",
        when: &[
            Match::ObjectId(ent!("4526")),
            Match::ObjectId(ent!("89.1.1.1.3.6.1.4.1.4526.100.4")),
            Match::Descr("ProSafe"),
        ],
        unless: &[],
        sources: &[
            Source::Str(ent_software_rev!("1")),
            Source::Descr(r"^(?<hardware>\S+) .*, (?<version>[\d.]+),", "${version}"),
        ],
    },
    Row {
        os: &["dlink"],
        origin: "resources/definitions/os_discovery/dlink.yaml",
        when: &[Match::ObjectId(ent!("171.10."))],
        unless: &[Match::ObjectId(ent!("171.10.37."))],
        sources: &[
            Source::Str(ent!("171.14.5.1.8.1.3.1")),
            Source::Str("1.3.6.1.2.1.16.19.2.0"),
            Source::Str(ent!("171.12.11.1.9.4.1.11.1")),
            Source::Descr(r"(D-Link )?(?<hardware>\S+) (?<version>[0-9.]+)?", "${version}"),
        ],
    },
    Row {
        os: &["jetstream"],
        origin: "resources/definitions/os_discovery/jetstream.yaml (TPLINK-SYSINFO-MIB::tpSysInfoSwVersion.0)",
        // Above the generic TP-Link row: `11863.` is a prefix of this one.
        when: &[Match::ObjectId(ent!("11863.5.")), Match::Descr("JetStream")],
        unless: &[],
        sources: &[Source::Str(ent!("11863.6.1.1.6.0"))],
    },
    Row {
        os: &["tplink"],
        origin: "resources/definitions/os_discovery/tplink.yaml (RMON-MIB::rmon.19.2.0)",
        when: &[
            Match::ObjectId(ent!("11863.")),
            Match::ObjectIdAndDescrPattern(ent!("16972"), r"Build \d+ Rel"),
        ],
        unless: &[],
        sources: &[Source::Str("1.3.6.1.2.1.16.19.2.0")],
    },
    Row {
        os: &["vmware-esxi"],
        origin: "resources/definitions/os_discovery/vmware-esxi.yaml (VMWARE-SYSTEM-MIB::vmwProdVersion.0)",
        when: &[Match::ObjectId(ent!("6876.4.1"))],
        unless: &[],
        sources: &[Source::Str(ent!("6876.1.2.0"))],
    },
    Row {
        os: &["netapp"],
        origin: "resources/definitions/os_discovery/netapp.yaml (NETAPP-MIB::productVersion.0)",
        when: &[Match::Descr("NetApp"), Match::Descr("NETAPP")],
        unless: &[],
        sources: &[Source::StrCut(
            ent!("789.1.1.2.0"),
            r"NetApp Release (?<version>.*?):",
            "${version}",
        )],
    },
    Row {
        os: &["gaia", "dsm"],
        origin: "resources/definitions/os_discovery/gaia.yaml (CHECKPOINT-MIB::svnVersion.0) + dsm.yaml (SYNOLOGY-SYSTEM-MIB::version.0)",
        // Last, because it is the broadest: an appliance built on Linux and net-snmp. LibreNMS tells
        // a Check Point (`8072.3.2.10` whose `osName.0` says Gaia) and a Synology (`Linux ` whose
        // `systemStatus.0` answers) apart with an extra GET each. Here the version objects are that
        // test — each answers only on its own vendor — so a plain Linux host costs two reads an
        // hour and resolves to nothing (ADR-138 decision 6: a kernel version is not shown).
        when: &[
            Match::ObjectId(ent!("8072.3.2.10")),
            Match::DescrStarts("Linux "),
        ],
        unless: &[],
        sources: &[
            Source::Str(ent!("2620.1.6.4.1.0")),
            Source::StrCut(
                ent!("6574.1.5.3.0"),
                r"(DSM )?(?<version>\S+)",
                "${version}",
            ),
        ],
    },
];

/// Every pattern in the table, compiled once. A pattern that does not compile is left out and its
/// source never answers — `every_pattern_compiles` is what stops one shipping.
static PATTERNS: LazyLock<HashMap<&'static str, Regex>> = LazyLock::new(|| {
    all_patterns()
        .filter_map(|pattern| Regex::new(pattern).ok().map(|re| (pattern, re)))
        .collect()
});

fn all_patterns() -> impl Iterator<Item = &'static str> {
    let detection = ROWS
        .iter()
        .flat_map(|row| row.when.iter().chain(row.unless))
        .filter_map(|m| match *m {
            Match::DescrPattern(pattern) | Match::ObjectIdAndDescrPattern(_, pattern) => {
                Some(pattern)
            }
            Match::ObjectId(_) | Match::Descr(_) | Match::DescrStarts(_) => None,
        });
    let sources = ROWS
        .iter()
        .flat_map(|row| row.sources.iter())
        .flat_map(|source| match *source {
            Source::StrCut(_, pattern, _)
            | Source::Descr(pattern, _)
            | Source::DescrFirstLine(pattern, _) => vec![pattern],
            Source::HuaweiVrp => vec![VRP_VERSION, VRP_RELEASE],
            Source::Str(_) | Source::StrGated { .. } | Source::Joined(..) => Vec::new(),
        });
    detection.chain(sources)
}

/// `sysDescr` as LibreNMS stores it: a trailing CR/LF and the surrounding quotes removed.
fn as_librenms_reads(sys_descr: &str) -> &str {
    sys_descr
        .trim_end_matches(['\r', '\n'])
        .trim_matches('"')
        .trim_end_matches(['\r', '\n'])
}

/// PHP's `empty()` on a string, which is what the recorded answers were decided with.
fn php_empty(value: &str) -> bool {
    value.is_empty() || value == "0"
}

fn matches(m: Match, sys_object_id: &str, sys_descr: &str) -> bool {
    let oid_starts = |prefix: &str| !sys_object_id.is_empty() && sys_object_id.starts_with(prefix);
    let descr_fits = |pattern: &str| {
        PATTERNS
            .get(pattern)
            .is_some_and(|re| re.is_match(sys_descr))
    };
    match m {
        Match::ObjectId(prefix) => oid_starts(prefix),
        Match::Descr(needle) => sys_descr.contains(needle),
        Match::DescrStarts(prefix) => sys_descr.starts_with(prefix),
        Match::DescrPattern(pattern) => descr_fits(pattern),
        Match::ObjectIdAndDescrPattern(prefix, pattern) => {
            oid_starts(prefix) && descr_fits(pattern)
        }
    }
}

fn row_for(sys_object_id: Option<&str>, sys_descr: Option<&str>) -> Option<&'static Row> {
    let oid = sys_object_id.unwrap_or_default().trim_start_matches('.');
    let descr = as_librenms_reads(sys_descr.unwrap_or_default());
    ROWS.iter().find(|row| {
        row.when.iter().any(|m| matches(*m, oid, descr))
            && !row.unless.iter().any(|m| matches(*m, oid, descr))
    })
}

/// The instance OIDs, beyond `sysDescr` and `sysObjectID`, that this device's version may be in —
/// empty when the table does not know the device or keeps its version in `sysDescr`.
#[must_use]
pub fn oids_to_read(sys_object_id: Option<&str>, sys_descr: Option<&str>) -> Reads {
    let mut reads = Reads::default();
    let Some(row) = row_for(sys_object_id, sys_descr) else {
        return reads;
    };
    let add = |list: &mut Vec<&'static str>, oid: &'static str| {
        if !list.contains(&oid) {
            list.push(oid);
        }
    };
    for source in row.sources {
        match *source {
            Source::Str(oid) | Source::StrCut(oid, ..) => add(&mut reads.strings, oid),
            Source::StrGated { oid, gate, .. } => {
                add(&mut reads.strings, oid);
                add(&mut reads.integers, gate);
            }
            Source::Joined(parts, _) => {
                for part in parts {
                    match *part {
                        Part::Str(oid) => add(&mut reads.strings, oid),
                        Part::Int(oid) => add(&mut reads.integers, oid),
                    }
                }
            }
            Source::Descr(..) | Source::DescrFirstLine(..) | Source::HuaweiVrp => {}
        }
    }
    reads
}

/// The device's OS version, from its `sysDescr`, `sysObjectID` and the [`Answers`] to
/// [`oids_to_read`]. `None` when the table does not know the device, or knows it and finds no
/// version — the two are deliberately not told apart, because to the node page they are the same
/// dash.
#[must_use]
pub fn resolve(
    sys_object_id: Option<&str>,
    sys_descr: Option<&str>,
    answers: &Answers,
) -> Option<String> {
    let row = row_for(sys_object_id, sys_descr)?;
    let descr = as_librenms_reads(sys_descr.unwrap_or_default());
    row.sources
        .iter()
        .find_map(|source| from_source(*source, descr, answers))
}

fn from_source(source: Source, descr: &str, answers: &Answers) -> Option<String> {
    let string = |oid: &str| {
        answers
            .strings
            .get(oid)
            .map(String::as_str)
            .filter(|v| !php_empty(v))
    };
    match source {
        Source::Str(oid) => string(oid).and_then(sanitize),
        Source::StrCut(oid, pattern, template) => {
            string(oid).and_then(|v| expand(pattern, template, v, false))
        }
        Source::StrGated { oid, gate, equals } => {
            if answers.integers.get(gate) == Some(&equals) {
                string(oid).and_then(sanitize)
            } else {
                None
            }
        }
        Source::Descr(pattern, template) => expand(pattern, template, descr, false),
        Source::DescrFirstLine(pattern, template) => expand(pattern, template, descr, true),
        Source::Joined(parts, template) => {
            let values: Vec<String> = parts
                .iter()
                .map(|part| match *part {
                    Part::Str(oid) => answers.strings.get(oid).cloned().unwrap_or_default(),
                    Part::Int(oid) => answers
                        .integers
                        .get(oid)
                        .map(i64::to_string)
                        .unwrap_or_default(),
                })
                .collect();
            if values.iter().all(|v| php_empty(v)) {
                return None;
            }
            let mut out = template.to_owned();
            for (i, value) in values.iter().enumerate() {
                out = out.replace(&format!("{{{i}}}"), value);
            }
            sanitize(&out)
        }
        Source::HuaweiVrp => {
            let version = PATTERNS.get(VRP_VERSION)?.captures(descr)?.get(1)?.as_str();
            let release = PATTERNS
                .get(VRP_RELEASE)
                .and_then(|re| re.captures(descr))
                .and_then(|caps| caps.name("version"))
                .map(|m| m.as_str())
                .filter(|r| !php_empty(r));
            match release {
                Some(release) => sanitize(&format!("{version} ({release})")),
                None => sanitize(version),
            }
        }
    }
}

fn expand(pattern: &str, template: &str, text: &str, first_line: bool) -> Option<String> {
    let caps = PATTERNS.get(pattern)?.captures(text)?;
    let mut out = String::new();
    caps.expand(template, &mut out);
    if first_line {
        // `Cisco.php`: keep the first line, unless it is empty.
        if let Some(line) = out.lines().next().filter(|l| !php_empty(l)) {
            out = line.to_owned();
        }
    }
    sanitize(&out)
}

/// Make a device-supplied version safe to store and show: control characters and runs of
/// whitespace become one space, the ends are trimmed, and the result is cut at
/// [`OS_VERSION_MAX_CHARS`]. `None` when nothing is left.
///
/// Applied where the version is resolved **and again at ingest**, because core cannot assume the
/// poller that sent it is this one.
#[must_use]
pub fn sanitize(raw: &str) -> Option<String> {
    let mut out = String::new();
    let mut count = 0usize;
    let mut gap = false;
    for c in raw.chars() {
        if c.is_whitespace() || c.is_control() {
            gap = count > 0;
            continue;
        }
        let needed = if gap { 2 } else { 1 };
        if count + needed > OS_VERSION_MAX_CHARS {
            break;
        }
        if gap {
            out.push(' ');
            count += 1;
            gap = false;
        }
        out.push(c);
        count += 1;
    }
    (!out.is_empty()).then_some(out)
}

/// Split an instance OID into the column a v2c walk reads it from and its trailing index:
/// `1.3.6.1.4.1.12356.101.4.1.1.0` → (`1.3.6.1.4.1.12356.101.4.1.1`, `0`). The v2c transport's
/// scalar GET returns numbers only, so a string scalar is reached by walking its column.
#[must_use]
pub fn walk_column(instance: &str) -> Option<(&str, u32)> {
    let (column, index) = instance.rsplit_once('.')?;
    Some((column, index.parse().ok()?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    #[test]
    fn sanitize_folds_whitespace_and_control_characters() {
        assert_eq!(
            sanitize("  Version 15.0(2a)EX5,\r\n RELEASE\tSOFTWARE  ").as_deref(),
            Some("Version 15.0(2a)EX5, RELEASE SOFTWARE")
        );
        assert_eq!(sanitize(" \n\t "), None);
        assert_eq!(sanitize(""), None);
        assert_eq!(sanitize("a\u{0}b").as_deref(), Some("a b"));
    }

    #[test]
    fn sanitize_cuts_at_the_cap_on_a_character_boundary() {
        let long = "版".repeat(OS_VERSION_MAX_CHARS + 20);
        let cut = sanitize(&long).expect("a version");
        assert_eq!(cut.chars().count(), OS_VERSION_MAX_CHARS);
        // A gap exactly at the cap must not leave a trailing space behind.
        let spaced = format!("{} tail", "x".repeat(OS_VERSION_MAX_CHARS));
        assert_eq!(
            sanitize(&spaced).expect("a version"),
            "x".repeat(OS_VERSION_MAX_CHARS)
        );
        let one_short = format!("{} tail", "x".repeat(OS_VERSION_MAX_CHARS - 1));
        assert_eq!(
            sanitize(&one_short).expect("a version"),
            "x".repeat(OS_VERSION_MAX_CHARS - 1)
        );
    }

    #[test]
    fn walk_column_splits_off_the_trailing_index() {
        assert_eq!(
            walk_column("1.3.6.1.4.1.12356.101.4.1.1.0"),
            Some(("1.3.6.1.4.1.12356.101.4.1.1", 0))
        );
        assert_eq!(
            walk_column("1.3.6.1.2.1.47.1.1.1.1.10.22"),
            Some(("1.3.6.1.2.1.47.1.1.1.1.10", 22))
        );
        assert_eq!(walk_column("1"), None);
        assert_eq!(walk_column("1.3.x"), None);
    }

    #[test]
    fn an_unknown_device_asks_for_nothing_and_resolves_to_nothing() {
        let unknown = Some("1.3.6.1.4.1.99999.1.7");
        let descr = Some("Acme Widget Controller rev B");
        assert!(oids_to_read(unknown, descr).is_empty());
        assert_eq!(resolve(unknown, descr, &Answers::default()), None);
        assert_eq!(resolve(None, None, &Answers::default()), None);
    }

    /// A plain Linux host lands on the appliance row, asks for the two vendor objects, and — since
    /// neither answers — shows no version. What it must not do is show its kernel version, which
    /// `sysDescr` carries and which is not the OS version the row is named for (ADR-138 decision 6).
    #[test]
    fn a_plain_linux_host_asks_for_the_appliance_objects_and_resolves_to_nothing() {
        let net_snmp = Some("1.3.6.1.4.1.8072.3.2.10");
        let descr = Some("Linux web-01 5.15.0-91-generic #101-Ubuntu SMP x86_64");
        let reads = oids_to_read(net_snmp, descr);
        assert_eq!(
            reads.strings,
            vec!["1.3.6.1.4.1.2620.1.6.4.1.0", "1.3.6.1.4.1.6574.1.5.3.0"]
        );
        assert_eq!(resolve(net_snmp, descr, &Answers::default()), None);

        // …and a Synology, which answers its own object, does get one.
        let mut answers = Answers::default();
        answers.strings.insert(
            "1.3.6.1.4.1.6574.1.5.3.0".to_owned(),
            "DSM 7.2-64570".to_owned(),
        );
        assert_eq!(
            resolve(net_snmp, descr, &answers).as_deref(),
            Some("7.2-64570")
        );
    }

    #[test]
    fn every_pattern_compiles() {
        let broken: Vec<(&str, String)> = all_patterns()
            .filter_map(|p| Regex::new(p).err().map(|e| (p, e.to_string())))
            .collect();
        assert!(broken.is_empty(), "{broken:#?}");
        assert_eq!(
            PATTERNS.len(),
            all_patterns().collect::<BTreeSet<_>>().len()
        );
    }

    #[test]
    fn every_row_says_where_it_came_from_and_where_to_look() {
        for row in ROWS {
            assert!(!row.os.is_empty(), "a row with no OS name");
            assert!(
                !row.origin.is_empty(),
                "{:?} names no LibreNMS source",
                row.os
            );
            assert!(!row.when.is_empty(), "{:?} recognises nothing", row.os);
            assert!(!row.sources.is_empty(), "{:?} has nowhere to look", row.os);
            for m in row.when.iter().chain(row.unless) {
                if let Match::ObjectId(prefix) = m {
                    assert!(!prefix.starts_with('.'), "{:?}: {prefix}", row.os);
                }
            }
        }
        // Every instance OID a source names can be reached by the v2c walk.
        for row in ROWS {
            let reads = oids_for_row(row);
            for oid in reads.strings.iter().chain(&reads.integers) {
                assert!(walk_column(oid).is_some(), "{:?}: {oid}", row.os);
            }
        }
    }

    fn oids_for_row(row: &Row) -> Reads {
        let mut reads = Reads::default();
        for source in row.sources {
            match *source {
                Source::Str(oid) | Source::StrCut(oid, ..) => reads.strings.push(oid),
                Source::StrGated { oid, gate, .. } => {
                    reads.strings.push(oid);
                    reads.integers.push(gate);
                }
                Source::Joined(parts, _) => {
                    for part in parts {
                        match *part {
                            Part::Str(oid) => reads.strings.push(oid),
                            Part::Int(oid) => reads.integers.push(oid),
                        }
                    }
                }
                Source::Descr(..) | Source::DescrFirstLine(..) | Source::HuaweiVrp => {}
            }
        }
        reads
    }

    #[test]
    fn a_cisco_ios_row_1_is_used_only_when_it_is_the_chassis() {
        let descr = Some(
            "Cisco IOS Software, C2960X Software (C2960X-UNIVERSALK9-M), Version 15.0(2a)EX5, RELEASE SOFTWARE (fc3)",
        );
        let oid = Some("1.3.6.1.4.1.9.1.1208");
        let mut answers = Answers::default();
        answers.strings.insert(
            "1.3.6.1.2.1.47.1.1.1.1.10.1".to_owned(),
            "15.2(7)E9".to_owned(),
        );
        // No gate answer ⇒ sysDescr.
        assert_eq!(
            resolve(oid, descr, &answers).as_deref(),
            Some("15.0(2a)EX5")
        );
        // Row 1 is contained in something ⇒ not the chassis ⇒ sysDescr.
        answers
            .integers
            .insert("1.3.6.1.2.1.47.1.1.1.1.4.1".to_owned(), 1001);
        assert_eq!(
            resolve(oid, descr, &answers).as_deref(),
            Some("15.0(2a)EX5")
        );
        // Row 1 is the chassis ⇒ its software revision.
        answers
            .integers
            .insert("1.3.6.1.2.1.47.1.1.1.1.4.1".to_owned(), 0);
        assert_eq!(resolve(oid, descr, &answers).as_deref(), Some("15.2(7)E9"));
    }

    /// One LibreNMS recording, trimmed by the script that wrote `testdata/os_version_fixtures.json`
    /// to the values an identity probe could read: strings (types 4/4x/6) and integers
    /// (2/65/66/67/70), and of ENTITY-MIB's software-revision column only the rows a source names.
    struct Fixture {
        name: String,
        os: String,
        lab: bool,
        sys_object_id: Option<String>,
        sys_descr: Option<String>,
        strings: HashMap<String, String>,
        integers: HashMap<String, i64>,
        checked: bool,
        expected: Option<String>,
    }

    fn fixtures() -> Vec<Fixture> {
        let raw: serde_json::Value =
            serde_json::from_str(include_str!("../testdata/os_version_fixtures.json"))
                .expect("fixture JSON");
        raw.as_array()
            .expect("an array")
            .iter()
            .map(|f| {
                let text = |k: &str| f[k].as_str().map(str::to_owned);
                Fixture {
                    name: text("name").expect("name"),
                    os: text("os").expect("os"),
                    lab: f["lab"].as_bool().unwrap_or(false),
                    sys_object_id: text("sys_object_id"),
                    sys_descr: text("sys_descr"),
                    strings: f["strings"]
                        .as_object()
                        .expect("strings")
                        .iter()
                        .map(|(k, v)| (k.clone(), v.as_str().unwrap_or_default().to_owned()))
                        .collect(),
                    integers: f["integers"]
                        .as_object()
                        .expect("integers")
                        .iter()
                        .map(|(k, v)| (k.clone(), v.as_i64().expect("an integer")))
                        .collect(),
                    checked: f["checked"].as_bool().expect("checked"),
                    expected: text("expected"),
                }
            })
            .collect()
    }

    /// Recordings of devices the table deliberately does not cover (ADR-138 decision 6): they must
    /// resolve to nothing, not to something a broad row happened to find.
    const UNCOVERED: &[&str] = &["linux", "eaton-mgeups", "ricoh"];

    /// Where this table knowingly answers differently from LibreNMS, and why.
    fn deviation(f: &Fixture) -> Option<(Option<String>, &'static str)> {
        match f.os.as_str() {
            // `Vrp.php` appends `[<patch>]` from a GETNEXT on `hwPatchVersion`. On a device with
            // no patch table that GETNEXT returns the next object instead (`[unlocked]`), so the
            // suffix is not read at all (ADR-138 decision 5).
            "vrp" => f
                .expected
                .as_deref()
                .and_then(|e| e.split(" [").next())
                .map(|e| (Some(e.to_owned()), "the GETNEXT patch suffix is not copied")),
            // `dnos.yaml` lists `dellNetStackUnitCodeVersion.1` as a version source and these
            // recordings carry it, yet LibreNMS's expected output is the sysDescr version. Its
            // definition, not its unexplained output, is what was copied.
            "dnos" if matches!(f.name.as_str(), "dnos" | "dnos_s4048") => Some((
                f.strings.get("1.3.6.1.4.1.6027.3.26.1.3.4.1.10.1").cloned(),
                "the YAML's OID source is honoured over LibreNMS's recorded sysDescr answer",
            )),
            _ => None,
        }
    }

    #[test]
    fn every_librenms_fixture_resolves_to_the_version_librenms_derived() {
        let fixtures = fixtures();
        let mut failures = Vec::new();
        let mut checked = 0;
        for f in &fixtures {
            let oid = f.sys_object_id.as_deref();
            let descr = f.sys_descr.as_deref();

            // Detection: the fixture lands on the row for its own OS, or on none when uncovered.
            let row = row_for(oid, descr);
            let uncovered = UNCOVERED.contains(&f.os.as_str());
            let landed = row.map(|r| r.os);
            if !uncovered && !landed.is_some_and(|os| os.contains(&f.os.as_str())) {
                failures.push(format!("{} ({}): detected as {landed:?}", f.name, f.os));
                continue;
            }

            // The device answers only what it is asked, and only what it holds.
            let reads = oids_to_read(oid, descr);
            let answers = Answers {
                strings: reads
                    .strings
                    .iter()
                    .filter_map(|o| f.strings.get(*o).map(|v| ((*o).to_owned(), v.clone())))
                    .collect(),
                integers: reads
                    .integers
                    .iter()
                    .filter_map(|o| f.integers.get(*o).map(|v| ((*o).to_owned(), *v)))
                    .collect(),
            };
            let got = resolve(oid, descr, &answers);

            if uncovered {
                if got.is_some() {
                    failures.push(format!(
                        "{} ({}): uncovered but resolved {got:?}",
                        f.name, f.os
                    ));
                }
                continue;
            }
            if !f.checked {
                continue;
            }
            checked += 1;
            let want = match deviation(f) {
                Some((want, _why)) => want,
                None => f.expected.clone(),
            };
            let want = want.as_deref().and_then(sanitize);
            if got != want {
                failures.push(format!(
                    "{} ({}{}): got {got:?}, LibreNMS {:?}, want {want:?}",
                    f.name,
                    f.os,
                    if f.lab { ", LAB" } else { "" },
                    f.expected
                ));
            }
        }
        assert!(
            failures.is_empty(),
            "{} failures:\n{}",
            failures.len(),
            failures.join("\n")
        );
        // Floors, so a fixture file that stopped loading cannot pass by checking nothing.
        assert!(fixtures.len() >= 170, "{} fixtures", fixtures.len());
        assert!(checked >= 140, "{checked} checked");
    }

    /// Every row is exercised by at least one recording, and every lab device the verification box
    /// replays is in the file — those are the versions the deployment will be checked against.
    #[test]
    fn every_row_has_a_recording_and_the_lab_devices_are_all_there() {
        let fixtures = fixtures();
        let oses: BTreeSet<&str> = fixtures.iter().map(|f| f.os.as_str()).collect();
        for row in ROWS {
            assert!(
                row.os.iter().any(|os| oses.contains(os)),
                "{:?} has no recording",
                row.os
            );
        }
        assert!(
            fixtures.iter().filter(|f| f.lab).count() >= 21,
            "the lab's replayed recordings are missing from the fixture file"
        );
    }
}
