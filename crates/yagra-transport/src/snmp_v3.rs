// SPDX-License-Identifier: AGPL-3.0-only
//! SNMP v3 (USM) GET over a pure-Rust client (`snmp2`, crypto-rust backend) — resolves
//! the ADR-021 v3 question without a net-snmp FFI fallback.
//!
//! Auth (MD5/SHA1/SHA2 family) and privacy (DES/AES-128/192/256 CFB) come from the
//! credential resolved by core and inlined into the job (ADR-018/020) — this layer never
//! reads the secret store and never logs key material. Values are returned **raw**
//! (counters included) — rates are derived at query time (ADR-012). Live-only (needs a
//! device + UDP); the parameter mapping is unit-tested.

use crate::walk_budget::{
    conclude, is_silence, not_asked, note_truncation, ColumnOutcome, ColumnReport, ColumnStop,
    TableWalk, Truncation, WalkBudget, WalkLimits,
};
use crate::{
    SnmpInstanceRow, SnmpSample, SnmpStringSample, SnmpTableSample, SnmpTableString, SnmpV3Params,
    SnmpValue, TransportError,
};
use snmp2::{v3, AsyncSession, Oid, Value};
use std::net::{IpAddr, SocketAddr};
use std::time::Duration;

/// Standard SNMP agent port.
const SNMP_PORT: u16 = 161;

/// GETBULK max-repetitions per request — bounds one response PDU's size so a large table is paged,
/// not pulled in one oversized PDU. Mirrors the v2c walker's cap.
const WALK_MAX_REPETITIONS: u32 = 20;

/// Safety cap on GETBULK requests per column: a broken or looping agent can't spin the walk
/// forever (`WALK_MAX_REPETITIONS` × this bounds the rows collected per column).
const MAX_WALK_REQUESTS: usize = 1000;

/// Row budget for the walkers that predate ADR-043 Increment 3's explicit cap.
///
/// They are not unbounded — [`MAX_WALK_REQUESTS`] × [`WALK_MAX_REPETITIONS`] bounds them at 20,000
/// rows per column — so this names *which* bound applies rather than leaving a bare `usize::MAX`
/// that reads as none.
const ROWS_BOUNDED_BY_REQUEST_CEILING: usize = usize::MAX;

/// One failed `snmp2` exchange in which the agent said **nothing at all** (ADR-110 Increment 3).
///
/// Written down here rather than at each of the five loops below, because the rule is subtle and
/// the whole consecutive-failure design is wrong without it: **only the `tokio::time::timeout` arm
/// is silence.** A `snmp2::Error` means the exchange produced *something* — a decode failure, a
/// report PDU, an authentication complaint — and a device answering `noSuchObject` for a column it
/// does not implement must never be read as unreachable. `walk_budget`'s module doc carries the
/// full argument.
///
/// ⚠️ **v3 rarely reaches this at all.** [`open_session`] runs engine discovery before the first
/// column, so a silent device fails the whole *call* in one timeout rather than paying one per
/// column — the asymmetry with v2c, whose `connect` only binds a socket and never speaks. The
/// budget is wired here anyway for the device that goes quiet after the session is up, and so that
/// the tenth loop someone adds inherits it rather than reopening the defect.
const AGENT_SAID_NOTHING: ColumnOutcome = ColumnOutcome::Failed;

/// The agent answered — including by complaining. See [`AGENT_SAID_NOTHING`].
const AGENT_ANSWERED: ColumnOutcome = ColumnOutcome::Answered;

/// Open a v3 session against `target` and run engine discovery (id/boots/time) — required
/// before authenticated requests.
async fn open_session(
    target: IpAddr,
    params: &SnmpV3Params,
    timeout: Duration,
) -> Result<AsyncSession, TransportError> {
    let security = build_security(params).map_err(TransportError::Io)?;
    let addr = SocketAddr::new(target, SNMP_PORT);
    let mut session = AsyncSession::new_v3(addr, 0, security)
        .await
        .map_err(|e| TransportError::Io(format!("snmp v3 connect {addr}: {e}")))?;
    tokio::time::timeout(timeout, session.init())
        .await
        .map_err(|_| TransportError::Io(format!("snmp v3 engine discovery {addr}: timeout")))?
        .map_err(|e| TransportError::Io(format!("snmp v3 engine discovery {addr}: {e}")))?;
    Ok(session)
}

/// What one scalar exchange produced. Owned, so the seam below does not have to hand back a
/// `Pdu<'_>` borrowed from the session it was read through.
#[derive(Debug, PartialEq, Eq)]
enum Exchange<T> {
    /// The agent answered; these are its varbinds that the mapper could use.
    Answered(Vec<T>),
    /// The agent answered by complaining — a report PDU, a decode failure, `noSuchObject`.
    /// Still an answer: see [`AGENT_SAID_NOTHING`].
    Complained,
    /// Nothing came back inside the timeout.
    Silent,
}

