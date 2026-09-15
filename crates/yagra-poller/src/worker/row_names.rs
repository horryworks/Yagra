// SPDX-License-Identifier: AGPL-3.0-only
//! The row-name walk: what a vendor table's rows are called, read after a table job (ADR-143).
//!
//! A table job's samples carry a row key and nothing else — `cisco_mem_used{ifindex="2"}`. This walk
//! reads the name that goes with each key (`I/O`), so an alert and the Device-health list can say
//! which memory pool, CPU or sensor they are about. **Where** each table keeps its names is decided
//! in one place, `yagra_common::row_names`; this file only walks what that table says.
//!
//! Three things keep it cheap, and each is a decision rather than an optimisation:
//!
//! - **Hourly per node**, decided in `stream.rs` with the same bookkeeping the identity probe uses —
//!   names change when hardware does, not per poll.
//! - **Only the rows whose value on this poll was not zero.** A Huawei S5731 reports 306 entity rows
//!   of which 302 are ports and fans with no memory at all; asking for all of them would store 306
//!   names to show four.
//! - **After the table job, inside its permit.** The device is already being talked to, so the walk
//!   costs round trips on a conversation that exists rather than a slot of its own.
//!
//! 🚨 **The row key comes from the same walk function as the values**, which is what lets a folded
//! multi-part index (a Cisco `cempMemPoolTable` row `2.1` → `227729484`) join its name at all.
//! Reading the names through `walk_instances` instead would hand back the unfolded index, and every
//! name would miss.

use super::*;
use yagra_bus::RowName;
use yagra_common::row_names::{row_name_source, sanitize_row_name, RowNameSource};

/// How long the name walk may run, whatever the job's interval. It is extra work on a permit the
/// table job already held, so it is bounded on its own rather than borrowing the job's budget.
const NAME_WALK_BUDGET: Duration = Duration::from_secs(20);

/// The most names one result carries — the same cap core applies on receipt, kept in one place.
pub(super) use yagra_common::row_names::ROW_NAMES_MAX;

/// Whether a job of this kind can carry the row-name walk. Only a table walk produces row keys, and
/// `stream.rs` asks this rather than naming the kinds itself (`guards.rs`).
pub(super) fn carries_row_names(check: &CheckSpec) -> bool {
    matches!(check, CheckSpec::SnmpTable(_) | CheckSpec::SnmpV3Table(_))
}

/// Which rows this result needs names for, and where each name is.
#[derive(Debug, Default, PartialEq)]
pub(super) struct NamePlan {
    wanted: Vec<(String, u32, RowNameSource)>,
}

impl NamePlan {
    /// Every (metric, row) on a column whose table has a name source and whose value on this poll was
    /// not zero, capped at [`ROW_NAMES_MAX`].
    pub(super) fn from(columns: &[SnmpColumn], samples: &[Sample]) -> Self {
        let sources: HashMap<&str, RowNameSource> = columns
            .iter()
            .filter_map(|c| row_name_source(&c.oid).map(|s| (c.metric_name.as_str(), s)))
            .collect();
        let mut seen: HashSet<(&str, u32)> = HashSet::new();
        let mut wanted = Vec::new();
        for sample in samples {
            let (Some(source), Some(row)) = (sources.get(sample.metric.as_str()), sample.ifindex)
            else {
                continue;
            };
            // Not zero, and a real number: a zero row is an entity that has none of this (a port
            // has no memory), and naming it would put 302 dead rows in front of four real ones.
            if sample.value == 0.0 || !sample.value.is_finite() {
                continue;
            }
            if !seen.insert((sample.metric.as_str(), row.0)) {
                continue;
            }
            if wanted.len() >= ROW_NAMES_MAX {
                break;
            }
            wanted.push((sample.metric.clone(), row.0, *source));
        }
        Self { wanted }
    }

    fn is_empty(&self) -> bool {
        self.wanted.is_empty()
    }

    /// The string columns to walk, each once.
    fn string_columns(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for (_, _, source) in &self.wanted {
            let column = match source {
                RowNameSource::Column(column) => *column,
                RowNameSource::Via { name, .. } => *name,
            };
            if !out.iter().any(|c| c == column) {
                out.push(column.to_owned());
            }
        }
        out
    }

    /// The integer pointer columns to walk first, each once.
    fn pointer_columns(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for (_, _, source) in &self.wanted {
            match source {
                RowNameSource::Via { pointer, .. } => {
                    if !out.iter().any(|c| c == pointer) {
                        out.push((*pointer).to_owned());
                    }
                }
                RowNameSource::Column(_) => {}
            }
        }
        out
    }

