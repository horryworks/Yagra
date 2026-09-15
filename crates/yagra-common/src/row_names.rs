// SPDX-License-Identifier: AGPL-3.0-only
//! What a vendor table's rows are called, and how a threshold rule picks one (ADR-143).
//!
//! A table walk labels every value with a row key and nothing else (ADR-011), so a switch's memory
//! arrives as `cisco_mem_used{ifindex="1"}`, `{ifindex="2"}` and `{ifindex="20"}`. The operator
//! needs `Processor`, `I/O` and `Driver text` — measured on a Catalyst 2960S whose alert said 83.9%
//! while its Device-health card said 56%, because the two were reading different rows and neither
//! could say which.
//!
//! ## Why the name's source is decided by the OID and not stored on the collection item
//!
//! Where a row's name lives is a fact about the **MIB table**, not about the template that happens
//! to collect it: `ciscoMemoryPoolUsed` is in the same table as `ciscoMemoryPoolName` whichever
//! template asks for it. So the answer is one `const` table keyed by the table entry OID, and no
//! database column, template field or API field has to carry it. An item an operator adds gets a
//! name exactly when its table is on the list below.
//!
//! ## Why a folded row key still joins
//!
//! A multi-part instance index is folded to one `u32` by the transport (`fold_subids`). That hash
//! cannot be inverted, which ADR-046 decision 5 read as "the rows cannot be named". They can: the
//! name column is walked by **the same walk function**, so its rows fold to the same keys. Measured on
//! a Cisco ASA: `cempMemPoolName.2.1` folds to `227729484`, the key its `cempMemPoolUsed` series
//! carries in the TSDB.

/// `entPhysicalName` (ENTITY-MIB) — the name of a physical entity, indexed by `entPhysicalIndex`.
pub const ENT_PHYSICAL_NAME: &str = "1.3.6.1.2.1.47.1.1.1.1.7";

/// The longest row name kept, in characters. A device-supplied string, so it is bounded.
pub const ROW_NAME_MAX_CHARS: usize = 128;

/// The longest row-name pattern a threshold rule may carry, in characters.
pub const ROW_MATCH_MAX_CHARS: usize = 128;

/// The most row names one poll result carries, and the most core keeps from one.
///
/// Bounded because the bus message is, and because a device that reported thousands of non-zero
/// rows would be naming something this feature was not built for. The poller stops naming at this
/// many and core drops anything past it — one constant, so the two cannot disagree about where the
/// cap is (ADR-143 Inc.2).
pub const ROW_NAMES_MAX: usize = 512;

/// Where one table's row names come from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowNameSource {
    /// A string column whose rows carry **the same row key** as the values: a name column in the
    /// same table, or `entPhysicalName` for a table indexed by `entPhysicalIndex`.
    Column(&'static str),
    /// The table's rows point at another table's index through `pointer` (an integer column), and
    /// the name is `name` at that index. `cpmCPUTotalTable` is the one case: its rows are CPU numbers,
    /// and `cpmCPUTotalPhysicalIndex` says which entity each one is.
    Via {
        pointer: &'static str,
        name: &'static str,
    },
}

