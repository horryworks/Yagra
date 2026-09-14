// SPDX-License-Identifier: AGPL-3.0-only
//! SNMP v2c GET + GETBULK table walk over a pure-Rust client (`csnmp`) — ADR-021 PoC.
//!
//! This validates the pure-Rust path before any net-snmp FFI fallback. v3 (auth/priv)
//! is still pending. Values are returned **raw** (counters included) — rates are derived
//! at query time (ADR-012).
//!
//! Every column is paged by one function, [`walk_column_v2c`], over the [`BulkPager`] seam
//! (ADR-110 Increment 9). The seam is what lets the paging — retries, rows kept past a failure, the
//! base GET on an empty column — run against a scripted agent in the tests below; the three
//! multi-column loops around it still need a device and are covered only by reading their source.

use crate::walk_budget::{
    conclude, is_silence, not_asked, note_retry, note_truncation, ColumnEnd, ColumnOutcome,
    ColumnReport, ColumnStop, RetryAllowance, TableWalk, Truncation, WalkBudget, WalkLimits,
};
use crate::{
    SnmpInstanceRow, SnmpSample, SnmpTableSample, SnmpTableString, SnmpValue, TransportError,
};
use async_trait::async_trait;
use csnmp::message::BindingValue;
use csnmp::{ObjectIdentifier, ObjectValue, Snmp2cClient, SnmpClientError};
use std::net::{IpAddr, SocketAddr};
use std::time::Duration;

/// Standard SNMP agent port.
const SNMP_PORT: u16 = 161;

/// Did the agent say **anything**, or nothing at all?
///
/// 🚨 **The whole of [`WalkBudget`]'s consecutive-failure rule depends on this distinction**, and
/// getting it backwards would truncate healthy devices. Most `csnmp` errors mean bytes came back and
/// we did not like them — the commonest by far being `FailedBinding { NoSuchObject }`, which is how
/// a scalar GET reports *"I do not implement that OID"*. That is an answer. Only the five variants
/// below mean the conversation did not happen.
///
/// ⚠️ A column walk reaches "unimplemented" without coming here: [`BulkPager::get_base`] turns
/// `noSuchObject` / `noSuchInstance` on the column base into [`BaseAnswer::NoSuch`], the way
/// `csnmp::walk_bulk` swallowed it. What does arrive here from a column walk is a page or a base
/// GET that errored, and the answer decides whether [`RetryAllowance`] may send it again.
fn outcome_of(err: &SnmpClientError) -> ColumnOutcome {
    match err {
        SnmpClientError::TimedOut
        | SnmpClientError::Sending { .. }
        | SnmpClientError::Receiving { .. }
        | SnmpClientError::CreatingSocket { .. }
        | SnmpClientError::Connecting { .. } => ColumnOutcome::Failed,
        // A wildcard on purpose, and it falls the safe way: this is a foreign enum, so a variant
        // added upstream lands here and is read as "the agent answered" — which never truncates a
        // walk. Enumerating the other nine would turn an upstream release into a build failure and
        // buy nothing, because the interesting split is exactly the five above.
        _ => ColumnOutcome::Answered,
    }
}

/// GETBULK max-repetitions per request. Bounded so a huge table is paged, not pulled in
/// one oversized PDU; [`walk_column_v2c`] repeats until the column is exhausted.
const WALK_MAX_REPETITIONS: u32 = 20;

/// Fetch `oids` from `target` via SNMP v2c. Per-OID failures are logged and skipped so a
/// single bad OID doesn't fail the whole poll — bounded by a [`WalkBudget`], so a device that
/// answers nothing costs two round trips rather than one per OID (ADR-110 Increment 3).
pub async fn snmp_get_v2c(
    target: IpAddr,
    community: &str,
    oids: &[String],
    timeout: Duration,
) -> Result<Vec<SnmpSample>, TransportError> {
    let client = connect(target, community, timeout).await?;

    let mut samples = Vec::with_capacity(oids.len());
    let mut budget = WalkBudget::new(timeout);
    for (asked, oid_str) in oids.iter().enumerate() {
        if let Some(reason) = budget.spent() {
            note_truncation(reason, target, oids.len() - asked);
            break;
        }
        let oid = match parse_oid(oid_str) {
            Some(o) => o,
            None => {
                tracing::warn!(%oid_str, "skipping malformed OID");
                budget.record(ColumnOutcome::Skipped);
                continue;
            }
        };
        match client.get(oid).await {
            Ok(value) => {
                if let Some(v) = numeric(&value) {
                    samples.push(SnmpSample {
                        oid: oid_str.clone(),
                        value: v,
                    });
                }
                budget.record(ColumnOutcome::Answered);
            }
            Err(e) => {
                tracing::debug!(%oid_str, error = %e, "snmp get failed");
                budget.record(outcome_of(&e));
            }
        }
    }
    Ok(samples)
}

/// Walk numeric table columns from `target` via GETBULK. Each column base yields one row per
/// instance: a single trailing sub-identifier is the row key directly, while a multi-part index
/// is folded to a synthetic key (see [`ifindex_of`]) so multi-index tables — vendor memory
/// (HUAWEI-MEMORY-MIB `hwMemoryDevTable`), BGP4-MIB peers, … — are collected too. A column that
/// fails ends there and keeps the rows it had already paged (ADR-110 Increment 9). Counters are
/// returned **raw** (rates derived at query time, ADR-012).
pub async fn snmp_walk_v2c(
    target: IpAddr,
    community: &str,
    column_oids: &[String],
    limits: WalkLimits,
) -> Result<TableWalk<SnmpTableSample>, TransportError> {
    let client = connect(target, community, limits.timeout).await?;
    let mut budget = WalkBudget::within(limits);
    // Why the walk stopped is returned rather than dropped. `snmp_walk_instances_v2c` keeps it to
    // spare a caller a second walk at a silent device; this one keeps it for the opposite reason —
    // the caller is the interface table walk, and a `Deadline` there means the node's configured
    // metric columns were never asked for at all (ADR-110 Increment 6).
    Ok(walk_columns(
        &client,
        target,
        column_oids,
        &mut budget,
        |base_str, base, oid, value| {
            let (ifindex, v) = (ifindex_of(oid, base)?, numeric(value)?);
            Some(SnmpTableSample {
                oid_base: base_str.to_owned(),
                ifindex,
                value: v,
            })
        },
    )
    .await)
}

