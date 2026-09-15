// SPDX-License-Identifier: AGPL-3.0-only
//! When a node's identity — its `sysDescr` and OS version — is read again (ADR-138).
//!
//! Core asks for an identity probe only while a node's maker is unknown (`scheduler/assemble.rs`),
//! so once a device was classified its version would never be read at all, and an upgrade would
//! never show. This is the other half: **each node is re-probed once an hour, decided here, on the
//! poller.** Deciding it in core would mean flipping `probe_identity` on the working-set spec twice
//! an hour per node — a delta on the bus each time, and a timestamp written to PostgreSQL to know
//! when — for a fact that changes a few times a year.
//!
//! Pure bookkeeping, no I/O. `stream.rs` owns the lock and asks; the probe itself is
//! `snmp.rs::execute_scalar_get`.
//!
//! ⚠️ **Soft state, lost on restart by design.** A restarted poller re-reads every node's version
//! within the hour, spread by [`first_offset`] rather than all on the first tick. Losing the map
//! costs one extra probe per node, never a missed one.

use super::*;

/// How long after a successful probe a node is probed again.
pub(super) const IDENTITY_PERIOD: Duration = Duration::from_secs(3_600);
/// How long after a failed one — the device was busy and the job was shed, or it did not answer.
/// Short enough that a node added during an outage gets its version soon after it recovers, long
/// enough that a device which never answers costs a probe per five minutes, not per poll.
pub(super) const IDENTITY_RETRY: Duration = Duration::from_secs(300);

/// Whether a job of this kind can carry the identity probe at all. Only the scalar SNMP GET runs
/// it (`execute_scalar_get`), so setting `probe_identity` on any other job would claim a probe
/// that never happens — and push its node's next attempt an hour away.
pub(super) fn carries_identity_probe(check: &CheckSpec) -> bool {
    matches!(check, CheckSpec::Snmp(_) | CheckSpec::SnmpV3(_))
}

/// Where in the first period a node's first probe lands after this poller starts: derived from the
/// node id, so a fleet spreads evenly across the hour and one node's offset is the same on every
/// restart. A random offset would spread as well; this one can be asserted.
pub(super) fn first_offset(node: NodeId) -> Duration {
    let bytes = node.as_uuid().as_u128().to_le_bytes();
    let mut low = [0u8; 8];
    low.copy_from_slice(&bytes[..8]);
    Duration::from_secs(u64::from_le_bytes(low) % IDENTITY_PERIOD.as_secs())
}

struct Entry {
    /// When the next probe is due.
    due: Instant,
    /// When a job for this node last went past — what [`IdentityCadence::prune`] ages by.
    seen: Instant,
}

/// The next identity probe per node.
#[derive(Default)]
pub(super) struct IdentityCadence {
    entries: HashMap<NodeId, Entry>,
    last_prune: Option<Instant>,
}

impl IdentityCadence {
    /// Whether the job going past now should probe identity.
    ///
    /// A node seen for the first time is scheduled `first_offset` from now and does not probe yet.
    /// A due node probes, and is **pessimistically** rescheduled [`IDENTITY_RETRY`] away: if the
    /// probe is shed or unanswered nothing else has to happen, and [`Self::succeeded`] moves it to
    /// [`IDENTITY_PERIOD`] only when it worked.
    pub(super) fn claim(&mut self, node: NodeId, now: Instant, first_offset: Duration) -> bool {
        self.prune(now);
        let Some(entry) = self.entries.get_mut(&node) else {
            self.entries.insert(
                node,
                Entry {
                    due: now + first_offset,
                    seen: now,
                },
            );
            return false;
        };
        entry.seen = now;
        if now < entry.due {
            return false;
        }
        entry.due = now + IDENTITY_RETRY;
        true
    }

    /// As [`Self::claim`], except that a node seen for the first time is due **now**.
    ///
    /// The row-name walk uses this rather than the offset (ADR-143 decision 2). Until a node's rows
    /// have names, a threshold rule scoped to a row name cannot match them, so a switch added a
    /// minute ago would be judged by the looser unnamed rule for up to an hour. The identity probe
    /// spreads its first reads across the hour to keep a restarted poller from issuing them all at
    /// once; the row-name walk rides a table job, and table jobs are already spread across their
    /// interval, so reading on first sight does not bunch them.
    pub(super) fn claim_first_now(&mut self, node: NodeId, now: Instant) -> bool {
        self.prune(now);
        if let std::collections::hash_map::Entry::Vacant(slot) = self.entries.entry(node) {
            slot.insert(Entry {
                due: now + IDENTITY_RETRY,
                seen: now,
            });
            return true;
        }
        self.claim(node, now, Duration::ZERO)
    }