/// Table entry OID → where its rows' names are. One line per MIB table, with the `INDEX` clause that
/// makes the source correct as the reason.
///
/// ⚠️ **A table missing from here and from [`UNNAMED_TABLE_ENTRIES`] fails the build**
/// (`every_vendor_table_is_either_named_or_declared_unnamed`), so a new vendor template has to decide.
const ROW_NAME_TABLES: &[(&str, RowNameSource)] = &[
    // HOST-RESOURCES-MIB hrStorageEntry — INDEX { hrStorageIndex }; hrStorageDescr is column 3.
    (
        "1.3.6.1.2.1.25.2.3.1",
        RowNameSource::Column("1.3.6.1.2.1.25.2.3.1.3"),
    ),
    // hrProcessorEntry — INDEX { hrDeviceIndex }; the description is hrDeviceDescr at that index.
    (
        "1.3.6.1.2.1.25.3.3.1",
        RowNameSource::Column("1.3.6.1.2.1.25.3.2.1.3"),
    ),
    // ENTITY-SENSOR-MIB entPhySensorEntry — INDEX { entPhysicalIndex }.
    (
        "1.3.6.1.2.1.99.1.1.1",
        RowNameSource::Column(ENT_PHYSICAL_NAME),
    ),
    // CISCO-PROCESS-MIB cpmCPUTotalEntry — INDEX { cpmCPUTotalIndex }; column 2 points at the entity.
    (
        "1.3.6.1.4.1.9.9.109.1.1.1.1",
        RowNameSource::Via {
            pointer: "1.3.6.1.4.1.9.9.109.1.1.1.1.2",
            name: ENT_PHYSICAL_NAME,
        },
    ),
    // CISCO-MEMORY-POOL-MIB ciscoMemoryPoolEntry — INDEX { ciscoMemoryPoolType }; name is column 2.
    // Measured on a C2960S: Processor / I/O / Driver text.
    (
        "1.3.6.1.4.1.9.9.48.1.1.1",
        RowNameSource::Column("1.3.6.1.4.1.9.9.48.1.1.1.2"),
    ),
    // CISCO-ENHANCED-MEMPOOL-MIB cempMemPoolEntry — INDEX { entPhysicalIndex, cempMemPoolIndex };
    // name is column 3. Measured on an ASA: DP System memory / MEMPOOL_MSGLYR / MEMPOOL_GLOBAL_SHARED.
    (
        "1.3.6.1.4.1.9.9.221.1.1.1.1",
        RowNameSource::Column("1.3.6.1.4.1.9.9.221.1.1.1.1.3"),
    ),
    // CISCO-ENVMON-MIB temperature / fan / supply status — each INDEX { its own index }, Descr col 2.
    (
        "1.3.6.1.4.1.9.9.13.1.3.1",
        RowNameSource::Column("1.3.6.1.4.1.9.9.13.1.3.1.2"),
    ),
    (
        "1.3.6.1.4.1.9.9.13.1.4.1",
        RowNameSource::Column("1.3.6.1.4.1.9.9.13.1.4.1.2"),
    ),
    (
        "1.3.6.1.4.1.9.9.13.1.5.1",
        RowNameSource::Column("1.3.6.1.4.1.9.9.13.1.5.1.2"),
    ),
    // CISCO-ENTITY-FRU-CONTROL-MIB fan tray and FRU power — both INDEX { entPhysicalIndex }.
    (
        "1.3.6.1.4.1.9.9.117.1.4.1.1",
        RowNameSource::Column(ENT_PHYSICAL_NAME),
    ),
    (
        "1.3.6.1.4.1.9.9.117.1.1.2.1",
        RowNameSource::Column(ENT_PHYSICAL_NAME),
    ),
    // JUNIPER-MIB jnxOperatingEntry — INDEX { four contents indexes }; jnxOperatingDescr is column 5.
    (
        "1.3.6.1.4.1.2636.3.1.13.1",
        RowNameSource::Column("1.3.6.1.4.1.2636.3.1.13.1.5"),
    ),
    // HUAWEI-ENTITY-EXTENT-MIB hwEntityStateEntry — INDEX { entPhysicalIndex }.
    // Measured on an S5731: the four rows with a memory value are MPU Board 0..3.
    (
        "1.3.6.1.4.1.2011.5.25.31.1.1.1.1",
        RowNameSource::Column(ENT_PHYSICAL_NAME),
    ),
    // UCD-SNMP-MIB dskEntry — INDEX { dskIndex }; dskPath is column 2.
    (
        "1.3.6.1.4.1.2021.9.1",
        RowNameSource::Column("1.3.6.1.4.1.2021.9.1.2"),
    ),
    // CISCO-FIREWALL-MIB cfwConnectionStatEntry — INDEX { cfwConnectionStatService,
    // cfwConnectionStatType }; cfwConnectionStatDescription is column 3.
    (
        "1.3.6.1.4.1.9.9.147.1.2.2.2.1",
        RowNameSource::Column("1.3.6.1.4.1.9.9.147.1.2.2.2.1.3"),
    ),
];