/// Walk string table columns (e.g. `ifName`, `ifAlias`) for interface metadata. Same
/// per-column behaviour as [`snmp_walk_v2c`]; non-string values are skipped.
pub async fn snmp_walk_strings_v2c(
    target: IpAddr,
    community: &str,
    column_oids: &[String],
    limits: WalkLimits,
) -> Result<TableWalk<SnmpTableString>, TransportError> {
    let client = connect(target, community, limits.timeout).await?;
    let mut budget = WalkBudget::within(limits);
    Ok(walk_columns(
        &client,
        target,
        column_oids,
        &mut budget,
        |base_str, base, oid, value| {
            let (ifindex, s) = (ifindex_of(oid, base)?, string_value(value)?);
            Some(SnmpTableString {
                oid_base: base_str.to_owned(),
                ifindex,
                value: s,
            })
        },
    )
    .await)
}

/// The column loop the numeric and string walkers share: one [`walk_column_v2c`] per column, each
/// folded into `budget`, stopping when the budget says so, and reporting how every column ended
/// (ADR-110 Increment 10).
///
/// `budget` is handed in rather than built here so that the budget's constructor stays in the text
/// of each public walker — `walk_budget.rs`'s `every_multi_column_call_takes_a_budget` reads it
/// there. `map` turns one in-subtree varbind into a row, or `None` to drop it (the column base
/// itself, a value of the wrong type).
async fn walk_columns<P, R>(
    pager: &P,
    target: IpAddr,
    column_oids: &[String],
    budget: &mut WalkBudget,
    map: impl Fn(&str, &ObjectIdentifier, &ObjectIdentifier, &ObjectValue) -> Option<R> + Send + Sync,
) -> TableWalk<R>
where
    P: BulkPager,
    R: Send,
{
    let mut rows = Vec::new();
    let mut columns = Vec::with_capacity(column_oids.len());
    let mut retries = RetryAllowance::new();
    let mut stopped: Option<Truncation> = None;
    for (asked, base_str) in column_oids.iter().enumerate() {
        if let Some(reason) = budget.spent() {
            note_truncation(reason, target, column_oids.len() - asked);
            stopped = Some(reason);
            columns.extend(not_asked(&column_oids[asked..]));
            break;
        }
        let Some(base) = parse_oid(base_str) else {
            tracing::warn!(%base_str, "skipping malformed table column OID");
            budget.record(ColumnOutcome::Skipped);
            columns.push(ColumnReport {
                column: base_str.clone(),
                end: ColumnEnd::Skipped,
            });
            continue;
        };
        let column = Column {
            base_str,
            base,
            row_budget: usize::MAX,
            empty: EmptyColumn::AskBase,
        };
        let stop = walk_column_v2c(pager, &column, budget, &mut retries, |oid, value| {
            if let Some(row) = map(base_str, &base, oid, value) {
                rows.push(row);
            }
        })
        .await;
        budget.record(stop.outcome());
        columns.push(ColumnReport {
            column: base_str.clone(),
            end: stop.end(),
        });
    }
    let stopped = conclude(stopped, budget, &columns, target);
    TableWalk {
        rows,
        columns,
        stopped,
    }
}

/// Walk table columns keeping each row's **full instance index** and **raw** value (ADR-038).
///
/// The two walkers above each collapse something on purpose — non-numeric values, or the
/// multi-part index — and both losses are fatal for adjacency data: `lldpRemTable` is indexed by
/// `(lldpRemTimeMark, lldpRemLocalPortNum, lldpRemIndex)` and a chassis id is typed octets. Same
/// per-column behaviour as the others; a value type this build has no representation for is
/// skipped rather than coerced.
/// `max_rows` bounds the **whole** call, across every column, and it is enforced *during* paging
/// rather than by truncating the result. That distinction is the point: memory is consumed while
/// the pages arrive, so a post-hoc truncation of a 400,000-row ARP table has already cost the
/// allocation it was meant to prevent. ADR-043 Increment 3 walks `ipNetToPhysicalPhysAddress` on
/// devices where that number is real.
pub async fn snmp_walk_instances_v2c(
    target: IpAddr,
    community: &str,
    column_oids: &[String],
    timeout: Duration,
    max_rows: usize,
) -> Result<crate::InstanceWalk, TransportError> {
    let client = connect(target, community, timeout).await?;
    let mut rows = Vec::new();
    let mut budget = WalkBudget::new(timeout);
    let mut retries = RetryAllowance::new();
    // Why the walk stopped, kept rather than dropped so the caller can be told the
    // device said nothing at all (ADR-110 Increment 4).
    let mut stopped: Option<Truncation> = None;
    for (asked, base_str) in column_oids.iter().enumerate() {
        if let Some(reason) = budget.spent() {
            note_truncation(reason, target, column_oids.len() - asked);
            stopped = Some(reason);
            break;
        }
        let Some(base) = parse_oid(base_str) else {
            tracing::warn!(%base_str, "skipping malformed table column OID");
            budget.record(ColumnOutcome::Skipped);
            continue;
        };
        if rows.len() >= max_rows {
            tracing::debug!(%base_str, max_rows, "instance walk row budget spent; skipping column");
            break;
        }
        let column = Column {
            base_str,
            base,
            row_budget: max_rows - rows.len(),
            // This walker never asked the base: an adjacency column's base carries no instance, so
            // there is nothing a GET could add to it.
            empty: EmptyColumn::Stop,
        };
        let stop = walk_column_v2c(&client, &column, &budget, &mut retries, |oid, value| {
            let Some(tail) = oid.relative_to(&base) else {
                return;
            };
            let instance = tail.as_slice().to_vec();
            if instance.is_empty() {
                return; // the column base itself: no instance
            }
            rows.push(SnmpInstanceRow {
                oid_base: base_str.clone(),
                instance,
                value: raw_value(value),
            });
        })
        .await;
        // This budget names no deadline, so no column here is ever cut part-way.
        budget.record(stop.outcome());
    }
    // The loop only consults the budget at the *top* of an iteration, so a walk whose last
    // two columns both failed ends by running out of columns rather than by tripping. Ask
    // once more — the question is about the device, not about where the loop stopped.
    let stopped = stopped.or_else(|| budget.spent());
    // A device that failed two columns in a row and gave up nothing is not a device that
    // does not implement these columns — it is one that is not answering. Saying so is what
    // lets `execute_mau` stop after one walk instead of three (ADR-110 Increment 4).
    if is_silence(rows.len(), stopped) {
        return Err(TransportError::Silent(target));
    }
    Ok(crate::InstanceWalk {
        every_column_answered: budget.every_column_answered(column_oids.len()),
        rows,
    })
}

/// One GETBULK page, as a column walk needs it.
pub(crate) struct Page {
    /// The varbinds in OID order.
    entries: Vec<(ObjectIdentifier, ObjectValue)>,
    /// The agent signalled end-of-MIB: nothing follows this page.
    end_of_mib: bool,
}

/// What a GET on a column's base returned, once `noSuchObject` / `noSuchInstance` have been taken
/// out of the error path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BaseAnswer {
    /// The base OID is itself a value.
    Value,
    /// The agent does not implement the column — an answer, not a failure.
    NoSuch,
}