    /// Join what the walks returned onto the rows that wanted a name. A row whose name did not come
    /// back — the column is not implemented, or the walk stopped first — is simply left out.
    pub(super) fn assemble(
        &self,
        strings: &[SnmpTableString],
        pointers: &[SnmpTableSample],
    ) -> Vec<RowName> {
        let text: HashMap<(&str, u32), &str> = strings
            .iter()
            .map(|r| ((r.oid_base.as_str(), r.ifindex), r.value.as_str()))
            .collect();
        let pointed: HashMap<(&str, u32), u32> = pointers
            .iter()
            .filter(|r| r.value.is_finite() && r.value >= 0.0 && r.value <= f64::from(u32::MAX))
            .map(|r| ((r.oid_base.as_str(), r.ifindex), r.value as u32))
            .collect();
        self.wanted
            .iter()
            .filter_map(|(metric, row, source)| {
                let raw = match source {
                    RowNameSource::Column(column) => text.get(&(*column, *row))?,
                    RowNameSource::Via { pointer, name } => {
                        let at = pointed.get(&(*pointer, *row))?;
                        text.get(&(*name, *at))?
                    }
                };
                Some(RowName {
                    metric: metric.clone(),
                    row: *row,
                    name: sanitize_row_name(raw)?,
                })
            })
            .collect()
    }
}