/// Vendor tables whose rows have no name worth reading, each with the reason. The rows still alert
/// and still show — as `#<row>`.
///
/// Read only by the test that pins both lists to the catalogue: its job is to make "this table has
/// no names" a decision someone wrote down rather than a table nobody looked at.
#[cfg(test)]
const UNNAMED_TABLE_ENTRIES: &[(&str, &str)] = &[
    // BGP4-MIB bgpPeerEntry — INDEX { bgpPeerRemoteAddr }: the key is the peer address, folded.
    ("1.3.6.1.2.1.15.3.1", "the row is a peer address"),
    // UPS-MIB upsOutputEntry — INDEX { upsOutputLineIndex }: output lines are only numbered.
    ("1.3.6.1.2.1.33.1.4.4.1", "output lines are only numbered"),
    // Printer-MIB marker and alert tables — two-part indexes with no descriptive column.
    ("1.3.6.1.2.1.43.10.2.1", "markers are only numbered"),
    (
        "1.3.6.1.2.1.43.18.1.1",
        "an alert row is an event, not a part",
    ),
    // POWER-ETHERNET-MIB pethMainPseEntry — INDEX { pethMainPseGroupIndex }: groups are numbered.
    ("1.3.6.1.2.1.105.1.3.1.1", "PSE groups are only numbered"),
    // HUAWEI-MEMORY-MIB hwMemoryDevTable — no descriptive column; not implemented on the S5731
    // measured, so there was nothing to check a pointer against.
    ("1.3.6.1.4.1.2011.6.3.5.1.1", "no descriptive column found"),
    // HUAWEI-SECURITY-STAT-MIB session monitor table — one row per board's session statistics, and
    // no descriptive column was found to name the board by.
    (
        "1.3.6.1.4.1.2011.6.122.15.1.2.1",
        "no descriptive column found",
    ),
    // FORTINET-FORTIGATE-MIB fgVpnSslStatsEntry — INDEX { fgVdEntIndex }: one row per VDOM.
    (
        "1.3.6.1.4.1.12356.101.12.2.3.1",
        "one row per virtual domain",
    ),
];

/// The table entry a column OID belongs to: the column with its last sub-identifier removed.
fn entry_of(column_oid: &str) -> Option<&str> {
    let oid = column_oid.trim_start_matches('.');
    let (entry, column) = oid.rsplit_once('.')?;
    (!column.is_empty() && column.bytes().all(|b| b.is_ascii_digit())).then_some(entry)
}

/// Where the rows of the table a value column belongs to get their names, if anywhere.
///
/// Takes the column base OID as stored on a collection item (`1.3.6.1.4.1.9.9.48.1.1.1.5`).
#[must_use]
pub fn row_name_source(column_oid: &str) -> Option<RowNameSource> {
    let entry = entry_of(column_oid)?;
    ROW_NAME_TABLES
        .iter()
        .find(|(e, _)| *e == entry)
        .map(|(_, source)| *source)
}

/// A device-supplied row name made safe to store and show: control characters become spaces, runs
/// of whitespace collapse, and the result is trimmed and capped at [`ROW_NAME_MAX_CHARS`]. `None`
/// when nothing is left.
#[must_use]
pub fn sanitize_row_name(raw: &str) -> Option<String> {
    let cleaned: String = raw
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let joined = cleaned.split_whitespace().collect::<Vec<_>>().join(" ");
    let capped: String = joined.chars().take(ROW_NAME_MAX_CHARS).collect();
    let capped = capped.trim_end().to_owned();
    (!capped.is_empty()).then_some(capped)
}