/// The two requests a column walk sends (ADR-110 Increment 9).
///
/// A seam and nothing more: [`Snmp2cClient`] is the only production implementation, and the tests
/// drive [`walk_column_v2c`] through a scripted one. Before it existed the paging was either inside
/// `csnmp::walk_bulk` or inside a function that opened a UDP socket, and neither could be run
/// without a device — so the rule "a column that fails keeps its rows" could not have been tested.
#[async_trait]
pub(crate) trait BulkPager: Sync {
    /// One GETBULK after `cursor`.
    async fn bulk(&self, cursor: ObjectIdentifier) -> Result<Page, SnmpClientError>;
    /// One GET on a column's base — asked only when paging found nothing under it.
    async fn get_base(&self, base: ObjectIdentifier) -> Result<BaseAnswer, SnmpClientError>;
}

#[async_trait]
impl BulkPager for Snmp2cClient {
    async fn bulk(&self, cursor: ObjectIdentifier) -> Result<Page, SnmpClientError> {
        let result = self.get_bulk(&[cursor], 0, WALK_MAX_REPETITIONS).await?;
        // `GetBulkResult::values` is a `BTreeMap`, so it already arrives in OID order — which is
        // what "leading entries" in `page_slice` means.
        Ok(Page {
            entries: result.values.into_iter().collect(),
            end_of_mib: result.end_of_mib_view,
        })
    }

    async fn get_base(&self, base: ObjectIdentifier) -> Result<BaseAnswer, SnmpClientError> {
        match self.get(base).await {
            Ok(_) => Ok(BaseAnswer::Value),
            // Exactly the two `csnmp::walk_bulk` swallowed on an empty column. Anything else is an
            // error, and `outcome_of` decides whether it was an answer.
            Err(SnmpClientError::FailedBinding { binding })
                if matches!(
                    binding.value,
                    BindingValue::NoSuchObject | BindingValue::NoSuchInstance
                ) =>
            {
                Ok(BaseAnswer::NoSuch)
            }
            Err(e) => Err(e),
        }
    }
}

/// What a column walk does when its subtree turned out to be empty.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EmptyColumn {
    /// Ask the base with a GET, as `csnmp::walk_bulk` did — the numeric and string walkers keep the
    /// exact request sequence they had, so a healthy device is asked the same things as before.
    AskBase,
    /// Stop — the instance walker never asked.
    Stop,
}

/// One column to walk.
struct Column<'a> {
    base_str: &'a str,
    base: ObjectIdentifier,
    /// How many in-subtree varbinds this column may take. `usize::MAX` for the walkers that are
    /// bounded only by the request ceiling.
    row_budget: usize,
    empty: EmptyColumn,
}

/// Page one column with GETBULK until it leaves the subtree, reaches end-of-MIB or spends its row
/// budget, handing each kept varbind to `keep` as it arrives (ADR-110 Increment 9).
///
/// Returns the column's verdict **about the device**, which the caller folds into its
/// [`WalkBudget`]. Three things this does that `csnmp::walk_bulk` did not:
///
/// - 🚨 **Rows reach `keep` as each page arrives, so a column that fails keeps what it paged.**
///   `walk_bulk` returned the column in one `Result`, and one late page threw the whole column
///   away.
/// - **An unanswered request is sent again** when [`RetryAllowance::claim`] allows it — never for a
///   device that has not answered anything in this walk, so a silent device still costs one
///   timeout per column.
/// - **A request ceiling**: an agent whose pages do not advance, or a column longer than
///   `MAX_REQUESTS × WALK_MAX_REPETITIONS` rows, ends as answered rather than spinning.
/// - **A cut at the caller's deadline** before any page after the first, when the budget names one
///   ([`WalkBudget::cuts_mid_column`], ADR-110 Increment 10). The first page is always sent: whether
///   to start a column at all is the column loop's question, asked before this is called.
///
/// What it keeps from `walk_bulk`, deliberately: the subtree and end-of-MIB stops, and — for
/// [`EmptyColumn::AskBase`] — one GET on the base of an empty column, with `noSuchObject` read as
/// "answered, empty". The module doc of `walk_budget.rs` explains why counting consecutive failures
/// is only safe because an unimplemented column ends that way. That GET is not cut at the deadline:
/// it is one round trip, and it is what finishes the column.
async fn walk_column_v2c<P: BulkPager>(
    pager: &P,
    column: &Column<'_>,
    budget: &WalkBudget,
    retries: &mut RetryAllowance,
    mut keep: impl FnMut(&ObjectIdentifier, &ObjectValue) + Send,
) -> ColumnStop {
    const MAX_REQUESTS: usize = 4096;
    let base = column.base;
    let mut cursor = base;
    let mut taken = 0usize;
    let mut retried = 0u32;
    let mut requests = 0usize;
    loop {
        if requests == MAX_REQUESTS {
            tracing::debug!(base = %column.base_str, "column walk hit its request ceiling");
            return ColumnStop::Ended(ColumnOutcome::Answered);
        }
        if requests > 0 && budget.cuts_mid_column() {
            tracing::debug!(
                base = %column.base_str,
                kept = taken,
                "snmp column walk cut at the walk's deadline"
            );
            let resume_after = cursor
                .relative_to(&base)
                .map(|tail| tail.as_slice().to_vec())
                .unwrap_or_default();
            return ColumnStop::Cut(resume_after);
        }
        requests += 1;
        let page = match pager.bulk(cursor).await {
            Ok(page) => {
                retries.heard();
                if retried > 0 {
                    note_retry(true);
                }
                retried = 0;
                page
            }
            Err(e) => {
                let outcome = outcome_of(&e);
                if outcome != ColumnOutcome::Failed {
                    retries.heard();
                }
                if retries.claim(outcome, retried, budget) {
                    retried += 1;
                    continue;
                }
                if retried > 0 {
                    note_retry(false);
                }
                tracing::debug!(
                    base = %column.base_str,
                    error = %e,
                    kept = taken,
                    "snmp column walk ended on an error"
                );
                return ColumnStop::Ended(outcome);
            }
        };
        let (take, next) = page_slice(&base, &page.entries, column.row_budget - taken);
        for (oid, value) in page.entries.iter().take(take) {
            keep(oid, value);
        }
        taken += take;
        match next {
            // `next != cursor` guards the non-advancing agent; GETBULK is specified to return
            // strictly greater OIDs, but a buggy one that repeats itself must not loop us.
            Some(n) if !page.end_of_mib && n != cursor => cursor = n,
            _ => break,
        }
    }
    if taken > 0 || column.empty == EmptyColumn::Stop {
        return ColumnStop::Ended(ColumnOutcome::Answered);
    }
    // Nothing under the base. `csnmp::walk_bulk` asked the base itself at this point, and the
    // walkers that used it keep doing so: a device is then asked exactly what it was asked before.
    let mut retried = 0u32;
    loop {
        match pager.get_base(base).await {
            Ok(_) => {
                retries.heard();
                if retried > 0 {
                    note_retry(true);
                }
                // The value is dropped either way: the base carries no instance, so no walker can
                // key a row from it.
                return ColumnStop::Ended(ColumnOutcome::Answered);
            }
            Err(e) => {
                let outcome = outcome_of(&e);
                if outcome != ColumnOutcome::Failed {
                    retries.heard();
                }
                if retries.claim(outcome, retried, budget) {
                    retried += 1;
                    continue;
                }
                if retried > 0 {
                    note_retry(false);
                }
                tracing::debug!(base = %column.base_str, error = %e, "snmp column base get failed");
                return ColumnStop::Ended(outcome);
            }
        }
    }
}