/// Read the names for this table job's rows and put them on its result.
///
/// Returns whether the device answered the walk — `true` also when there was nothing to name, so a
/// node whose tables have no names is asked again in an hour rather than every few minutes.
pub(super) async fn collect(
    job: &PollJob,
    transport: &dyn Transport,
    result: &mut PollResult,
) -> bool {
    let (walker, columns, timeout_ms) = if let CheckSpec::SnmpTable(t) = &job.check {
        (
            SnmpWalker::V2c(t.community.clone()),
            &t.columns,
            t.timeout_ms,
        )
    } else if let CheckSpec::SnmpV3Table(t) = &job.check {
        (SnmpWalker::V3(t.auth.clone()), &t.columns, t.timeout_ms)
    } else {
        return false;
    };
    let plan = NamePlan::from(columns, &result.samples);
    if plan.is_empty() {
        return true;
    }
    let timeout = Duration::from_millis(u64::from(timeout_ms));
    let deadline = Instant::now() + NAME_WALK_BUDGET;

    let mut pointers = Vec::new();
    let pointer_columns = plan.pointer_columns();
    if !pointer_columns.is_empty() {
        match walker
            .walk(
                transport,
                job.target,
                &pointer_columns,
                WalkLimits::until(timeout, deadline),
            )
            .await
        {
            Ok(walk) => pointers = walk.rows,
            Err(err) => {
                tracing::debug!(job_id = %job.job_id, error = %err, "row-name pointer walk failed")
            }
        }
    }
    let strings = match walker
        .walk_strings(
            transport,
            job.target,
            &plan.string_columns(),
            WalkLimits::until(timeout, deadline),
        )
        .await
    {
        Ok(walk) => walk.rows,
        Err(err) => {
            tracing::debug!(job_id = %job.job_id, error = %err, "row-name walk failed");
            return false;
        }
    };
    result.row_names = plan.assemble(&strings, &pointers);
    metrics::counter!("yagra_poll_row_names_total").increment(result.row_names.len() as u64);
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;
    use uuid::Uuid;
    use yagra_bus::TraceContext;
    use yagra_transport::FakeTransport;

    const POOL_USED: &str = "1.3.6.1.4.1.9.9.48.1.1.1.5";
    const POOL_NAME: &str = "1.3.6.1.4.1.9.9.48.1.1.1.2";
    const HW_MEM: &str = "1.3.6.1.4.1.2011.5.25.31.1.1.1.1.7";
    const CPU_5MIN: &str = "1.3.6.1.4.1.9.9.109.1.1.1.1.8";
    const CPU_PHYS: &str = "1.3.6.1.4.1.9.9.109.1.1.1.1.2";
    const ENT_NAME: &str = yagra_common::row_names::ENT_PHYSICAL_NAME;

    fn column(metric: &str, oid: &str) -> SnmpColumn {
        SnmpColumn {
            metric_name: metric.into(),
            oid: oid.into(),
            kind: MetricKind::Gauge,
        }
    }

    fn row(metric: &str, row: u32, value: f64) -> Sample {
        Sample::interface(metric, IfIndex(row), value, MetricKind::Gauge)
    }

    fn text(oid: &str, row: u32, value: &str) -> SnmpTableString {
        SnmpTableString {
            oid_base: oid.into(),
            ifindex: row,
            value: value.into(),
        }
    }

    fn table_job(columns: Vec<SnmpColumn>) -> PollJob {
        PollJob {
            job_id: Uuid::nil(),
            node_id: NodeId::from(Uuid::nil()),
            target: IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1)),
            check: CheckSpec::SnmpTable(SnmpTableCheck {
                community: "public".into(),
                columns,
                meta_columns: Vec::new(),
                timeout_ms: 2000,
            }),
            interval_secs: 300,
            credential_ref: None,
            probe_identity: false,
            on_demand: false,
            trace_context: TraceContext::new(),
        }
    }

    fn names(r: &PollResult) -> Vec<(String, u32, String)> {
        r.row_names
            .iter()
            .map(|n| (n.metric.clone(), n.row, n.name.clone()))
            .collect()
    }

    /// The C2960S this ADR started from: three pools, and the one at 40 bytes is still named while a
    /// port-table column on the same job is not asked about at all.
    #[tokio::test]
    async fn a_cisco_pool_table_gets_its_names_from_the_same_table() {
        let job = table_job(vec![
            column("cisco_mem_used", POOL_USED),
            column("if_hc_in_octets", "1.3.6.1.2.1.31.1.1.1.6"),
        ]);
        let transport = FakeTransport::reachable(1.0).with_snmp_table_strings(vec![
            text(POOL_NAME, 1, "Processor"),
            text(POOL_NAME, 2, "I/O"),
            text(POOL_NAME, 20, "Driver text"),
        ]);
        let mut result = result(
            &job,
            0,
            CheckOutcome::Reachable,
            vec![
                row("cisco_mem_used", 1, 37_548_112.0),
                row("cisco_mem_used", 2, 12_312_508.0),
                row("cisco_mem_used", 20, 40.0),
                row("if_hc_in_octets", 1, 99.0),
            ],
        );
        assert!(collect(&job, &transport, &mut result).await);
        assert_eq!(
            names(&result),
            vec![
                ("cisco_mem_used".into(), 1, "Processor".into()),
                ("cisco_mem_used".into(), 2, "I/O".into()),
                ("cisco_mem_used".into(), 20, "Driver text".into()),
            ]
        );
    }

    /// The S5731: entity rows with no value are not named, and the name comes from ENTITY-MIB.
    #[test]
    fn a_zero_row_is_not_named() {
        let columns = vec![column("huawei_mem_usage", HW_MEM)];
        let plan = NamePlan::from(
            &columns,
            &[
                row("huawei_mem_usage", 67_108_873, 33.0),
                row("huawei_mem_usage", 67_108_874, 0.0),
                row("huawei_mem_usage", 67_108_875, f64::NAN),
            ],
        );
        assert_eq!(plan.string_columns(), vec![ENT_NAME.to_owned()]);
        assert!(plan.pointer_columns().is_empty());
        let out = plan.assemble(
            &[
                text(ENT_NAME, 67_108_873, "MPU Board 0"),
                text(ENT_NAME, 67_108_874, "GigabitEthernet0/0/1"),
            ],
            &[],
        );
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].name, "MPU Board 0");
    }

    /// The pointer shape: a CPU row names the entity it points at.
    #[test]
    fn a_cpu_row_is_named_through_its_physical_index() {
        let columns = vec![column("cisco_cpu_5min", CPU_5MIN)];
        let plan = NamePlan::from(&columns, &[row("cisco_cpu_5min", 7, 12.0)]);
        assert_eq!(plan.pointer_columns(), vec![CPU_PHYS.to_owned()]);
        let out = plan.assemble(
            &[
                text(ENT_NAME, 1000, "CPU of Switch 1"),
                text(ENT_NAME, 7, "wrong"),
            ],
            &[SnmpTableSample {
                oid_base: CPU_PHYS.into(),
                ifindex: 7,
                value: 1000.0,
            }],
        );
        assert_eq!(out.len(), 1);
        assert_eq!(
            out[0].name, "CPU of Switch 1",
            "the pointer decides, not the row key"
        );
    }

    /// A name that did not come back, or cleans to nothing, is left out rather than stored empty.
    #[test]
    fn a_missing_or_empty_name_is_left_out() {
        let columns = vec![column("cisco_mem_used", POOL_USED)];
        let plan = NamePlan::from(
            &columns,
            &[row("cisco_mem_used", 1, 5.0), row("cisco_mem_used", 2, 5.0)],
        );
        let out = plan.assemble(&[text(POOL_NAME, 1, " \t ")], &[]);
        assert!(out.is_empty());
    }

    /// Nothing to name is still an answer: the walk is not issued and the node waits the period.
    #[tokio::test]
    async fn a_table_with_nothing_to_name_issues_no_walk() {
        let job = table_job(vec![column("if_hc_in_octets", "1.3.6.1.2.1.31.1.1.1.6")]);
        let transport = FakeTransport::reachable(1.0);
        let mut result = result(
            &job,
            0,
            CheckOutcome::Reachable,
            vec![row("if_hc_in_octets", 1, 99.0)],
        );
        assert!(collect(&job, &transport, &mut result).await);
        assert!(result.row_names.is_empty());
        assert!(
            transport.asked().is_empty(),
            "no walk should have been issued for a job with no named table"
        );
    }

    #[test]
    fn only_a_table_walk_carries_row_names() {
        assert!(carries_row_names(&table_job(Vec::new()).check));
        assert!(!carries_row_names(&testkit::icmp_job().check));
    }
}