/// Why a row-name pattern was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowMatchError {
    /// Longer than [`ROW_MATCH_MAX_CHARS`].
    TooLong,
    /// Contains a control character.
    ControlCharacter,
    /// Set on an interface rule, which already names exactly one port — a pattern there would be
    /// stored, listed, and never match anything.
    OnInterfaceRule,
}

/// A rule's row-name pattern in the form to store, for a rule at `level`: [`normalize_row_match`],
/// and refused on an interface rule.
///
/// One function for every edge a rule enters by — the API and the configuration bundle — so the two
/// cannot accept different patterns. The level half used to be written out by hand at each of those
/// edges, and only the normalization was shared (ADR-143 Inc.2).
///
/// # Errors
/// [`RowMatchError`] naming what was wrong.
pub fn row_match_for_level(
    level: crate::ScopeLevel,
    pattern: Option<&str>,
) -> Result<Option<String>, RowMatchError> {
    let pattern = normalize_row_match(pattern)?;
    if pattern.is_some() && level == crate::ScopeLevel::Interface {
        return Err(RowMatchError::OnInterfaceRule);
    }
    Ok(pattern)
}

/// A threshold rule's row-name pattern in the form to store: trimmed, and `None` when absent or
/// blank — a form that left the field empty means "every row", not "a row named nothing".
///
/// The half of [`row_match_for_level`] that does not depend on the level. Private since ADR-143
/// Inc.2: an edge that called this alone skipped the interface refusal, and both edges did.
fn normalize_row_match(pattern: Option<&str>) -> Result<Option<String>, RowMatchError> {
    let Some(trimmed) = pattern.map(str::trim).filter(|p| !p.is_empty()) else {
        return Ok(None);
    };
    if trimmed.chars().count() > ROW_MATCH_MAX_CHARS {
        return Err(RowMatchError::TooLong);
    }
    if trimmed.chars().any(char::is_control) {
        return Err(RowMatchError::ControlCharacter);
    }
    Ok(Some(trimmed.to_owned()))
}

/// Whether a row named `name` matches a rule's `pattern`.
///
/// Case-insensitive, and `*` stands for any run of characters (including none). Nothing else is
/// special — a row name is device text, and `I/O` or `MPU Board 0` must not be read as syntax.
///
/// Asked once per rule per row per evaluation, so the common case allocates nothing: when both
/// strings are ASCII — every name measured on real devices is — the bytes are compared with ASCII
/// case folding, which gives exactly what lowercasing both would. Anything else takes the Unicode
/// path, whose lowercasing can change a string's length and so cannot be done a byte at a time.
#[must_use]
pub fn row_name_matches(pattern: &str, name: &str) -> bool {
    if pattern.is_ascii() && name.is_ascii() {
        return wildcard_match(
            pattern.as_bytes(),
            name.as_bytes(),
            &b'*',
            u8::eq_ignore_ascii_case,
        );
    }
    let p: Vec<char> = pattern.to_lowercase().chars().collect();
    let n: Vec<char> = name.to_lowercase().chars().collect();
    wildcard_match(&p, &n, &'*', |a, b| a == b)
}