/// How much of one GETBULK page this walk may keep, and where to continue from.
///
/// Returns the number of **leading** entries that are inside `base`'s subtree and within `budget`,
/// plus the OID to continue after — `None` meaning stop. Leading, because a page that leaves the
/// subtree ends the walk: everything after that point belongs to another table.
///
/// Pure so the four stop conditions can be tested without an agent. They are the whole correctness
/// of a bounded walk, and three of them (budget, subtree edge, empty page) are silent when wrong —
/// the walk simply returns less, or more, than it should.
fn page_slice(
    base: &ObjectIdentifier,
    page: &[(ObjectIdentifier, ObjectValue)],
    budget: usize,
) -> (usize, Option<ObjectIdentifier>) {
    let mut take = 0usize;
    for (oid, _) in page {
        if !base.is_prefix_of_or_equal(oid) {
            // Out of subtree: keep what came before it and stop for good.
            return (take, None);
        }
        if take == budget {
            return (take, None);
        }
        take += 1;
    }
    // The whole page was in-subtree and affordable. Continue after its last OID — unless the budget
    // is now spent, in which case there is nothing more to fetch.
    if take == budget {
        return (take, None);
    }
    (take, page.last().map(|(oid, _)| *oid))
}

/// Map an SNMP value onto [`SnmpValue`] without coercing.
///
/// Total, and matched variant-by-variant rather than with a wildcard: this is the walker that must
/// not silently drop a column, so a value type gaining a representation should be a compile error
/// here rather than a row that quietly disappears from an operator's neighbour table.
pub(crate) fn raw_value(value: &ObjectValue) -> SnmpValue {
    match value {
        ObjectValue::Integer(i) => SnmpValue::Int(i64::from(*i)),
        ObjectValue::Counter32(c) | ObjectValue::Unsigned32(c) | ObjectValue::TimeTicks(c) => {
            SnmpValue::Int(i64::from(*c))
        }
        // Saturate rather than wrap: a negative value would be read as a different subtype.
        ObjectValue::Counter64(c) => SnmpValue::Int(i64::try_from(*c).unwrap_or(i64::MAX)),
        ObjectValue::String(bytes) | ObjectValue::Opaque(bytes) => SnmpValue::Bytes(bytes.clone()),
        // Kept as octets so `render_bare_address` reads it the same way it reads a CDP address.
        ObjectValue::IpAddress(ip) => SnmpValue::Bytes(ip.octets().to_vec()),
        ObjectValue::ObjectId(oid) => SnmpValue::Oid(oid.to_string()),
    }
}

/// Open an SNMP v2c client to `target`.
async fn connect(
    target: IpAddr,
    community: &str,
    timeout: Duration,
) -> Result<Snmp2cClient, TransportError> {
    let addr = SocketAddr::new(target, SNMP_PORT);
    Snmp2cClient::new(addr, community.as_bytes().to_vec(), None, Some(timeout))
        .await
        .map_err(|e| TransportError::Io(format!("snmp connect {addr}: {e}")))
}

/// Row key for an instance OID relative to its column `base` — the numeric identity of a row,
/// delegated to the shared [`crate::ifindex_from_tail`] so the v2c and v3 walkers key rows
/// identically. Returns `None` when the OID isn't under `base`, or is `base` itself (no instance).
fn ifindex_of(oid: &ObjectIdentifier, base: &ObjectIdentifier) -> Option<u32> {
    let tail = oid.relative_to(base)?;
    crate::ifindex_from_tail(tail.as_slice())
}

/// Map an SNMP string-ish value to a Rust `String`: `OCTET STRING` (lossy UTF-8) or
/// `OBJECT IDENTIFIER` (dotted decimal — e.g. `sysObjectID`). Other value types yield
/// `None`. Device-supplied — callers must treat the result as untrusted.
fn string_value(value: &ObjectValue) -> Option<String> {
    match value {
        ObjectValue::String(bytes) => Some(String::from_utf8_lossy(bytes).into_owned()),
        ObjectValue::ObjectId(oid) => Some(oid.to_string()),
        _ => None,
    }
}

/// Parse a dotted OID string into a `csnmp` [`ObjectIdentifier`].
fn parse_oid(s: &str) -> Option<ObjectIdentifier> {
    let parts: Vec<u32> = s
        .split('.')
        .map(|p| p.parse().ok())
        .collect::<Option<_>>()?;
    ObjectIdentifier::try_from(parts.as_slice()).ok()
}