/// One scalar GET, with the answer already mapped out of the borrowed PDU.
///
/// A seam rather than a direct `session.get` call because `AsyncSession` is a concrete type with
/// no constructor a test can drive, so the rule in [`read_scalars`] — the one that shipped wrong —
/// was unreachable by any test for the whole life of the v3 client (ADR-161).
#[async_trait::async_trait]
trait ScalarSession: Send {
    async fn scalar_get<T: Send + 'static>(
        &mut self,
        oid: &Oid<'_>,
        timeout: Duration,
        map: for<'a, 'b> fn(&'a Value<'b>) -> Option<T>,
    ) -> Exchange<T>;
}

#[async_trait::async_trait]
impl ScalarSession for AsyncSession {
    async fn scalar_get<T: Send + 'static>(
        &mut self,
        oid: &Oid<'_>,
        timeout: Duration,
        map: for<'a, 'b> fn(&'a Value<'b>) -> Option<T>,
    ) -> Exchange<T> {
        match tokio::time::timeout(timeout, self.get(oid)).await {
            Ok(Ok(pdu)) => Exchange::Answered(
                pdu.varbinds
                    .filter_map(|(_, value)| map(&value))
                    .collect::<Vec<T>>(),
            ),
            Ok(Err(e)) => {
                tracing::debug!(error = %e, "snmp v3 scalar get failed");
                Exchange::Complained
            }
            Err(_) => Exchange::Silent,
        }
    }
}

/// Read scalar `oids` one at a time off an open session, mapping each answer with `map`.
///
/// Shared by the numeric and string reads, which differ only in the mapper and in what they make
/// of a session that heard nothing (`extensibility.md` §3).
///
/// 🚨 **A silent exchange ends the read, and that is a correctness rule, not a budget.**
/// `tokio::time::timeout` cancels the `get` future but the socket stays open inside the session,
/// and the vendored `snmp2` does a single un-matched `recv` and validates the request-id
/// *afterwards* (`vendor/snmp2/src/asyncsession.rs:177`, `pdu.rs:699`) — there is no "keep reading
/// until the id matches" loop. So the late reply to the timed-out request is what the **next**
/// OID's `recv` returns, and it fails validation as `RequestIdMismatch`, which arrives here as
/// [`Exchange::Complained`] — an *answer*. The session is one reply behind from then on, every
/// remaining OID yields nothing, and the caller reads the empty result as "this credential does
/// not work". For discovery that meant a correct v3 credential on a slow device being scored as a
/// failure (ADR-161). Once anything has been silent the session cannot be trusted, so we stop.
async fn read_scalars<S: ScalarSession + ?Sized, T: Send + 'static>(
    session: &mut S,
    target: IpAddr,
    oids: &[String],
    timeout: Duration,
    map: for<'a, 'b> fn(&'a Value<'b>) -> Option<T>,
) -> (Vec<(String, T)>, WalkBudget) {
    let mut read = Vec::with_capacity(oids.len());
    let mut budget = WalkBudget::new(timeout);
    for (asked, oid_str) in oids.iter().enumerate() {
        if let Some(reason) = budget.spent() {
            note_truncation(reason, target, oids.len() - asked);
            break;
        }
        let Some(oid) = parse_oid(oid_str) else {
            tracing::warn!(%oid_str, "skipping malformed OID");
            budget.record(ColumnOutcome::Skipped);
            continue;
        };
        match session.scalar_get(&oid, timeout, map).await {
            Exchange::Answered(values) => {
                read.extend(values.into_iter().map(|v| (oid_str.clone(), v)));
                budget.record(AGENT_ANSWERED);
            }
            Exchange::Complained => budget.record(AGENT_ANSWERED),
            Exchange::Silent => {
                tracing::debug!(%oid_str, "snmp v3 scalar get timed out");
                budget.record(AGENT_SAID_NOTHING);
                // See the 🚨 above: this session is now desynced. Stopping costs the remaining
                // OIDs of a device that has already proved slow; continuing costs correctness.
                let skipped = oids.len() - asked - 1;
                if skipped > 0 {
                    note_truncation(Truncation::Silent, target, skipped);
                }
                break;
            }
        }
    }
    (read, budget)
}

/// Fetch `oids` from `target` via SNMP v3 (USM). Per-OID failures are logged and skipped
/// so a single bad OID doesn't fail the whole poll; an auth/engine failure fails the call.
///
/// As the v2c GET: `Ok(vec![])` means the agent answered and implements none of these OIDs,
/// and a session that then heard nothing is [`TransportError::Silent`] (ADR-138 Increment 5).
/// A device silent from the start fails engine discovery instead, as an `Io` error.
pub async fn snmp_get_v3(
    target: IpAddr,
    params: &SnmpV3Params,
    oids: &[String],
    timeout: Duration,
) -> Result<Vec<SnmpSample>, TransportError> {
    let mut session = open_session(target, params, timeout).await?;
    let (read, budget) = read_scalars(&mut session, target, oids, timeout, numeric).await;
    let samples: Vec<SnmpSample> = read
        .into_iter()
        .map(|(oid, value)| SnmpSample { oid, value })
        .collect();
    // As the v2c GET: a session that opened and then heard nothing is reported as silent, so the
    // caller can tell it from an agent that answered `noSuchObject` to everything (ADR-138
    // Increment 5). A device that is silent from the start never reaches here — engine discovery
    // in [`open_session`] fails first, as an `Io` error.
    if budget.heard_nothing() {
        return Err(TransportError::Silent(target));
    }
    Ok(samples)
}