/// `pattern` against `name`, where `star` stands for any run of units (including none) and `same`
/// says whether two other units are equal. One walk for both spellings of [`row_name_matches`], so
/// the fast path cannot come to disagree with the slow one about what a `*` does.
fn wildcard_match<T: PartialEq>(
    pattern: &[T],
    name: &[T],
    star: &T,
    same: impl Fn(&T, &T) -> bool,
) -> bool {
    let (mut pi, mut ni) = (0usize, 0usize);
    // The pattern position just after the last `*` seen, and the name position it was tried from.
    let mut backtrack: Option<(usize, usize)> = None;
    while ni < name.len() {
        if pi < pattern.len() && pattern[pi] == *star {
            backtrack = Some((pi + 1, ni));
            pi += 1;
        } else if pi < pattern.len() && same(&pattern[pi], &name[ni]) {
            pi += 1;
            ni += 1;
        } else if let Some((after_star, from)) = backtrack {
            // Let the last `*` swallow one more unit and try again.
            pi = after_star;
            ni = from + 1;
            backtrack = Some((after_star, from + 1));
        } else {
            return false;
        }
    }
    pattern[pi..].iter().all(|c| c == star)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::collection::{builtin_catalog, builtin_templates, item_publishes_per_interface};
    use crate::CollectionKind;

    /// The three tables measured on real devices resolve to the columns those devices answered on.
    #[test]
    fn the_measured_tables_resolve_to_the_columns_the_devices_answered() {
        // C2960S: ciscoMemoryPoolUsed / Free → ciscoMemoryPoolName.
        for col in ["1.3.6.1.4.1.9.9.48.1.1.1.5", "1.3.6.1.4.1.9.9.48.1.1.1.6"] {
            assert_eq!(
                row_name_source(col),
                Some(RowNameSource::Column("1.3.6.1.4.1.9.9.48.1.1.1.2"))
            );
        }
        // ASA: cempMemPoolUsed / Free → cempMemPoolName.
        for col in [
            "1.3.6.1.4.1.9.9.221.1.1.1.1.18",
            "1.3.6.1.4.1.9.9.221.1.1.1.1.20",
        ] {
            assert_eq!(
                row_name_source(col),
                Some(RowNameSource::Column("1.3.6.1.4.1.9.9.221.1.1.1.1.3"))
            );
        }
        // S5731: hwEntityCpuUsage / MemUsage / Temperature → entPhysicalName.
        for col in [
            "1.3.6.1.4.1.2011.5.25.31.1.1.1.1.5",
            "1.3.6.1.4.1.2011.5.25.31.1.1.1.1.7",
            "1.3.6.1.4.1.2011.5.25.31.1.1.1.1.11",
        ] {
            assert_eq!(
                row_name_source(col),
                Some(RowNameSource::Column(ENT_PHYSICAL_NAME))
            );
        }
        // The one pointer table.
        assert_eq!(
            row_name_source("1.3.6.1.4.1.9.9.109.1.1.1.1.8"),
            Some(RowNameSource::Via {
                pointer: "1.3.6.1.4.1.9.9.109.1.1.1.1.2",
                name: ENT_PHYSICAL_NAME,
            })
        );
    }

    /// An interface column, a table nobody listed, a scalar and a malformed OID have no source.
    #[test]
    fn anything_not_on_the_list_has_no_source() {
        assert_eq!(row_name_source("1.3.6.1.2.1.31.1.1.1.6"), None);
        assert_eq!(row_name_source("1.3.6.1.4.1.99999.1.2.3"), None);
        assert_eq!(
            row_name_source("1.3.6.1.4.1.9.9.48.1.1.1"),
            None,
            "the entry itself"
        );
        assert_eq!(row_name_source("1.3.6.1.4.1.9.9.48.1.1.1.x"), None);
        assert_eq!(row_name_source(""), None);
        // A leading dot is the same OID.
        assert!(row_name_source(".1.3.6.1.4.1.9.9.48.1.1.1.5").is_some());
    }

    /// 🚨 Both directions against the catalogue: every non-interface table the product ships is
    /// decided, and nothing on either list is dead.
    #[test]
    fn every_vendor_table_is_either_named_or_declared_unnamed() {
        let mut entries: Vec<String> = Vec::new();
        let items = builtin_templates()
            .into_iter()
            .flat_map(|t| t.items)
            .chain(builtin_catalog());
        for item in items {
            if item.kind != CollectionKind::Table || item_publishes_per_interface(&item) {
                continue;
            }
            let entry = entry_of(&item.oid)
                .unwrap_or_else(|| panic!("{} has a malformed column OID", item.metric_name))
                .to_owned();
            if !entries.contains(&entry) {
                entries.push(entry);
            }
        }
        assert!(
            entries.len() >= 15,
            "only {} vendor tables were found in the catalogue; the check below ran over almost nothing",
            entries.len()
        );
        // Every undecided table in one message, so a new template is fixed in one pass.
        let undecided: Vec<String> = entries
            .iter()
            .filter_map(|entry| {
                let named = ROW_NAME_TABLES.iter().any(|(e, _)| e == entry);
                let unnamed = UNNAMED_TABLE_ENTRIES.iter().any(|(e, _)| e == entry);
                (named == unnamed).then(|| format!("{entry} (named={named}, unnamed={unnamed})"))
            })
            .collect();
        assert!(
            undecided.is_empty(),
            "every vendor table must be on exactly one of ROW_NAME_TABLES and UNNAMED_TABLE_ENTRIES \
             — say where its rows' names are, or why they have none (ADR-143):\n  {}",
            undecided.join("\n  ")
        );
        for (entry, _) in ROW_NAME_TABLES {
            assert!(
                entries.iter().any(|e| e == entry),
                "ROW_NAME_TABLES names {entry}, which no built-in item collects"
            );
        }
        for (entry, _) in UNNAMED_TABLE_ENTRIES {
            assert!(
                entries.iter().any(|e| e == entry),
                "UNNAMED_TABLE_ENTRIES names {entry}, which no built-in item collects"
            );
        }
    }

    #[test]
    fn a_device_name_is_cleaned_capped_and_refused_when_empty() {
        assert_eq!(sanitize_row_name("  I/O  ").as_deref(), Some("I/O"));
        assert_eq!(
            sanitize_row_name("MPU\tBoard\n 0").as_deref(),
            Some("MPU Board 0")
        );
        assert_eq!(sanitize_row_name(" \u{0} \t"), None);
        let long = "x".repeat(ROW_NAME_MAX_CHARS + 40);
        assert_eq!(
            sanitize_row_name(&long).map(|s| s.chars().count()),
            Some(ROW_NAME_MAX_CHARS)
        );
    }

    #[test]
    fn a_pattern_is_trimmed_blank_means_every_row_and_long_or_controlled_is_refused() {
        assert_eq!(
            normalize_row_match(Some("  I/O ")),
            Ok(Some("I/O".to_owned()))
        );
        assert_eq!(normalize_row_match(Some("   ")), Ok(None));
        assert_eq!(normalize_row_match(None), Ok(None));
        assert_eq!(
            normalize_row_match(Some(&"a".repeat(ROW_MATCH_MAX_CHARS + 1))),
            Err(RowMatchError::TooLong)
        );
        assert_eq!(
            normalize_row_match(Some("I/\u{7}O")),
            Err(RowMatchError::ControlCharacter)
        );
    }

    /// ADR-143 Inc.2: the level half of the check, which the API and the bundle importer each wrote
    /// out by hand. The accepting cases first — a check that refused everything would pass the rest.
    #[test]
    fn a_pattern_is_refused_only_on_an_interface_rule() {
        use crate::ScopeLevel;
        assert_eq!(
            row_match_for_level(ScopeLevel::Node, Some(" I/O ")),
            Ok(Some("I/O".to_owned()))
        );
        assert_eq!(
            row_match_for_level(ScopeLevel::Global, Some("MPU Board *")),
            Ok(Some("MPU Board *".to_owned()))
        );
        assert_eq!(
            row_match_for_level(ScopeLevel::Interface, None),
            Ok(None),
            "an interface rule with no pattern is an ordinary interface rule"
        );
        assert_eq!(
            row_match_for_level(ScopeLevel::Interface, Some("   ")),
            Ok(None),
            "blank is no pattern, on any level"
        );
        assert_eq!(
            row_match_for_level(ScopeLevel::Interface, Some("I/O")),
            Err(RowMatchError::OnInterfaceRule)
        );
        // The normalization runs first, so its refusals reach every level.
        assert_eq!(
            row_match_for_level(ScopeLevel::Node, Some(&"a".repeat(ROW_MATCH_MAX_CHARS + 1))),
            Err(RowMatchError::TooLong)
        );
        assert_eq!(
            row_match_for_level(ScopeLevel::Node, Some("I/\u{7}O")),
            Err(RowMatchError::ControlCharacter)
        );
    }

    /// ADR-143 Inc.2: the allocation-free ASCII path answers exactly as the implementation it
    /// replaced, which lowercased both strings on every call. The old one is kept here, verbatim,
    /// as the oracle — sharing the walk with the code under test would make the check agree with
    /// itself.
    #[test]
    fn the_ascii_fast_path_answers_as_lowercasing_both_strings_did() {
        fn before_inc2(pattern: &str, name: &str) -> bool {
            let p: Vec<char> = pattern.to_lowercase().chars().collect();
            let n: Vec<char> = name.to_lowercase().chars().collect();
            let (mut pi, mut ni) = (0usize, 0usize);
            let mut backtrack: Option<(usize, usize)> = None;
            while ni < n.len() {
                if pi < p.len() && p[pi] == '*' {
                    backtrack = Some((pi + 1, ni));
                    pi += 1;
                } else if pi < p.len() && p[pi] == n[ni] {
                    pi += 1;
                    ni += 1;
                } else if let Some((after_star, from)) = backtrack {
                    pi = after_star;
                    ni = from + 1;
                    backtrack = Some((after_star, from + 1));
                } else {
                    return false;
                }
            }
            p[pi..].iter().all(|c| *c == '*')
        }
        let patterns = [
            "I/O",
            "i/o",
            "MPU Board *",
            "*shared",
            "*",
            "",
            "a*b*c",
            "I?O",
            "x",
            "*o",
            "PROC*SOR",
            "**",
            "*İ*",
        ];
        let names = [
            "I/O",
            "Processor",
            "MPU Board 3",
            "LPU Board 3",
            "MEMPOOL_GLOBAL_SHARED",
            "",
            "a--b--b--c",
            "a--b--",
            "I?O",
            "Driver text",
            "İ/O",
            "STRASSE",
            "straße",
        ];
        let mut compared = 0;
        let mut matched = 0;
        for p in patterns {
            for n in names {
                let got = row_name_matches(p, n);
                assert_eq!(got, before_inc2(p, n), "pattern {p:?} against name {n:?}");
                compared += 1;
                matched += usize::from(got);
            }
        }
        assert_eq!(compared, patterns.len() * names.len());
        assert!(
            matched > 10 && matched < compared - 10,
            "the table must hold both answers in quantity, or agreeing proves little ({matched} of {compared} matched)"
        );
    }

    /// The accepting side first: an exact name, any case, and the wildcard in each position.
    #[test]
    fn a_pattern_matches_the_names_it_should() {
        assert!(row_name_matches("I/O", "I/O"));
        assert!(row_name_matches("i/o", "I/O"));
        assert!(row_name_matches("MPU Board *", "MPU Board 3"));
        assert!(row_name_matches("*shared", "MEMPOOL_GLOBAL_SHARED"));
        assert!(row_name_matches("*", "anything"));
        assert!(row_name_matches("*", ""));
        assert!(row_name_matches("a*b*c", "a--b--b--c"));
    }

    #[test]
    fn a_pattern_refuses_the_names_it_should() {
        assert!(!row_name_matches("I/O", "Processor"));
        assert!(
            !row_name_matches("I/O", "I/O extra"),
            "no implicit trailing wildcard"
        );
        assert!(!row_name_matches("MPU Board *", "LPU Board 3"));
        assert!(!row_name_matches("a*b*c", "a--b--"));
        assert!(!row_name_matches("x", ""));
        // Only `*` is special: `?` and `.` are characters a device name really contains.
        assert!(!row_name_matches("I?O", "I/O"));
        assert!(row_name_matches("I?O", "I?O"));
    }
}
