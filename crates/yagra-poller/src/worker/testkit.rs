// SPDX-License-Identifier: AGPL-3.0-only
//! Fixtures more than one conversation's tests need (ADR-099).
//!
//! Only two things qualify, and that is the point: everything else in this module's 58 tests builds
//! a job for one check family and belongs beside it. Keeping the shared pair here rather than
//! duplicating it is the same rule the production code follows.
//!
//! ⚠️ [`sample`] had **two byte-identical copies** in the old single test module, `sample` and
//! `sample_value`, five lines each. That is `extensibility.md` §3 at its smallest scale, and the
//! small ones are the ones nobody reviews.

use super::*;
use std::net::{IpAddr, Ipv4Addr};
use uuid::Uuid;
use yagra_bus::IcmpCheck;
use yagra_common::NodeId;

pub(super) fn icmp_job() -> PollJob {
    PollJob::icmp(
        Uuid::nil(),
        NodeId::from(Uuid::nil()),
        IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1)),
        IcmpCheck::default(),
        30,
    )
}

pub(super) fn sample(r: &PollResult, metric: &str) -> Option<f64> {
    r.samples
        .iter()
        .find(|s| s.metric == metric)
        .map(|s| s.value)
}