/// Map a numeric SNMP value to `f64`; non-numeric values yield `None` (skipped).
#[allow(clippy::cast_precision_loss)]
fn numeric(value: &ObjectValue) -> Option<f64> {
    match value {
        ObjectValue::Integer(i) => Some(*i as f64),
        ObjectValue::Counter32(c) => Some(f64::from(*c)),
        ObjectValue::Unsigned32(u) => Some(f64::from(*u)),
        ObjectValue::TimeTicks(t) => Some(f64::from(*t)),
        ObjectValue::Counter64(c) => Some(*c as f64),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use std::sync::Mutex;
    use std::time::Instant;

    // ── Bounded instance walk (ADR-043 I3) ───────────────────────────────────
    //
    // The cap exists because `ipNetToPhysicalPhysAddress` on a campus router is a real
    // hundred-thousand-row table, and every one of these stop conditions fails *silently* when it
    // is wrong: the walk just returns fewer rows, or keeps paging past its budget, and nothing
    // downstream can tell.

    fn oid(s: &str) -> ObjectIdentifier {
        parse_oid(s).expect("test oid")
    }

    fn page(oids: &[&str]) -> Vec<(ObjectIdentifier, ObjectValue)> {
        oids.iter()
            .map(|s| (oid(s), ObjectValue::Integer(1)))
            .collect()
    }

    #[test]
    fn a_full_in_subtree_page_is_taken_and_continues_after_its_last_oid() {
        let base = oid("1.3.6.1.2.1.4.35.1.4");
        let p = page(&[
            "1.3.6.1.2.1.4.35.1.4.1",
            "1.3.6.1.2.1.4.35.1.4.2",
            "1.3.6.1.2.1.4.35.1.4.3",
        ]);
        let (take, next) = page_slice(&base, &p, 100);
        assert_eq!(take, 3);
        assert_eq!(next, Some(oid("1.3.6.1.2.1.4.35.1.4.3")));
    }

    #[test]
    fn a_page_that_leaves_the_subtree_keeps_only_what_came_before_and_stops() {
        // GETBULK walks off the end of a column into the next one. Everything past that boundary
        // belongs to another table, and continuing would attribute its rows to this column.
        let base = oid("1.3.6.1.2.1.4.35.1.4");
        let p = page(&[
            "1.3.6.1.2.1.4.35.1.4.1",
            "1.3.6.1.2.1.4.35.1.5.1", // next column
            "1.3.6.1.2.1.4.35.1.5.2",
        ]);
        let (take, next) = page_slice(&base, &p, 100);
        assert_eq!(take, 1);
        assert_eq!(next, None, "leaving the subtree ends the walk for good");
    }

    #[test]
    fn the_budget_stops_the_walk_mid_page_rather_than_after_it() {
        // The property the whole change exists for. Taking the page and truncating afterwards has
        // already paid for the rows it meant to refuse.
        let base = oid("1.3.6.1.2.1.4.35.1.4");
        let p = page(&[
            "1.3.6.1.2.1.4.35.1.4.1",
            "1.3.6.1.2.1.4.35.1.4.2",
            "1.3.6.1.2.1.4.35.1.4.3",
        ]);
        let (take, next) = page_slice(&base, &p, 2);
        assert_eq!(take, 2);
        assert_eq!(next, None);
    }

    #[test]
    fn a_page_that_exactly_spends_the_budget_does_not_ask_for_another() {
        let base = oid("1.3.6.1.2.1.4.35.1.4");
        let p = page(&["1.3.6.1.2.1.4.35.1.4.1", "1.3.6.1.2.1.4.35.1.4.2"]);
        let (take, next) = page_slice(&base, &p, 2);
        assert_eq!(take, 2);
        assert_eq!(next, None, "the next request could only be refused anyway");
    }

    #[test]
    fn a_zero_budget_takes_nothing() {
        let base = oid("1.3.6.1.2.1.4.35.1.4");
        assert_eq!(
            page_slice(&base, &page(&["1.3.6.1.2.1.4.35.1.4.1"]), 0),
            (0, None)
        );
    }

    #[test]
    fn an_empty_page_stops_instead_of_looping() {
        // An agent that answers with nothing must end the walk; continuing from an unchanged cursor
        // is the infinite loop the request ceiling exists as a second guard against.
        let base = oid("1.3.6.1.2.1.4.35.1.4");
        assert_eq!(page_slice(&base, &[], 100), (0, None));
    }

    #[test]
    fn the_column_base_itself_is_still_paged_past() {
        // `is_prefix_of_or_equal` means the base can appear in a page. It carries no instance, so
        // the row is dropped later — but it must not end the walk, or a column would return empty.
        let base = oid("1.3.6.1.2.1.4.35.1.4");
        let p = page(&["1.3.6.1.2.1.4.35.1.4", "1.3.6.1.2.1.4.35.1.4.1"]);
        let (take, next) = page_slice(&base, &p, 100);
        assert_eq!(take, 2);
        assert_eq!(next, Some(oid("1.3.6.1.2.1.4.35.1.4.1")));
    }

    // ── Column walk over a scripted agent (ADR-110 Increment 9) ──────────────
    //
    // These run `walk_column_v2c` — the loop itself, not a decision extracted from it — against an
    // agent whose every reply is written down. Each asserts both what came back and what the agent
    // was *asked*, because the failure modes here are about requests: a retry that is never sent, a
    // retry sent to a silent device, a base GET that disappears.

    const COL: &str = "1.3.6.1.2.1.2.2.1.8";
    const NEXT_COL: &str = "1.3.6.1.2.1.2.2.1.9.1";

    /// One scripted reply.
    enum Reply {
        /// A GETBULK page carrying these OIDs (each valued `1`).
        Page(&'static [&'static str]),
        /// Nothing inside the per-request timeout.
        Timeout,
        /// An error PDU: bytes came back, so the agent is there.
        ErrorPdu,
        /// The base GET answered `noSuchObject`.
        NoSuch,
        /// A GETBULK page that arrives only after this long — how a test lets a deadline fall while
        /// a column is being paged. A real sleep: the budget reads `std::time::Instant`, which
        /// Tokio's paused clock does not move.
        Late(Duration, &'static [&'static str]),
    }

    #[derive(Debug, PartialEq, Eq)]
    enum Asked {
        Bulk(String),
        Base(String),
    }

    struct ScriptedAgent {
        replies: Mutex<VecDeque<Reply>>,
        asked: Mutex<Vec<Asked>>,
    }

    impl ScriptedAgent {
        fn new(replies: Vec<Reply>) -> Self {
            Self {
                replies: Mutex::new(replies.into()),
                asked: Mutex::new(Vec::new()),
            }
        }

        fn next(&self) -> Reply {
            self.replies
                .lock()
                .unwrap()
                .pop_front()
                .expect("the walk sent a request the script has no reply for")
        }

        fn asked(&self) -> Vec<Asked> {
            std::mem::take(&mut *self.asked.lock().unwrap())
        }
    }

    fn error_pdu(at: ObjectIdentifier) -> SnmpClientError {
        SnmpClientError::FailedBinding {
            binding: csnmp::message::VariableBinding {
                name: at,
                value: BindingValue::Unspecified,
            },
        }
    }

    #[async_trait]
    impl BulkPager for ScriptedAgent {
        async fn bulk(&self, cursor: ObjectIdentifier) -> Result<Page, SnmpClientError> {
            self.asked
                .lock()
                .unwrap()
                .push(Asked::Bulk(cursor.to_string()));
            match self.next() {
                Reply::Page(oids) => Ok(Page {
                    entries: page(oids),
                    end_of_mib: false,
                }),
                Reply::Late(delay, oids) => {
                    tokio::time::sleep(delay).await;
                    Ok(Page {
                        entries: page(oids),
                        end_of_mib: false,
                    })
                }
                Reply::Timeout => Err(SnmpClientError::TimedOut),
                Reply::ErrorPdu => Err(error_pdu(cursor)),
                Reply::NoSuch => panic!("a GETBULK was answered with a base-GET reply"),
            }
        }

        async fn get_base(&self, base: ObjectIdentifier) -> Result<BaseAnswer, SnmpClientError> {
            self.asked
                .lock()
                .unwrap()
                .push(Asked::Base(base.to_string()));
            match self.next() {
                Reply::NoSuch => Ok(BaseAnswer::NoSuch),
                Reply::Timeout => Err(SnmpClientError::TimedOut),
                Reply::ErrorPdu => Err(error_pdu(base)),
                Reply::Page(_) | Reply::Late(..) => panic!("a base GET was answered with a page"),
            }
        }
    }

    fn bulk(at: &str) -> Asked {
        Asked::Bulk(at.to_owned())
    }

    /// Walk [`COL`] once with a fresh budget and allowance, returning the verdict and the rows kept.
    async fn walk_one(agent: &ScriptedAgent) -> (ColumnOutcome, Vec<String>) {
        let budget = WalkBudget::new(Duration::from_secs(2));
        let mut retries = RetryAllowance::new();
        walk_with(agent, &budget, &mut retries).await
    }

    async fn walk_with(
        agent: &ScriptedAgent,
        budget: &WalkBudget,
        retries: &mut RetryAllowance,
    ) -> (ColumnOutcome, Vec<String>) {
        let (stop, kept) = walk_stop(agent, budget, retries).await;
        (stop.outcome(), kept)
    }

    /// As [`walk_with`], keeping whether the column was cut rather than folding that into an answer.
    async fn walk_stop(
        agent: &ScriptedAgent,
        budget: &WalkBudget,
        retries: &mut RetryAllowance,
    ) -> (ColumnStop, Vec<String>) {
        let column = Column {
            base_str: COL,
            base: oid(COL),
            row_budget: usize::MAX,
            empty: EmptyColumn::AskBase,
        };
        let mut kept = Vec::new();
        let stop = walk_column_v2c(agent, &column, budget, retries, |o, _| {
            kept.push(o.to_string());
        })
        .await;
        (stop, kept)
    }

    /// **The accepting side, first**: a healthy column pages until it leaves the subtree, keeps
    /// every row, and asks the base nothing. A walk that stopped after one page, or never, would
    /// fail here before any of the failure-path tests below could pass for the wrong reason.
    #[tokio::test]
    async fn a_healthy_column_pages_until_it_leaves_the_subtree() {
        let agent = ScriptedAgent::new(vec![
            Reply::Page(&["1.3.6.1.2.1.2.2.1.8.1", "1.3.6.1.2.1.2.2.1.8.2"]),
            Reply::Page(&["1.3.6.1.2.1.2.2.1.8.3", NEXT_COL]),
        ]);
        let (outcome, kept) = walk_one(&agent).await;
        assert_eq!(outcome, ColumnOutcome::Answered);
        assert_eq!(kept.len(), 3);
        assert_eq!(
            agent.asked(),
            vec![bulk(COL), bulk("1.3.6.1.2.1.2.2.1.8.2")],
            "two pages and no base GET: the column had rows"
        );
    }

    /// 🚨 The property Increment 3's silence rule stands on: a column the device does not implement
    /// is **answered**, not failed — one page walks straight out of the subtree, and one GET on the
    /// base comes back `noSuchObject`, exactly the request sequence `csnmp::walk_bulk` sent.
    #[tokio::test]
    async fn an_unimplemented_column_asks_the_base_once_and_ends_answered_empty() {
        let agent = ScriptedAgent::new(vec![Reply::Page(&[NEXT_COL]), Reply::NoSuch]);
        let (outcome, kept) = walk_one(&agent).await;
        assert_eq!(outcome, ColumnOutcome::Answered);
        assert!(kept.is_empty());
        assert_eq!(agent.asked(), vec![bulk(COL), Asked::Base(COL.to_owned())]);
    }

    /// A page that times out after the device has answered is asked again **from the same
    /// cursor**, and the column then finishes whole. Before Increment 9 this column was one
    /// `walk_bulk` call and ended as a failure with no rows at all.
    #[tokio::test]
    async fn a_timeout_after_the_device_answered_is_retried_once_from_the_same_cursor() {
        let agent = ScriptedAgent::new(vec![
            Reply::Page(&["1.3.6.1.2.1.2.2.1.8.1", "1.3.6.1.2.1.2.2.1.8.2"]),
            Reply::Timeout,
            Reply::Page(&["1.3.6.1.2.1.2.2.1.8.3", NEXT_COL]),
        ]);
        let (outcome, kept) = walk_one(&agent).await;
        assert_eq!(outcome, ColumnOutcome::Answered);
        assert_eq!(kept.len(), 3);
        assert_eq!(
            agent.asked(),
            vec![
                bulk(COL),
                bulk("1.3.6.1.2.1.2.2.1.8.2"),
                bulk("1.3.6.1.2.1.2.2.1.8.2"),
            ]
        );
    }

    /// 🚨 **A device that has not answered anything is never asked twice.** This is what keeps a
    /// silent device at Increment 3's price — one timeout per column — and a mass outage is exactly
    /// when every device is in this state.
    #[tokio::test]
    async fn a_timeout_before_the_device_ever_answered_is_not_retried() {
        let agent = ScriptedAgent::new(vec![Reply::Timeout]);
        let (outcome, kept) = walk_one(&agent).await;
        assert_eq!(outcome, ColumnOutcome::Failed);
        assert!(kept.is_empty());
        assert_eq!(agent.asked(), vec![bulk(COL)], "one request, no retry");
    }

    /// A page that stays unanswered through its retry ends the column as failed — and the rows
    /// paged before it are **kept**, which is the half `walk_bulk` could not do.
    #[tokio::test]
    async fn a_page_that_fails_twice_ends_the_column_failed_and_keeps_its_rows() {
        let agent = ScriptedAgent::new(vec![
            Reply::Page(&["1.3.6.1.2.1.2.2.1.8.1", "1.3.6.1.2.1.2.2.1.8.2"]),
            Reply::Timeout,
            Reply::Timeout,
        ]);
        let (outcome, kept) = walk_one(&agent).await;
        assert_eq!(outcome, ColumnOutcome::Failed);
        assert_eq!(
            kept.len(),
            2,
            "the two rows paged before the silence survive"
        );
        assert_eq!(
            agent.asked().len(),
            3,
            "the page, then the timeout and its one retry"
        );
    }

    /// An error PDU is an answer: the column ends there, keeps its rows, and is **not** retried —
    /// re-asking a device that replied would only get the same reply.
    #[tokio::test]
    async fn an_error_pdu_mid_column_keeps_rows_and_counts_as_answered() {
        let agent = ScriptedAgent::new(vec![
            Reply::Page(&["1.3.6.1.2.1.2.2.1.8.1"]),
            Reply::ErrorPdu,
        ]);
        let (outcome, kept) = walk_one(&agent).await;
        assert_eq!(outcome, ColumnOutcome::Answered);
        assert_eq!(kept.len(), 1);
        assert_eq!(agent.asked().len(), 2, "no retry after an answer");
    }

    /// The allowance is shared by the walk, not renewed per column: once it is spent, a timeout
    /// that would have been retried ends its column at once.
    #[tokio::test]
    async fn a_spent_allowance_is_not_renewed_by_the_next_column() {
        let budget = WalkBudget::new(Duration::from_secs(2));
        let mut retries = RetryAllowance::new();
        retries.heard();
        for _ in 0..crate::walk_budget::MAX_RETRIES_PER_WALK {
            assert!(retries.claim(ColumnOutcome::Failed, 0, &budget));
        }
        let agent = ScriptedAgent::new(vec![
            Reply::Page(&["1.3.6.1.2.1.2.2.1.8.1"]),
            Reply::Timeout,
        ]);
        let (outcome, kept) = walk_with(&agent, &budget, &mut retries).await;
        assert_eq!(outcome, ColumnOutcome::Failed);
        assert_eq!(kept.len(), 1);
        assert_eq!(agent.asked().len(), 2, "the spent allowance sent no retry");
    }

    /// **The whole loop, at a silent device**: two columns, one request each, and the walk stops
    /// as `Silent` without asking the other three. Increment 3 measured this as 51 s → 4 s; a
    /// retry on the first page of each column would quietly double it.
    #[tokio::test]
    async fn a_silent_device_costs_one_request_per_column_until_the_walk_stops() {
        let agent = ScriptedAgent::new(vec![Reply::Timeout, Reply::Timeout]);
        let columns: Vec<String> = (8..13).map(|c| format!("1.3.6.1.2.1.2.2.1.{c}")).collect();
        let mut budget = WalkBudget::new(Duration::from_secs(2));
        let walk = walk_columns(
            &agent,
            IpAddr::from([10, 0, 0, 1]),
            &columns,
            &mut budget,
            |_, base, oid, _| ifindex_of(oid, base),
        )
        .await;
        assert!(walk.rows.is_empty());
        assert_eq!(walk.stopped, Some(Truncation::Silent));
        assert_eq!(agent.asked().len(), 2, "one request per column, no retries");
        let ends: Vec<ColumnEnd> = walk.columns.into_iter().map(|c| c.end).collect();
        assert_eq!(
            ends,
            vec![
                ColumnEnd::Failed,
                ColumnEnd::Failed,
                ColumnEnd::NotAsked,
                ColumnEnd::NotAsked,
                ColumnEnd::NotAsked,
            ],
            "every column is reported, including the three never asked"
        );
    }

    // ── A deadline named by the caller (ADR-110 Increment 10) ─────────────────

    /// **The accepting side, first**: a column whose budget has run out but names no deadline still
    /// pages to its end. That is every walk except the interface table walk, and cutting them at the
    /// page would take away the polls that finished by overrunning 16 s.
    #[tokio::test]
    async fn a_column_with_no_named_deadline_finishes_even_past_its_budget() {
        let agent = ScriptedAgent::new(vec![
            Reply::Page(&["1.3.6.1.2.1.2.2.1.8.1", "1.3.6.1.2.1.2.2.1.8.2"]),
            Reply::Page(&["1.3.6.1.2.1.2.2.1.8.3", NEXT_COL]),
        ]);
        let expired = WalkBudget::with_remaining(Duration::ZERO);
        let (stop, kept) = walk_stop(&agent, &expired, &mut RetryAllowance::new()).await;
        assert_eq!(stop, ColumnStop::Ended(ColumnOutcome::Answered));
        assert_eq!(kept.len(), 3);
    }

    /// A deadline the caller named, passed, stops the column **before its next page** — the rows in
    /// hand are kept, and the column says where it stopped.
    #[tokio::test]
    async fn a_named_deadline_cuts_the_column_before_its_next_page() {
        let agent = ScriptedAgent::new(vec![Reply::Page(&[
            "1.3.6.1.2.1.2.2.1.8.1",
            "1.3.6.1.2.1.2.2.1.8.2",
        ])]);
        let passed = WalkBudget::within(WalkLimits::until(Duration::from_secs(2), Instant::now()));
        let (stop, kept) = walk_stop(&agent, &passed, &mut RetryAllowance::new()).await;
        assert_eq!(stop, ColumnStop::Cut(vec![2]), "resumes after ifIndex 2");
        assert_eq!(kept.len(), 2, "the page in hand is kept");
        assert_eq!(
            agent.asked(),
            vec![bulk(COL)],
            "the first page is always sent; the second is not"
        );
    }

    /// **The whole loop**: the deadline falls while the first column is being paged. That column is
    /// reported cut after the row it reached, the two after it are never asked, and the walk says
    /// `Deadline`.
    #[tokio::test]
    async fn a_deadline_that_falls_mid_column_cuts_it_and_asks_nothing_after() {
        let agent = ScriptedAgent::new(vec![Reply::Late(
            Duration::from_millis(120),
            &["1.3.6.1.2.1.2.2.1.8.1", "1.3.6.1.2.1.2.2.1.8.2"],
        )]);
        let columns: Vec<String> = (8..11).map(|c| format!("1.3.6.1.2.1.2.2.1.{c}")).collect();
        let deadline = Instant::now() + Duration::from_millis(40);
        let mut budget = WalkBudget::within(WalkLimits::until(Duration::from_secs(2), deadline));
        let walk = walk_columns(
            &agent,
            IpAddr::from([10, 0, 0, 1]),
            &columns,
            &mut budget,
            |_, base, oid, _| ifindex_of(oid, base),
        )
        .await;
        assert_eq!(walk.rows, vec![1, 2]);
        assert_eq!(walk.stopped, Some(Truncation::Deadline));
        let ends: Vec<ColumnEnd> = walk.columns.into_iter().map(|c| c.end).collect();
        assert_eq!(
            ends,
            vec![
                ColumnEnd::Partial {
                    resume_after: vec![2]
                },
                ColumnEnd::NotAsked,
                ColumnEnd::NotAsked,
            ]
        );
        assert_eq!(agent.asked().len(), 1);
    }

    /// 🚨 **A last column that finished after the deadline has asked for everything.** The page that
    /// carried it past the deadline also left its subtree, so nothing was cut — and the walk used to
    /// read the clock at the end and call itself truncated anyway, which is a `snmp_walk_complete`
    /// of `0` for a table that was read whole.
    #[tokio::test]
    async fn a_walk_whose_last_page_landed_after_the_deadline_is_not_truncated() {
        let agent = ScriptedAgent::new(vec![Reply::Late(
            Duration::from_millis(120),
            &["1.3.6.1.2.1.2.2.1.8.1", NEXT_COL],
        )]);
        let columns = vec![COL.to_owned()];
        let deadline = Instant::now() + Duration::from_millis(40);
        let mut budget = WalkBudget::within(WalkLimits::until(Duration::from_secs(2), deadline));
        let walk = walk_columns(
            &agent,
            IpAddr::from([10, 0, 0, 1]),
            &columns,
            &mut budget,
            |_, base, oid, _| ifindex_of(oid, base),
        )
        .await;
        assert_eq!(walk.rows, vec![1]);
        assert_eq!(walk.stopped, None);
        assert_eq!(walk.columns[0].end, ColumnEnd::Answered);
    }

    /// No v2c walker pages through `csnmp::walk_bulk` any more — and the three that walk columns
    /// all go through [`walk_column_v2c`]. Read as source, because a walker that quietly went back
    /// to `walk_bulk` would compile, pass every test above (they drive the shared function), and
    /// lose the rows of every column with one late page.
    ///
    /// ⚠️ The count is the accepting half: a detector that stopped matching would otherwise find
    /// "no `walk_bulk`" in an empty string.
    #[test]
    fn every_v2c_column_walk_pages_through_the_shared_loop() {
        use crate::module_source::{files_no_comments, roots};

        let code = files_no_comments(&roots("src", "snmp"))
            .into_iter()
            .find(|(name, _)| name == "snmp.rs")
            .map(|(_, code)| code)
            .expect("snmp.rs");
        assert!(
            !code.contains(&format!(".{}(", "walk_bulk")),
            "a v2c walker calls `walk_bulk` again: one late page would discard its whole column"
        );
        // The definition is spelled `walk_column_v2c<P: BulkPager>(`, so this counts call sites only.
        let callers = code.matches(&format!("{}(", "walk_column_v2c")).count();
        assert!(
            callers >= 2,
            "only {callers} calls to the shared column loop were found; the numeric/string loop \
             (`walk_columns`) and the instance walker should both call it"
        );
    }

    #[test]
    fn parses_valid_oid_and_rejects_garbage() {
        assert!(parse_oid("1.3.6.1.2.1.1.3.0").is_some());
        assert!(parse_oid("1.3.x.1").is_none());
        assert!(parse_oid("").is_none());
    }

    #[test]
    fn maps_numeric_values_and_skips_others() {
        assert_eq!(numeric(&ObjectValue::Counter64(1_000)), Some(1_000.0));
        assert_eq!(numeric(&ObjectValue::Integer(-5)), Some(-5.0));
        assert_eq!(numeric(&ObjectValue::TimeTicks(42)), Some(42.0));
        assert_eq!(numeric(&ObjectValue::String(vec![1, 2, 3])), None);
    }

    #[test]
    fn ifindex_uses_single_subid_directly_and_folds_multi_index() {
        let base = parse_oid("1.3.6.1.2.1.31.1.1.1.6").unwrap();
        // base + .7 → ifIndex 7 (single trailing sub-id, used as-is).
        let instance = parse_oid("1.3.6.1.2.1.31.1.1.1.6.7").unwrap();
        assert_eq!(ifindex_of(&instance, &base), Some(7));
        // The column base itself has no instance.
        assert_eq!(ifindex_of(&base, &base), None);
        // A multi-part instance (multi-index table, e.g. hwMemoryDevTable .1.0.0) folds to a
        // stable non-None key, and two distinct instances fold to distinct keys.
        let multi_a = parse_oid("1.3.6.1.2.1.31.1.1.1.6.1.0.0").unwrap();
        let multi_b = parse_oid("1.3.6.1.2.1.31.1.1.1.6.2.0.0").unwrap();
        let ka = ifindex_of(&multi_a, &base);
        let kb = ifindex_of(&multi_b, &base);
        assert!(ka.is_some() && kb.is_some());
        assert_ne!(ka, kb);
        assert_eq!(ifindex_of(&multi_a, &base), ka, "folding is deterministic");
        // An OID under a different column is not relative to this base.
        let other = parse_oid("1.3.6.1.2.1.2.2.1.8.7").unwrap();
        assert_eq!(ifindex_of(&other, &base), None);
    }

    #[test]
    fn string_value_decodes_octet_string_and_skips_numerics() {
        assert_eq!(
            string_value(&ObjectValue::String(b"Gi0/1".to_vec())),
            Some("Gi0/1".to_owned())
        );
        assert_eq!(string_value(&ObjectValue::Counter32(5)), None);
    }

    #[test]
    fn string_value_renders_object_id_as_dotted_decimal() {
        // sysObjectID comes back as an OBJECT IDENTIFIER — render it dotted for classification.
        let oid = parse_oid("1.3.6.1.4.1.9.1.516").unwrap();
        assert_eq!(
            string_value(&ObjectValue::ObjectId(oid)),
            Some("1.3.6.1.4.1.9.1.516".to_owned())
        );
    }

    /// The point of the instance walk: octets survive as octets. A chassis id that went through
    /// `string_value` would come back as replacement characters and could no longer be told apart
    /// from a different undecodable id.
    #[test]
    fn raw_value_keeps_octets_verbatim_where_string_value_would_mangle_them() {
        let mac = vec![0x00, 0x1b, 0x54, 0xff, 0x00, 0x9a];
        assert_eq!(
            raw_value(&ObjectValue::String(mac.clone())),
            SnmpValue::Bytes(mac.clone())
        );
        // The existing string walker's mapper loses those bytes.
        assert_ne!(
            string_value(&ObjectValue::String(mac.clone())).map(String::into_bytes),
            Some(mac)
        );
    }

    #[test]
    fn raw_value_maps_every_value_type() {
        assert_eq!(raw_value(&ObjectValue::Integer(-5)), SnmpValue::Int(-5));
        assert_eq!(raw_value(&ObjectValue::Counter32(7)), SnmpValue::Int(7));
        assert_eq!(
            raw_value(&ObjectValue::Counter64(u64::MAX)),
            SnmpValue::Int(i64::MAX),
            "an out-of-range counter saturates rather than wrapping into a negative subtype"
        );
        let oid = parse_oid("1.3.6.1.4.1.9").unwrap();
        assert_eq!(
            raw_value(&ObjectValue::ObjectId(oid)),
            SnmpValue::Oid("1.3.6.1.4.1.9".to_owned())
        );
        // An IpAddress arrives as its octets, so the same renderer reads it as a CDP address does.
        assert_eq!(
            raw_value(&ObjectValue::IpAddress(std::net::Ipv4Addr::new(
                10, 0, 0, 1
            ))),
            SnmpValue::Bytes(vec![10, 0, 0, 1])
        );
    }
}