    /// [`Self::claim`], or `true` whatever it answers when an operator asked for this poll (ADR-149).
    ///
    /// The claim is still taken, so a node first seen on a "poll now" gets its entry. What the
    /// operator skips is the wait — they are looking at a node page with "—" on it, not at the hour.
    /// A read that answers moves the node a full period out through [`Self::succeeded`], exactly as a
    /// scheduled one does, so the next scheduled job does not read it again.
    pub(super) fn claim_or_asked(
        &mut self,
        node: NodeId,
        now: Instant,
        first_offset: Duration,
        on_demand: bool,
    ) -> bool {
        self.claim(node, now, first_offset) || on_demand
    }

    /// [`Self::claim_first_now`], or `true` whatever it answers when an operator asked for this poll
    /// — the row-name walk's form of [`Self::claim_or_asked`].
    pub(super) fn claim_first_now_or_asked(
        &mut self,
        node: NodeId,
        now: Instant,
        on_demand: bool,
    ) -> bool {
        self.claim_first_now(node, now) || on_demand
    }

    /// A probe for this node got an answer: the next one is a full period away.
    pub(super) fn succeeded(&mut self, node: NodeId, now: Instant) {
        if let Some(entry) = self.entries.get_mut(&node) {
            entry.due = now + IDENTITY_PERIOD;
            entry.seen = now;
        } else {
            self.entries.insert(
                node,
                Entry {
                    due: now + IDENTITY_PERIOD,
                    seen: now,
                },
            );
        }
    }