/// Fetch string-valued scalar `oids` (e.g. `sysDescr.0` / `sysName.0`) from `target` via
/// SNMP v3 (USM). Non-string values are skipped. Used by discovery for device identity.
///
/// Unlike [`snmp_get_v3`], a session that heard nothing is `Ok(vec![])` here, not
/// [`TransportError::Silent`]: its one caller, the identity probe's string read, treats an error
/// and an empty answer alike, so the distinction would decide nothing (ADR-138 Increment 5).
pub async fn snmp_get_v3_strings(
    target: IpAddr,
    params: &SnmpV3Params,
    oids: &[String],
    timeout: Duration,
) -> Result<Vec<SnmpStringSample>, TransportError> {
    let mut session = open_session(target, params, timeout).await?;
    let (read, _budget) = read_scalars(&mut session, target, oids, timeout, string_value).await;
    Ok(read
        .into_iter()
        .map(|(oid, value)| SnmpStringSample { oid, value })
        .collect())
}

/// Walk numeric table columns from `target` via SNMP v3 (USM) GETBULK — the v3 analogue of
/// `snmp_walk_v2c`. Each column base yields one numeric row per instance, keyed by ifIndex (a
/// single trailing sub-id) or a folded synthetic key (multi-index tables). A per-column walk
/// failure is logged and skipped. Counters are returned **raw** (rates derived at query time,
/// ADR-012).
pub async fn snmp_walk_v3(
    target: IpAddr,
    params: &SnmpV3Params,
    column_oids: &[String],
    limits: WalkLimits,
) -> Result<TableWalk<SnmpTableSample>, TransportError> {
    let mut session = open_session(target, params, limits.timeout).await?;
    let mut budget = WalkBudget::within(limits);
    // Why the walk stopped is kept, for the reason `snmp_walk_v2c` gives: a `Deadline` here means
    // the caller's configured metric columns were never asked for (ADR-110 Increment 6).
    Ok(walk_columns_v3(
        &mut session,
        target,
        column_oids,
        limits.timeout,
        &mut budget,
        |base_str, tail, value| {
            let ifindex = crate::ifindex_from_tail(tail)?;
            numeric(value).map(|v| SnmpTableSample {
                oid_base: base_str.to_owned(),
                ifindex,
                value: v,
            })
        },
    )
    .await)
}

/// Walk string-valued table columns (e.g. `ifName`, `ifAlias`) from `target` via SNMP v3 (USM)
/// GETBULK — the v3 analogue of `snmp_walk_strings_v2c`, for interface metadata (PostgreSQL, never
/// TSDB labels — ADR-011). Same per-column skip-on-error behaviour; non-string values are skipped.
pub async fn snmp_walk_strings_v3(
    target: IpAddr,
    params: &SnmpV3Params,
    column_oids: &[String],
    limits: WalkLimits,
) -> Result<TableWalk<SnmpTableString>, TransportError> {
    let mut session = open_session(target, params, limits.timeout).await?;
    let mut budget = WalkBudget::within(limits);
    Ok(walk_columns_v3(
        &mut session,
        target,
        column_oids,
        limits.timeout,
        &mut budget,
        |base_str, tail, value| {
            let ifindex = crate::ifindex_from_tail(tail)?;
            string_value(value).map(|s| SnmpTableString {
                oid_base: base_str.to_owned(),
                ifindex,
                value: s,
            })
        },
    )
    .await)
}

