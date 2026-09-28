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
//!
//! ⚠️ **The exception is a node whose maker core does not know** (ADR-138 Increment 6): it is read
//! in full on first sight, so a restarted poller reads every such node on its first poll, together.
//! Accepted rather than spread: before Increment 6 those nodes were read in full on *every* poll.

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

/// How much of a node's identity one job reads (ADR-138 Increment 6).
///
/// Core asks for `sysDescr` on every poll while a node's maker is unknown, and until this existed
/// the poller answered that ask with the whole probe — the OS-version OIDs, a Huawei's patch table
/// and the ENTITY-MIB serial walk — on every poll. What core needs to classify is two scalars;
/// the rest changes a few times a year and belongs to the hourly cadence.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum IdentityRead {
    /// Read nothing.
    Skip,
    /// `sysDescr` and `sysObjectID` in one GET — what core classifies a device from.
    Classify,
    /// The whole probe: version, patch, model and serial as well.
    Full,
}

impl IdentityRead {
    /// What a job asks for on its own, with no cadence consulted: the full probe when core set the
    /// flag. Only a caller that has no [`IdentityCadence`] — a test — reads this way.
    #[cfg(test)]
    pub(crate) fn asked_by(job: &PollJob) -> Self {
        if job.probe_identity {
            Self::Full
        } else {
            Self::Skip
        }
    }
}

struct Entry {
    /// When the next probe is due.
    due: Instant,
    /// When a job for this node last went past — what [`IdentityCadence::prune`] ages by.
    seen: Instant,
    /// Where core's ask for this node stands (ADR-138 Increment 7). Only [`IdentityCadence::read_for`]
    /// reads it; the row-name walk shares this type and leaves it at [`Asked::No`].
    asked: Asked,
}