    /// Forget nodes no job has mentioned for two periods — reassigned to another poller, or deleted —
    /// so the map tracks the working set rather than every node this poller ever held. Runs at most
    /// once a period, so it costs nothing on the per-job path.
    fn prune(&mut self, now: Instant) {
        if self
            .last_prune
            .is_some_and(|last| now.saturating_duration_since(last) < IDENTITY_PERIOD)
        {
            return;
        }
        self.last_prune = Some(now);
        self.entries
            .retain(|_, entry| now.saturating_duration_since(entry.seen) < 2 * IDENTITY_PERIOD);
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.entries.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    fn node(n: u128) -> NodeId {
        NodeId::from(Uuid::from_u128(n))
    }

    #[test]
    fn a_new_node_waits_for_its_offset_then_probes() {
        let mut c = IdentityCadence::default();
        let t0 = Instant::now();
        let offset = Duration::from_secs(600);
        assert!(!c.claim(node(1), t0, offset), "first sight only schedules");
        assert!(!c.claim(node(1), t0 + Duration::from_secs(599), offset));
        assert!(c.claim(node(1), t0 + offset, offset), "due at the offset");
    }

    #[test]
    fn a_probe_that_worked_waits_a_period_and_one_that_did_not_waits_the_retry() {
        let mut c = IdentityCadence::default();
        let t0 = Instant::now();
        assert!(!c.claim(node(1), t0, Duration::ZERO));
        assert!(c.claim(node(1), t0, Duration::ZERO));

        // Not reported as succeeded ⇒ the retry, not the period.
        assert!(!c.claim(
            node(1),
            t0 + IDENTITY_RETRY - Duration::from_secs(1),
            Duration::ZERO
        ));
        let retry_at = t0 + IDENTITY_RETRY;
        assert!(c.claim(node(1), retry_at, Duration::ZERO));

        // Succeeded ⇒ a full period, and the retry interval no longer triggers one.
        c.succeeded(node(1), retry_at);
        assert!(!c.claim(node(1), retry_at + IDENTITY_RETRY, Duration::ZERO));
        assert!(!c.claim(
            node(1),
            retry_at + IDENTITY_PERIOD - Duration::from_secs(1),
            Duration::ZERO
        ));
        assert!(c.claim(node(1), retry_at + IDENTITY_PERIOD, Duration::ZERO));
    }

    /// A claim is taken once: two jobs for one due node in the same instant probe once.
    #[test]
    fn a_due_node_is_claimed_once() {
        let mut c = IdentityCadence::default();
        let t0 = Instant::now();
        assert!(!c.claim(node(1), t0, Duration::ZERO));
        assert!(c.claim(node(1), t0, Duration::ZERO));
        assert!(!c.claim(node(1), t0, Duration::ZERO));
    }

    #[test]
    fn nodes_no_job_mentions_any_more_are_forgotten() {
        let mut c = IdentityCadence::default();
        let t0 = Instant::now();
        c.claim(node(1), t0, Duration::ZERO);
        c.claim(node(2), t0, Duration::ZERO);
        // Node 2 keeps being polled; node 1 was reassigned away. The last claim is more than a
        // period after the previous prune, or pruning would not run at all on it.
        let later = t0 + 2 * IDENTITY_PERIOD + Duration::from_secs(2);
        c.claim(
            node(2),
            t0 + IDENTITY_PERIOD + Duration::from_secs(1),
            Duration::ZERO,
        );
        c.claim(node(2), later, Duration::ZERO);
        assert_eq!(c.len(), 1, "only the node still in the working set is kept");
    }

    #[test]
    fn first_offsets_are_stable_and_inside_the_period() {
        for n in [
            0u128,
            1,
            7,
            u128::MAX,
            0x0123_4567_89ab_cdef_0123_4567_89ab_cdef,
        ] {
            let offset = first_offset(node(n));
            assert!(offset < IDENTITY_PERIOD, "{n}: {offset:?}");
            assert_eq!(offset, first_offset(node(n)));
        }
        // Not all the same second: a fleet must not land on one tick.
        let distinct: HashSet<_> = (1..=64u128)
            .map(|n| {
                first_offset(NodeId::from(Uuid::from_u128(
                    n.wrapping_mul(0x9E37_79B9_7F4A_7C15),
                )))
            })
            .collect();
        assert!(distinct.len() > 32, "{} distinct offsets", distinct.len());
    }

    /// ADR-143: the row-name walk reads a new node straight away, then waits like the probe does.
    #[test]
    fn claim_first_now_is_due_on_first_sight_and_then_keeps_the_period() {
        let mut c = IdentityCadence::default();
        let t0 = Instant::now();
        assert!(c.claim_first_now(node(1), t0), "a new node is read at once");
        assert!(
            !c.claim_first_now(node(1), t0 + Duration::from_secs(1)),
            "and is not read again on the next job"
        );
        // Unanswered ⇒ the retry; answered ⇒ a full period.
        assert!(c.claim_first_now(node(1), t0 + IDENTITY_RETRY));
        c.succeeded(node(1), t0 + IDENTITY_RETRY);
        assert!(!c.claim_first_now(
            node(1),
            t0 + IDENTITY_RETRY + IDENTITY_PERIOD - Duration::from_secs(1)
        ));
        assert!(c.claim_first_now(node(1), t0 + IDENTITY_RETRY + IDENTITY_PERIOD));
    }

    /// ADR-149: an operator's "poll now" reads a node the cadence would not — one seen for the first
    /// time, and one read a moment ago.
    #[test]
    fn an_operators_poll_reads_whatever_the_cadence_says() {
        let mut c = IdentityCadence::default();
        let t0 = Instant::now();
        let offset = Duration::from_secs(600);
        assert!(
            !c.claim_or_asked(node(1), t0, offset, false),
            "a scheduled job on first sight only schedules"
        );
        assert!(
            c.claim_or_asked(node(2), t0, offset, true),
            "a poll now reads a node seen for the first time"
        );

        c.succeeded(node(1), t0);
        let soon = t0 + Duration::from_secs(1);
        assert!(
            !c.claim_or_asked(node(1), soon, offset, false),
            "a scheduled job waits the period"
        );
        assert!(
            c.claim_or_asked(node(1), soon, offset, true),
            "a poll now does not"
        );
    }

    /// ADR-149: a read that answered on "poll now" moves the schedule a full period out — the slot the
    /// node was first scheduled for no longer fires, so pressing the button does not cost a second read.
    #[test]
    fn a_poll_now_that_answered_moves_the_schedule_a_period_out() {
        let mut c = IdentityCadence::default();
        let t0 = Instant::now();
        let offset = Duration::from_secs(600);
        assert!(!c.claim_or_asked(node(1), t0, offset, false));
        let pressed = t0 + Duration::from_secs(10);
        assert!(c.claim_or_asked(node(1), pressed, offset, true));
        c.succeeded(node(1), pressed);

        assert!(
            !c.claim_or_asked(node(1), t0 + offset, offset, false),
            "the first-read slot no longer fires"
        );
        assert!(!c.claim_or_asked(
            node(1),
            pressed + IDENTITY_PERIOD - Duration::from_secs(1),
            offset,
            false
        ));
        assert!(c.claim_or_asked(node(1), pressed + IDENTITY_PERIOD, offset, false));
    }

    /// ADR-149, the row-name form: right after a walk that answered, only a poll now walks again.
    #[test]
    fn claim_first_now_or_asked_walks_again_only_when_asked() {
        let mut c = IdentityCadence::default();
        let t0 = Instant::now();
        assert!(c.claim_first_now_or_asked(node(1), t0, false));
        c.succeeded(node(1), t0);
        let soon = t0 + Duration::from_secs(1);
        assert!(
            !c.claim_first_now_or_asked(node(1), soon, false),
            "the schedule waits the period"
        );
        assert!(
            c.claim_first_now_or_asked(node(1), soon, true),
            "a poll now walks the names anyway"
        );
    }

    #[test]
    fn only_the_scalar_snmp_get_carries_the_probe() {
        assert!(!carries_identity_probe(&testkit::icmp_job().check));
    }
}