/// The column loop the numeric and string v3 walkers share — the twin of `snmp::walk_columns`: one
/// [`walk_column_v3`] per column, folded into `budget`, reporting how every column ended
/// (ADR-110 Increment 10).
///
/// `budget` is handed in so that its constructor stays in the text of each public walker, where
/// `every_multi_column_call_takes_a_budget` reads it. `map` receives the column base as the caller
/// spelled it, the instance tail, and the value.
async fn walk_columns_v3<R>(
    session: &mut AsyncSession,
    target: IpAddr,
    column_oids: &[String],
    timeout: Duration,
    budget: &mut WalkBudget,
    map: impl Fn(&str, &[u32], &Value) -> Option<R>,
) -> TableWalk<R> {
    let mut rows = Vec::new();
    let mut columns = Vec::with_capacity(column_oids.len());
    let mut stopped: Option<Truncation> = None;
    for (asked, base_str) in column_oids.iter().enumerate() {
        if let Some(reason) = budget.spent() {
            note_truncation(reason, target, column_oids.len() - asked);
            stopped = Some(reason);
            columns.extend(not_asked(&column_oids[asked..]));
            break;
        }
        let stop = walk_column_v3(
            session,
            base_str,
            timeout,
            budget,
            ROWS_BOUNDED_BY_REQUEST_CEILING,
            |tail, value| map(base_str, tail, value),
            &mut rows,
        )
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

/// Walk table columns keeping each row's **full instance index** and **raw** value via SNMP v3
/// (USM) — the v3 analogue of `snmp::snmp_walk_instances_v2c` (ADR-038). Adjacency data cannot go
/// through the numeric or string walkers: those fold the multi-part index and lossily decode the
/// octets, and `lldpRemTable` needs both preserved.
/// `max_rows` bounds the whole call across every column, enforced while paging — see the v2c twin
/// for why truncating afterwards would not be a bound at all. The pre-existing
/// [`MAX_WALK_REQUESTS`] ceiling is a *request* limit and stays as the defence against an agent
/// that never advances; it says nothing about how many rows a well-behaved agent can return.
pub async fn snmp_walk_instances_v3(
    target: IpAddr,
    params: &SnmpV3Params,
    column_oids: &[String],
    timeout: Duration,
    max_rows: usize,
) -> Result<crate::InstanceWalk, TransportError> {
    let mut session = open_session(target, params, timeout).await?;
    let mut rows = Vec::new();
    let mut budget = WalkBudget::new(timeout);
    // Why the walk stopped, kept rather than dropped so the caller can be told the
    // device said nothing at all (ADR-110 Increment 4).
    let mut stopped: Option<Truncation> = None;
    for (asked, base_str) in column_oids.iter().enumerate() {
        if let Some(reason) = budget.spent() {
            note_truncation(reason, target, column_oids.len() - asked);
            stopped = Some(reason);
            break;
        }
        if rows.len() >= max_rows {
            tracing::debug!(%base_str, max_rows, "instance walk row budget spent; skipping column");
            break;
        }
        let row_budget = max_rows - rows.len();
        // This budget names no deadline, so no column here is ever cut part-way.
        let stop = walk_column_v3(
            &mut session,
            base_str,
            timeout,
            &budget,
            row_budget,
            |tail, value| {
                raw_value(value).map(|v| SnmpInstanceRow {
                    oid_base: base_str.clone(),
                    instance: tail.to_vec(),
                    value: v,
                })
            },
            &mut rows,
        )
        .await;
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

/// Walk one column subtree via repeated GETBULK, mapping each in-subtree varbind to an `R` row
/// (via `map`) and pushing it onto `out`. Pages until the walk leaves the column's subtree, the
/// agent signals end-of-MIB, a request fails/times out, the agent stops advancing, or the
/// per-column request cap ([`MAX_WALK_REQUESTS`]) is hit. Errors are logged and end this column
/// only (one bad column doesn't fail the whole poll).
///
/// `map` receives the instance's **whole** sub-identifier tail. The metric walkers immediately fold
/// it with [`crate::ifindex_from_tail`] (so the v2c and v3 row keying can never diverge); the
/// neighbour walker keeps it, because a folded `lldpRemTable` index cannot be reassembled into
/// which local port faces which peer.
/// `row_budget` caps how many rows this column may contribute. [`ROWS_BOUNDED_BY_REQUEST_CEILING`]
/// is the value for the walkers that predate ADR-043 Increment 3 and are already bounded by
/// [`MAX_WALK_REQUESTS`] × [`WALK_MAX_REPETITIONS`] rows — naming it says which bound applies rather
/// than leaving a bare `usize::MAX` that reads as "no bound at all".
///
/// `budget` is consulted before every page after the first, and stops the column there only when
/// it names the caller's deadline ([`WalkBudget::cuts_mid_column`], ADR-110 Increment 10).
async fn walk_column_v3<R>(
    session: &mut AsyncSession,
    base_str: &str,
    timeout: Duration,
    budget: &WalkBudget,
    row_budget: usize,
    map: impl Fn(&[u32], &Value) -> Option<R>,
    out: &mut Vec<R>,
) -> ColumnStop {
    if parse_oid(base_str).is_none() {
        tracing::warn!(%base_str, "skipping malformed table column OID");
        return ColumnStop::Ended(ColumnOutcome::Skipped);
    }
    let mut cursor_str = base_str.to_owned();
    let mut taken = 0usize;
    for request in 0..MAX_WALK_REQUESTS {
        if taken >= row_budget {
            return ColumnStop::Ended(AGENT_ANSWERED);
        }
        if request > 0 && budget.cuts_mid_column() {
            tracing::debug!(%base_str, kept = taken, "snmp v3 column walk cut at the walk's deadline");
            return ColumnStop::Cut(tail_subids(&cursor_str, base_str).unwrap_or_default());
        }
        let Some(cursor) = parse_oid(&cursor_str) else {
            return ColumnStop::Ended(ColumnOutcome::Skipped);
        };
        let pdu = match tokio::time::timeout(
            timeout,
            session.getbulk(&[&cursor], 0, WALK_MAX_REPETITIONS),
        )
        .await
        {
            Ok(Ok(pdu)) => pdu,
            Ok(Err(e)) => {
                tracing::debug!(%base_str, error = %e, "snmp v3 table walk failed");
                return ColumnStop::Ended(AGENT_ANSWERED);
            }
            Err(_) => {
                tracing::debug!(%base_str, "snmp v3 table walk timed out");
                return ColumnStop::Ended(AGENT_SAID_NOTHING);
            }
        };
        // Scan this page: collect in-subtree rows and note the last OID reached so the next
        // GETBULK can continue after it. Stop the moment the walk leaves the column subtree or
        // the agent reports it has no more.
        let mut last_in_subtree: Option<String> = None;
        let mut stop = false;
        for (oid, value) in pdu.varbinds {
            // The budget is checked here, inside the page loop, rather than after it: the cost this
            // bound exists to refuse is the memory the rows occupy, and a row already pushed has
            // already cost it.
            if taken >= row_budget {
                stop = true;
                break;
            }
            if matches!(
                value,
                Value::EndOfMibView | Value::NoSuchObject | Value::NoSuchInstance
            ) {
                stop = true;
                break;
            }
            let oid_str = oid.to_id_string();
            let Some(tail) = tail_subids(&oid_str, base_str) else {
                stop = true; // walked past this column's subtree — done
                break;
            };
            // Counted whether or not `map` produced a row: the budget bounds what the *device* is
            // allowed to make this walk read, and a row the mapper declined still arrived.
            taken += 1;
            if let Some(row) = map(&tail, &value) {
                out.push(row);
            }
            last_in_subtree = Some(oid_str);
        }
        match last_in_subtree {
            // More in-subtree rows may follow — advance past the last one, unless the agent
            // failed to advance (defensive: GETBULK returns strictly-greater OIDs).
            Some(next) if !stop && next != cursor_str => cursor_str = next,
            // The agent answered every page it was asked for; the column ended on its own terms.
            _ => return ColumnStop::Ended(AGENT_ANSWERED),
        }
    }
    ColumnStop::Ended(AGENT_ANSWERED)
}

/// Sub-identifiers of `oid_str` past the column `base_str`, or `None` when `oid_str` is not a
/// strict descendant of `base_str` (a different subtree, or the base itself — no instance).
/// Compared on the dotted-decimal form so this doesn't depend on the client's relative-OID API;
/// requires a `.` boundary after the base so `…2` is not read as a prefix of `…20`.
fn tail_subids(oid_str: &str, base_str: &str) -> Option<Vec<u32>> {
    let rest = oid_str
        .strip_prefix(base_str)
        .and_then(|r| r.strip_prefix('.'))?;
    rest.split('.').map(|p| p.parse::<u32>().ok()).collect()
}

/// Map job-level v3 params onto the `snmp2` USM security config. Key material flows
/// through untouched and is never logged. `Err` carries a *static* description only
/// (no secrets) suitable for logs.
fn build_security(params: &SnmpV3Params) -> Result<v3::Security, String> {
    let auth_key = params.auth_key.as_deref().unwrap_or("");
    let mut security = v3::Security::new(params.user.as_bytes(), auth_key.as_bytes());

    match params.security_level.as_str() {
        "noauth" => {
            security = security.with_auth(v3::Auth::NoAuthNoPriv);
        }
        "auth" => {
            security = security
                .with_auth_protocol(parse_auth_protocol(params.auth_protocol.as_deref())?)
                .with_auth(v3::Auth::AuthNoPriv);
        }
        "authpriv" => {
            let priv_key = params
                .priv_key
                .as_deref()
                .ok_or_else(|| "snmp v3 authpriv requires a privacy key".to_owned())?;
            security = security
                .with_auth_protocol(parse_auth_protocol(params.auth_protocol.as_deref())?)
                .with_auth(v3::Auth::AuthPriv {
                    cipher: parse_cipher(params.priv_protocol.as_deref())?,
                    privacy_password: priv_key.as_bytes().to_vec(),
                });
        }
        other => return Err(format!("unknown snmp v3 security level: {other}")),
    }
    Ok(security)
}

/// Parse an auth-protocol token (defaults to SHA-1 when unset — the common modern floor;
/// MD5 must be opted into explicitly).
fn parse_auth_protocol(token: Option<&str>) -> Result<v3::AuthProtocol, String> {
    match token.unwrap_or("sha") {
        "md5" => Ok(v3::AuthProtocol::Md5),
        "sha" | "sha1" => Ok(v3::AuthProtocol::Sha1),
        "sha224" => Ok(v3::AuthProtocol::Sha224),
        "sha256" => Ok(v3::AuthProtocol::Sha256),
        "sha384" => Ok(v3::AuthProtocol::Sha384),
        "sha512" => Ok(v3::AuthProtocol::Sha512),
        other => Err(format!("unknown snmp v3 auth protocol: {other}")),
    }
}

/// Parse a privacy-cipher token (defaults to AES-128 when unset; DES is explicit-only).
fn parse_cipher(token: Option<&str>) -> Result<v3::Cipher, String> {
    match token.unwrap_or("aes") {
        "des" => Ok(v3::Cipher::Des),
        "aes" | "aes128" => Ok(v3::Cipher::Aes128),
        "aes192" => Ok(v3::Cipher::Aes192),
        "aes256" => Ok(v3::Cipher::Aes256),
        other => Err(format!("unknown snmp v3 privacy protocol: {other}")),
    }
}

/// Parse a dotted OID string into an `snmp2` [`Oid`].
fn parse_oid(s: &str) -> Option<Oid<'static>> {
    let parts: Vec<u64> = s
        .split('.')
        .map(|p| p.parse().ok())
        .collect::<Option<_>>()?;
    Oid::from(parts.as_slice()).ok()
}

/// Map a numeric SNMP value to `f64`; non-numeric values yield `None` (skipped).
#[allow(clippy::cast_precision_loss)]
fn numeric(value: &Value) -> Option<f64> {
    match value {
        Value::Integer(i) => Some(*i as f64),
        Value::Counter32(c) => Some(f64::from(*c)),
        Value::Unsigned32(u) => Some(f64::from(*u)),
        Value::Timeticks(t) => Some(f64::from(*t)),
        Value::Counter64(c) => Some(*c as f64),
        _ => None,
    }
}

/// Map an SNMP value onto [`SnmpValue`] without coercing — the neighbour walk's mapper. `None` for
/// types with no representation (null, end-of-MIB, the exception markers), skipped like any other
/// unusable row. Mirrors `snmp::raw_value`; kept per-module because the two clients have separate
/// value enums.
///
/// Matched variant-by-variant rather than with a wildcard, for the same reason `snmp::raw_value`
/// is: this is a walker that must not silently drop a column. It used to end in `_ => None`, and
/// that wildcard swallowed two types the v2c mapper handles — `IpAddress` and `Opaque`. Nothing
/// noticed, because the only consumer at the time (the neighbour walk) reads neither. `ipAdEntNetMask`
/// is an ASN.1 `IpAddress`, so ADR-043's IPv4 mask column would have come back empty on every
/// SNMPv3 node while working perfectly on v2c. A new variant should be a compile error here, not a
/// row that quietly disappears.
pub(crate) fn raw_value(value: &Value) -> Option<SnmpValue> {
    match value {
        Value::Integer(i) => Some(SnmpValue::Int(*i)),
        Value::Counter32(c) | Value::Unsigned32(c) => Some(SnmpValue::Int(i64::from(*c))),
        Value::Timeticks(t) => Some(SnmpValue::Int(i64::from(*t))),
        // Saturate rather than wrap: a negative value would be read as a different subtype.
        Value::Counter64(c) => Some(SnmpValue::Int(i64::try_from(*c).unwrap_or(i64::MAX))),
        Value::OctetString(bytes) | Value::Opaque(bytes) => {
            Some(SnmpValue::Bytes((*bytes).to_vec()))
        }
        // Kept as octets so the caller reads it the same way it reads any other address column.
        Value::IpAddress(octets) => Some(SnmpValue::Bytes(octets.to_vec())),
        Value::ObjectIdentifier(oid) => Some(SnmpValue::Oid(oid.to_id_string())),
        // No representation as a scalar column value: structural types, the three exception
        // markers an agent returns instead of a value, and the PDU tags that never appear in a
        // varbind at all. Listed so the next variant added upstream lands here as a compile error.
        Value::Boolean(_)
        | Value::Null
        | Value::Sequence(_)
        | Value::Set(_)
        | Value::Constructed(..)
        | Value::EndOfMibView
        | Value::NoSuchObject
        | Value::NoSuchInstance
        | Value::GetRequest(_)
        | Value::GetNextRequest(_)
        | Value::GetBulkRequest(_)
        | Value::Response(_)
        | Value::SetRequest(_)
        | Value::InformRequest(_)
        | Value::Trap(_)
        | Value::Report(_) => None,
    }
}

/// Map a string-ish SNMP value to a `String`: octet string (lossy UTF-8) or object
/// identifier (dotted decimal — e.g. `sysObjectID`). Device-supplied, treat as untrusted;
/// other value types yield `None` (skipped).
fn string_value(value: &Value) -> Option<String> {
    match value {
        Value::OctetString(bytes) => Some(String::from_utf8_lossy(bytes).into_owned()),
        Value::ObjectIdentifier(oid) => Some(oid.to_id_string()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A scripted session: one outcome per call, and it counts the calls.
    ///
    /// It answers with a real `Value` rather than a pre-mapped `T` so one fake serves both
    /// mappers — the only way to write it, since [`ScalarSession::scalar_get`] is generic in `T`
    /// and a fake cannot conjure one.
    struct ScriptedSession {
        script: Vec<Exchange<&'static [u8]>>,
        asked: usize,
    }

    impl ScriptedSession {
        fn new(script: Vec<Exchange<&'static [u8]>>) -> Self {
            Self { script, asked: 0 }
        }
    }

    #[async_trait::async_trait]
    impl ScalarSession for ScriptedSession {
        async fn scalar_get<T: Send + 'static>(
            &mut self,
            _oid: &Oid<'_>,
            _timeout: Duration,
            map: for<'a, 'b> fn(&'a Value<'b>) -> Option<T>,
        ) -> Exchange<T> {
            let step = self.script.get(self.asked);
            self.asked += 1;
            match step {
                Some(Exchange::Answered(bytes)) => Exchange::Answered(
                    bytes
                        .iter()
                        .filter_map(|b| map(&Value::OctetString(b)))
                        .collect(),
                ),
                Some(Exchange::Complained) => Exchange::Complained,
                Some(Exchange::Silent) | None => Exchange::Silent,
            }
        }
    }

    /// One scripted answer carrying a single octet-string varbind.
    fn answered(value: &'static [u8]) -> Exchange<&'static [u8]> {
        Exchange::Answered(vec![value])
    }

    fn three_oids() -> Vec<String> {
        vec![
            "1.3.6.1.2.1.1.1.0".to_owned(),
            "1.3.6.1.2.1.1.2.0".to_owned(),
            "1.3.6.1.2.1.1.5.0".to_owned(),
        ]
    }

    fn here() -> IpAddr {
        IpAddr::V4(std::net::Ipv4Addr::LOCALHOST)
    }

    /// 🚨 ADR-161: the rule that could not be tested before this seam existed, and was wrong.
    ///
    /// The first OID goes unanswered. Its reply is still in flight, and `snmp2` has no loop that
    /// discards a reply whose request-id does not match — so asking the second OID reads the
    /// *first* one's answer and fails validation. The scripted `Complained` after the `Silent` is
    /// exactly that, and it used to be recorded as "the agent answered".
    #[tokio::test]
    async fn a_silent_scalar_get_ends_the_read() {
        let mut session = ScriptedSession::new(vec![
            Exchange::Silent,
            Exchange::Complained,
            answered(b"never reached"),
        ]);
        let (read, budget) = read_scalars(
            &mut session,
            here(),
            &three_oids(),
            Duration::from_millis(10),
            string_value,
        )
        .await;
        assert_eq!(
            session.asked, 1,
            "the session is one reply behind after a timeout, so nothing more may be asked of it"
        );
        assert!(read.is_empty());
        assert!(
            budget.heard_nothing(),
            "and the silence must survive as silence — recording the desynced reply as an answer              is what turned a slow device into a wrong credential"
        );
    }

    /// The other direction, so the fix cannot be "stop on anything that is not a value".
    /// A complaint is an answer (see [`AGENT_SAID_NOTHING`]): `noSuchObject` for an OID a device
    /// does not implement must not end the read of the OIDs it does.
    #[tokio::test]
    async fn a_complaint_is_an_answer_and_the_read_continues() {
        let mut session = ScriptedSession::new(vec![
            Exchange::Complained,
            answered(b"VRP (R) software"),
            answered(b"core-sw-01"),
        ]);
        let (read, budget) = read_scalars(
            &mut session,
            here(),
            &three_oids(),
            Duration::from_millis(10),
            string_value,
        )
        .await;
        assert_eq!(session.asked, 3);
        assert_eq!(read.len(), 2);
        assert_eq!(read[0].1, "VRP (R) software");
        assert!(!budget.heard_nothing());
    }

    /// The ordinary path, stated because the two tests above are both about not-answers and a
    /// read that returned nothing at all would satisfy neither.
    #[tokio::test]
    async fn every_oid_is_read_when_the_agent_answers() {
        let mut session =
            ScriptedSession::new(vec![answered(b"a"), answered(b"b"), answered(b"c")]);
        let oids = three_oids();
        let (read, _) = read_scalars(
            &mut session,
            here(),
            &oids,
            Duration::from_millis(10),
            string_value,
        )
        .await;
        assert_eq!(session.asked, 3);
        assert_eq!(
            read.iter().map(|(o, _)| o.clone()).collect::<Vec<_>>(),
            oids,
            "each value is tagged with the OID it was read for"
        );
    }

    fn params(level: &str) -> SnmpV3Params {
        SnmpV3Params {
            user: "monitor".to_owned(),
            security_level: level.to_owned(),
            auth_protocol: Some("sha256".to_owned()),
            auth_key: Some("auth-pass-12345".to_owned()),
            priv_protocol: Some("aes256".to_owned()),
            priv_key: Some("priv-pass-12345".to_owned()),
        }
    }

    #[test]
    fn builds_security_for_every_level() {
        assert!(build_security(&params("noauth")).is_ok());
        assert!(build_security(&params("auth")).is_ok());
        assert!(build_security(&params("authpriv")).is_ok());
    }

    #[test]
    fn authpriv_without_priv_key_is_rejected() {
        let mut p = params("authpriv");
        p.priv_key = None;
        let err = build_security(&p).expect_err("must require a privacy key");
        // The error names the problem without echoing any key material.
        assert!(err.contains("privacy key"));
    }

    #[test]
    fn unknown_level_and_protocols_are_rejected() {
        let mut p = params("paranoid");
        assert!(build_security(&p).is_err());
        p.security_level = "auth".to_owned();
        p.auth_protocol = Some("rot13".to_owned());
        assert!(build_security(&p).is_err());
        let mut p2 = params("authpriv");
        p2.priv_protocol = Some("xor".to_owned());
        assert!(build_security(&p2).is_err());
    }

    #[test]
    fn protocol_tokens_map_and_default_safely() {
        // Defaults: SHA-1 auth, AES-128 privacy. MD5/DES only on explicit request.
        assert!(matches!(
            parse_auth_protocol(None),
            Ok(v3::AuthProtocol::Sha1)
        ));
        assert!(matches!(
            parse_auth_protocol(Some("sha512")),
            Ok(v3::AuthProtocol::Sha512)
        ));
        assert!(matches!(
            parse_auth_protocol(Some("md5")),
            Ok(v3::AuthProtocol::Md5)
        ));
        assert!(matches!(parse_cipher(None), Ok(v3::Cipher::Aes128)));
        assert!(matches!(
            parse_cipher(Some("aes256")),
            Ok(v3::Cipher::Aes256)
        ));
        assert!(matches!(parse_cipher(Some("des")), Ok(v3::Cipher::Des)));
    }

    #[test]
    fn parses_valid_oid_and_rejects_garbage() {
        assert!(parse_oid("1.3.6.1.2.1.1.3.0").is_some());
        assert!(parse_oid("1.3.x.1").is_none());
        assert!(parse_oid("").is_none());
    }

    #[test]
    fn tail_subids_extracts_instance_and_rejects_non_descendants() {
        let base = "1.3.6.1.2.1.31.1.1.1.6";
        // A single-instance row (ifIndex 7).
        assert_eq!(tail_subids("1.3.6.1.2.1.31.1.1.1.6.7", base), Some(vec![7]));
        // A multi-part instance (multi-index table) returns every sub-id.
        assert_eq!(
            tail_subids("1.3.6.1.2.1.31.1.1.1.6.1.0.0", base),
            Some(vec![1, 0, 0])
        );
        // The column base itself has no instance.
        assert_eq!(tail_subids(base, base), None);
        // A different subtree is not a descendant.
        assert_eq!(tail_subids("1.3.6.1.2.1.2.2.1.8.7", base), None);
        // A string prefix that is NOT an OID-boundary descendant must be rejected: `…1.6` must not
        // capture `…1.60.1` (the `.` boundary guards this).
        assert_eq!(tail_subids("1.3.6.1.2.1.31.1.1.1.60.1", base), None);
    }

    /// `walk_column_v3` hands the mapper the **whole** tail. The metric walkers fold it; the
    /// neighbour walker must be able to keep it, or `lldpRemTable`'s three-part index is lost.
    #[test]
    fn the_walk_mapper_sees_the_unfolded_instance() {
        let base = "1.0.8802.1.1.2.1.4.1.1.5";
        let tail = tail_subids("1.0.8802.1.1.2.1.4.1.1.5.0.7.3", base).unwrap();
        assert_eq!(tail, vec![0, 7, 3]);
        // Folding it — what the metric mappers do — is a one-way trip.
        assert!(crate::ifindex_from_tail(&tail).is_some());
    }

    #[test]
    fn raw_value_keeps_octets_verbatim_where_string_value_would_mangle_them() {
        let mac = b"\x00\x1bT\xff\x00\x9a";
        assert_eq!(
            raw_value(&Value::OctetString(mac)),
            Some(SnmpValue::Bytes(mac.to_vec()))
        );
        assert_ne!(
            string_value(&Value::OctetString(mac)).map(String::into_bytes),
            Some(mac.to_vec())
        );
        assert_eq!(raw_value(&Value::Integer(4)), Some(SnmpValue::Int(4)));
        assert_eq!(raw_value(&Value::NoSuchObject), None);
    }

    #[test]
    fn tail_subids_feeds_shared_ifindex_keying() {
        // A single trailing sub-id keys directly; a multi-part tail folds to a stable, distinct key
        // — the exact same [`crate::ifindex_from_tail`] the v2c walker uses (no divergence).
        let base = "1.3.6.1.2.1.31.1.1.1.6";
        let single = tail_subids("1.3.6.1.2.1.31.1.1.1.6.7", base).unwrap();
        assert_eq!(crate::ifindex_from_tail(&single), Some(7));
        let a = tail_subids("1.3.6.1.2.1.31.1.1.1.6.1.0.0", base).unwrap();
        let b = tail_subids("1.3.6.1.2.1.31.1.1.1.6.2.0.0", base).unwrap();
        let ka = crate::ifindex_from_tail(&a);
        let kb = crate::ifindex_from_tail(&b);
        assert!(ka.is_some() && kb.is_some());
        assert_ne!(ka, kb);
    }

    #[test]
    fn maps_numeric_values_and_skips_others() {
        assert_eq!(numeric(&Value::Counter64(1_000)), Some(1_000.0));
        assert_eq!(numeric(&Value::Integer(-5)), Some(-5.0));
        assert_eq!(numeric(&Value::Timeticks(42)), Some(42.0));
        assert_eq!(numeric(&Value::OctetString(b"x")), None);
        assert_eq!(numeric(&Value::NoSuchObject), None);
    }

    #[test]
    fn maps_string_values_and_skips_others() {
        assert_eq!(
            string_value(&Value::OctetString(b"Huawei USG")),
            Some("Huawei USG".to_owned())
        );
        // Invalid UTF-8 decodes lossily rather than failing (device data is untrusted).
        assert_eq!(
            string_value(&Value::OctetString(b"fw\xff01")),
            Some("fw\u{fffd}01".to_owned())
        );
        assert_eq!(string_value(&Value::Integer(1)), None);
        assert_eq!(string_value(&Value::NoSuchObject), None);
    }

    #[test]
    fn string_value_renders_object_id_as_dotted_decimal() {
        // sysObjectID comes back as an OBJECT IDENTIFIER — render it dotted for classification.
        let oid = parse_oid("1.3.6.1.4.1.2011.2.1").unwrap();
        assert_eq!(
            string_value(&Value::ObjectIdentifier(oid)),
            Some("1.3.6.1.4.1.2011.2.1".to_owned())
        );
    }
}