/// Whether core is asking about a node's maker, and whether a full read has answered since it
/// started (ADR-138 Increment 7).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Asked {
    /// Core does not ask: the node's maker is known.
    No,
    /// Core asks, and no full read has answered since it started. Nothing is read between full
    /// reads: a classify read would reach core before the device's own model, and core would fill
    /// the node's model from the `sysDescr` guess and keep it (`COALESCE`) — an AireOS controller's
    /// `Cisco Controller` for good (ADR-147 Increment 6).
    AwaitingFull,
    /// Core asks, and a full read has answered: the two classifying scalars are read between.
    Classifying,
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
                    asked: Asked::No,
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
                asked: Asked::No,
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

    /// How much of this node's identity the job going past now reads (ADR-138 Increment 6).
    ///
    /// `core_asked` is the job's `probe_identity` as core sent it: the node's maker is still unknown.
    /// Such a node reads the full probe when the cadence is due — **on first sight**
    /// ([`Self::claim_first_now`]), so a device added a minute ago still shows its version and serial
    /// after its first poll, and **on the first poll after core starts asking** about a node it
    /// already knew — and the two classifying scalars between, but only once a full read has
    /// answered ([`Asked`]). A node core did not ask about reads in full on the offset cadence, as it
    /// always has.
    ///
    /// Report the outcome through [`Self::finished`], which moves the schedule only after an
    /// answered full read.
    pub(super) fn read_for(
        &mut self,
        node: NodeId,
        now: Instant,
        first_offset: Duration,
        core_asked: bool,
        on_demand: bool,
    ) -> IdentityRead {
        if !core_asked {
            if let Some(entry) = self.entries.get_mut(&node) {
                entry.asked = Asked::No;
            }
            return if self.claim_or_asked(node, now, first_offset, on_demand) {
                IdentityRead::Full
            } else {
                IdentityRead::Skip
            };
        }
        // A node core has only now started asking about — its maker cleared, or reclassified
        // (ADR-140) — is read in full at once, whatever its old schedule said. A node never seen
        // before is due at once through `claim_first_now`.
        if let Some(entry) = self.entries.get_mut(&node) {
            if entry.asked == Asked::No {
                entry.asked = Asked::AwaitingFull;
                entry.due = now;
            }
        }
        let full = self.claim_first_now_or_asked(node, now, on_demand);
        let asked = match self.entries.get_mut(&node) {
            Some(entry) => {
                if entry.asked == Asked::No {
                    entry.asked = Asked::AwaitingFull;
                }
                entry.asked
            }
            None => Asked::AwaitingFull,
        };
        match (full, asked) {
            (true, _) => IdentityRead::Full,
            (false, Asked::Classifying) => IdentityRead::Classify,
            (false, Asked::AwaitingFull | Asked::No) => IdentityRead::Skip,
        }
    }

    /// A job's identity read is over; `answered` is whether it brought back a `sysDescr`.
    ///
    /// 🚨 **The one place that decides whether a read moves the schedule** (ADR-138 Increments 6
    /// and 7). Only an answered [`IdentityRead::Full`] does: a `Classify` read answers `sysDescr`
    /// on every poll, and counting it would push the full read a period away on every poll — the
    /// version would never be read at all. `stream.rs` calls this after every job without a
    /// condition of its own, so the rule is here, beside the tests that hold it.
    pub(super) fn finished(
        &mut self,
        node: NodeId,
        now: Instant,
        read: IdentityRead,
        answered: bool,
    ) {
        match read {
            IdentityRead::Full if answered => {
                self.succeeded(node, now);
                if let Some(entry) = self.entries.get_mut(&node) {
                    if entry.asked == Asked::AwaitingFull {
                        entry.asked = Asked::Classifying;
                    }
                }
            }
            IdentityRead::Full | IdentityRead::Classify | IdentityRead::Skip => {}
        }
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
                    asked: Asked::No,
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

    /// ADR-138 Increment 6. An unknown-maker node reads the full probe on first sight and then once
    /// a period, and only the two classifying scalars on every poll between — the case that used to
    /// read the version, patch table and serial walk on every poll.
    #[test]
    fn a_node_core_asks_about_reads_in_full_once_a_period_and_classifies_between() {
        let mut c = IdentityCadence::default();
        let t0 = Instant::now();
        let offset = Duration::from_secs(1_800);
        assert_eq!(
            c.read_for(node(1), t0, offset, true, false),
            IdentityRead::Full,
            "a device added a minute ago gets its version on its first poll"
        );
        c.finished(node(1), t0, IdentityRead::Full, true);
        for minutes in [1, 5, 30, 59] {
            assert_eq!(
                c.read_for(
                    node(1),
                    t0 + Duration::from_secs(minutes * 60),
                    offset,
                    true,
                    false
                ),
                IdentityRead::Classify,
                "{minutes} min after a full read, core's ask reads sysDescr only"
            );
        }
        assert_eq!(
            c.read_for(node(1), t0 + IDENTITY_PERIOD, offset, true, false),
            IdentityRead::Full,
            "a period later the full read is due again"
        );
    }

    /// 🚨 The trap Increment 6 was written around, now held where the rule lives: an answered
    /// `Classify` read, reported through `finished` exactly as `stream.rs` reports it, moves
    /// nothing. Were it counted, every poll would push the full read a period away and the version
    /// would never be read. This asserts the schedule, which is what that mistake breaks.
    #[test]
    fn classify_reads_do_not_push_the_full_read_away() {
        let mut c = IdentityCadence::default();
        let t0 = Instant::now();
        assert_eq!(
            c.read_for(node(1), t0, Duration::ZERO, true, false),
            IdentityRead::Full
        );
        c.finished(node(1), t0, IdentityRead::Full, true);
        for minutes in [1, 2, 30, 59] {
            let at = t0 + Duration::from_secs(minutes * 60);
            assert_eq!(
                c.read_for(node(1), at, Duration::ZERO, true, false),
                IdentityRead::Classify
            );
            c.finished(node(1), at, IdentityRead::Classify, true);
        }
        assert_eq!(
            c.read_for(node(1), t0 + IDENTITY_PERIOD, Duration::ZERO, true, false),
            IdentityRead::Full,
            "the period is counted from the full read, however many classify reads answered between"
        );
    }

    /// ADR-138 Increment 7. Until a full read has answered, core's ask reads nothing between full
    /// reads: a classify read would reach core first, and core would fill the node's model from the
    /// `sysDescr` guess and keep it — the device's own model could never replace it.
    #[test]
    fn no_classify_read_comes_before_a_full_read_has_answered() {
        let mut c = IdentityCadence::default();
        let t0 = Instant::now();
        assert_eq!(
            c.read_for(node(1), t0, Duration::ZERO, true, false),
            IdentityRead::Full
        );
        // Shed, or no sysDescr: the short retry, and nothing read before it.
        c.finished(node(1), t0, IdentityRead::Full, false);
        for minutes in [1, 2, 4] {
            let at = t0 + Duration::from_secs(minutes * 60);
            assert_eq!(
                c.read_for(node(1), at, Duration::ZERO, true, false),
                IdentityRead::Skip,
                "{minutes} min after an unanswered full read"
            );
            c.finished(node(1), at, IdentityRead::Skip, false);
        }
        assert_eq!(
            c.read_for(node(1), t0 + IDENTITY_RETRY, Duration::ZERO, true, false),
            IdentityRead::Full,
            "the retry"
        );
        c.finished(node(1), t0 + IDENTITY_RETRY, IdentityRead::Full, true);
        assert_eq!(
            c.read_for(
                node(1),
                t0 + IDENTITY_RETRY + Duration::from_secs(60),
                Duration::ZERO,
                true,
                false
            ),
            IdentityRead::Classify,
            "once a full read has answered"
        );
    }

    /// ADR-138 Increment 7. A node whose maker was known and is not any more — cleared by an
    /// operator, or reclassified (ADR-140) — is read in full on the first poll core asks about it,
    /// not at the end of the period its old schedule was on. The same holds each time core starts
    /// asking again.
    #[test]
    fn a_node_core_starts_asking_about_is_read_in_full_at_once() {
        let mut c = IdentityCadence::default();
        let t0 = Instant::now();
        let offset = Duration::from_secs(600);
        assert_eq!(
            c.read_for(node(1), t0, offset, false, false),
            IdentityRead::Skip
        );
        assert_eq!(
            c.read_for(node(1), t0 + offset, offset, false, false),
            IdentityRead::Full
        );
        c.finished(node(1), t0 + offset, IdentityRead::Full, true);

        let cleared = t0 + offset + Duration::from_secs(120);
        assert_eq!(
            c.read_for(node(1), cleared, offset, true, false),
            IdentityRead::Full,
            "the first poll after core starts asking"
        );
        c.finished(node(1), cleared, IdentityRead::Full, true);
        let next = cleared + Duration::from_secs(60);
        assert_eq!(
            c.read_for(node(1), next, offset, true, false),
            IdentityRead::Classify
        );

        // Classified, then cleared again.
        let known = next + Duration::from_secs(60);
        assert_eq!(
            c.read_for(node(1), known, offset, false, false),
            IdentityRead::Skip
        );
        let again = known + Duration::from_secs(60);
        assert_eq!(
            c.read_for(node(1), again, offset, true, false),
            IdentityRead::Full,
            "each time core starts asking"
        );
    }

    /// ADR-138 Increment 7. Only an answered full read moves the schedule; nothing else does.
    #[test]
    fn only_an_answered_full_read_moves_the_schedule() {
        let t0 = Instant::now();
        for (read, answered) in [
            (IdentityRead::Full, false),
            (IdentityRead::Classify, true),
            (IdentityRead::Classify, false),
            (IdentityRead::Skip, false),
        ] {
            let mut c = IdentityCadence::default();
            assert!(!c.claim(node(1), t0, Duration::ZERO));
            assert!(c.claim(node(1), t0, Duration::ZERO), "due");
            c.finished(node(1), t0, read, answered);
            assert!(
                c.claim(node(1), t0 + IDENTITY_RETRY, Duration::ZERO),
                "{read:?} answered={answered} left the retry in place"
            );
        }
        let mut c = IdentityCadence::default();
        assert!(!c.claim(node(1), t0, Duration::ZERO));
        assert!(c.claim(node(1), t0, Duration::ZERO));
        c.finished(node(1), t0, IdentityRead::Full, true);
        assert!(!c.claim(node(1), t0 + IDENTITY_RETRY, Duration::ZERO));
    }

    #[test]
    fn a_node_core_does_not_ask_about_keeps_the_offset_cadence() {
        let mut c = IdentityCadence::default();
        let t0 = Instant::now();
        let offset = Duration::from_secs(600);
        assert_eq!(
            c.read_for(node(1), t0, offset, false, false),
            IdentityRead::Skip,
            "first sight only schedules"
        );
        assert_eq!(
            c.read_for(node(1), t0 + offset, offset, false, false),
            IdentityRead::Full
        );
        assert_eq!(
            c.read_for(node(2), t0, offset, false, true),
            IdentityRead::Full,
            "a poll now reads in full"
        );
        assert_eq!(
            c.read_for(node(3), t0, offset, true, true),
            IdentityRead::Full,
            "a poll now reads in full whether or not core asked"
        );
    }

    #[test]
    fn only_the_scalar_snmp_get_carries_the_probe() {
        assert!(!carries_identity_probe(&testkit::icmp_job().check));
    }
}
