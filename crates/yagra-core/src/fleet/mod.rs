// SPDX-License-Identifier: AGPL-3.0-only
//! The poller fleet: the coordinator, the durable inventory, pools and their coverage, and the ring
//! that spreads nodes over pollers (ADR-009/020/107).

// Distributed poller pool (ADR-009/020): the coordinator owns the live registry + working-set
// distribution and consumes the ring / Redis mirror / durable inventory below.
pub(crate) mod coordinator;
/// How far apart each node's polls are, shared with the readers outside the scheduler (ADR-144).
pub(crate) mod pollers;
pub(crate) mod pool_coverage;
/// Effective poll-pool resolution (node > ancestor folder > default).
pub(crate) mod poolres;
pub(crate) mod ring;
pub(crate) mod volatile;
