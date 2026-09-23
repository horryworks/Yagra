// SPDX-License-Identifier: AGPL-3.0-only
//! What each of a Meraki organization's two collect lanes sends next (ADR-169).
//!
//! **Why this is not in `run_meraki_scheduler`.** The loop in `main.rs` reads three stores and
//! publishes to the bus, so no test ever drove it — and the order it chose collects in was the
//! thing that went wrong: availability, due every 60 s on a lab deployment, ran 185 times in six
//! hours instead of 360, because a switch-port collect or an SSID read held the organization's one
//! flight for minutes. This module holds every decision that loop makes and nothing it reaches,
//! so [`MerakiSchedule::plan`] can be run against a clock a test controls.
//!
//! **The lanes** ([`MerakiLane`]): the fast one carries availability, uplink, traffic and the
//! wireless rounds; the slow one carries the switch ports, the SSID read and the periodic inventory
//! sync. Each runs one collect at a time.
//!
//! 🚨 **The SSID read is a job of its own, and publishes no client count or utilization**
//! (決定 2, `MerakiCollectCheck::ssid_only`). Two designs were measured and dropped on the way:
//! sent as an *extra* wireless round, it adds a sample between two rounds — VictoriaMetrics
//! estimates a series' interval from its samples, a short interval shrinks the estimate, and the
//! ordinary interval is then drawn as a gap, the dotted line this whole change exists to remove;
//! sent *in place of* a round, it drags the rounds' timing wherever the slow lane happens to be
//! free, and the simulation below read 210, 270, 330 and 45 s intervals where 300 was due. On its
//! own, the rounds keep their cadence and the read goes wherever the switch ports leave room.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use uuid::Uuid;
use yagra_common::MerakiTier;

use crate::meraki::{
    pool_can_run, port_names_due, ssid_statuses_due, MerakiLane, MerakiOrg, PoolCaps, SlowReads,
    SSID_STATUSES_EVERY,
};

/// How long an SSID read holds the slow lane, with room to spare: about 80 s measured on a real
/// organization of 1,710 access points, 85–90 s on the lab deployment (決定 2). The read starts
/// only when the switch ports are not due for at least this long, so it ends before they are —
/// measured on the lab deployment, the read going first made every fourth switch-port interval
/// 360 s, which a chart draws as a gap.
pub const SSID_HOLD: Duration = Duration::from_secs(120);

/// How long past its twenty minutes a due SSID read waits for the switch ports to leave it room
/// ([`SSID_HOLD`]) before it goes as soon as the slow lane is free, room or not — a switch-port
/// collect slower than its interval minus [`SSID_HOLD`] never leaves any. 1,200 + 450 + one read
/// (about 90 s) stays inside the thirty minutes a latest value is looked back for
/// (`store.rs::latest_query`), so the SSID count never goes blank.
pub const SSID_DEFER: Duration = Duration::from_secs(450);

/// One collect the scheduler has decided to send.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MerakiWork {
    pub tier: MerakiTier,
    pub slow: SlowReads,
}

impl MerakiWork {
    /// The lane this collect runs in — decided by what it reads, never chosen separately.
    #[must_use]
    pub fn lane(&self) -> MerakiLane {
        MerakiLane::of(self.tier, self.slow)
    }

    /// The SSID read on its own (決定 2) — a wireless job, but not a wireless round.
    fn is_ssid_read(&self) -> bool {
        self.tier == MerakiTier::Wireless && self.slow.ssid_statuses
    }
}

/// What each lane sends this tick. A lane that is busy, or has nothing due, sends nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct LanePlan {
    pub fast: Option<MerakiWork>,
    pub slow: Option<MerakiWork>,
}

impl LanePlan {
    /// Both lanes' work, slow first. The order is not a priority — each lane is independent — but
    /// it is the one a test can rely on.
    pub fn works(&self) -> impl Iterator<Item = MerakiWork> {
        self.slow.into_iter().chain(self.fast)
    }
}

/// When each organization's tiers and slow reads last had their turn, and when core last counted a
/// failure of its own against a tier. Lives in the scheduler's loop; lost on restart, which makes
/// everything due at once — and the fast lane then starts with availability (決定 5).
#[derive(Debug, Default)]
pub struct MerakiSchedule {
    last: HashMap<(Uuid, MerakiTier), Instant>,
    // When each organization's switch-port collect last asked for the ports' names (ADR-167 決定 1).
    port_names_at: HashMap<Uuid, Instant>,
    // When each organization's SSIDs and radio settings were last read (ADR-168 決定 1).
    ssid_statuses_at: HashMap<Uuid, Instant>,
    // When a tier was last counted as failed for a reason **core itself** knows about — its key
    // could not be opened, its imported devices could not be read, or which networks it watches
    // could not be read. No job is sent for any of the three, so no poller can report them (ADR-164
    // 決定 18). The tier stays due and is retried every tick, but it is *counted* once per cadence
    // (`meraki_health::count_once_per_cadence`), or three ticks of 15 s would read as three failed
    // collects.
    core_failures: HashMap<(Uuid, MerakiTier), Instant>,
}

impl MerakiSchedule {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// What `org`'s lanes send at `now`. `free` says whether a lane may take a collect (no
    /// unexpired flight holds it); `caps` which tiers the pool can run at all.
    ///
    /// - **Slow lane**: a due switch-port collect; else a due SSID read, where the switch ports are
    ///   not due for [`SSID_HOLD`] — or anywhere, once it has waited [`SSID_DEFER`]. The switch
    ///   ports go first because a late one is a gap in every port's traffic chart, while a late
    ///   SSID read only waits: its values are looked back for thirty minutes. (The periodic
    ///   inventory sync takes this lane too, from its own loop — `free` says so.)
    /// - **Fast lane**: the **first** due tier in [`MerakiOrg::active_tiers`]' order — availability,
    ///   uplink, wireless, traffic. 🚨 Not the most overdue one, which is what the single lane
    ///   picked: measured on the simulation below, a traffic collect (300 s, so 300 s "overdue"
    ///   when due) then went ahead of availability (60 s) and availability waited two ticks.
    ///   Everything in this lane takes seconds, so a tier that waits a tick behind one before it
    ///   loses 15 s, never a cadence. Availability being first is also what makes a restart count
    ///   core's own failures against it (ADR-164 決定 18).
    ///   ⚠️ Nothing else the scheduler sends may take this lane. The periodic inventory sync did,
    ///   from its own loop, and delayed availability by one tick on every run — a 75 s interval at a
    ///   60 s cadence, which a chart draws as a gap (VictoriaMetrics: about 1.125× the interval). It
    ///   is in the slow lane now (`meraki_sync::MerakiSync::sync_org_scheduled`). The one exception
    ///   is an operator's "Sync now" while the slow lane is busy (ADR-169 決定 1), and that sync
    ///   reads no network's LAN side here (ADR-164 決定 29), so it stays a few seconds.
    #[must_use]
    pub fn plan(
        &self,
        org: &MerakiOrg,
        now: Instant,
        caps: PoolCaps,
        free: impl Fn(MerakiLane) -> bool,
    ) -> LanePlan {
        let tiers: Vec<MerakiTier> = org
            .active_tiers()
            .into_iter()
            .filter(|t| pool_can_run(*t, caps))
            .collect();
        let mut plan = LanePlan::default();
        if free(MerakiLane::Slow) {
            plan.slow = self.slow_work(org, &tiers, now);
        }
        if free(MerakiLane::Fast) {
            plan.fast = tiers
                .iter()
                .copied()
                .find(|&tier| {
                    MerakiLane::of(tier, SlowReads::default()) == MerakiLane::Fast
                        && self.is_due(org, tier, now)
                })
                .map(|tier| MerakiWork {
                    tier,
                    slow: SlowReads::default(),
                });
        }
        plan
    }

    fn slow_work(&self, org: &MerakiOrg, tiers: &[MerakiTier], now: Instant) -> Option<MerakiWork> {
        let switch_ports = tiers.contains(&MerakiTier::SwitchPorts);
        if switch_ports && self.is_due(org, MerakiTier::SwitchPorts, now) {
            return Some(MerakiWork {
                tier: MerakiTier::SwitchPorts,
                slow: SlowReads {
                    port_names: port_names_due(self.port_names_at.get(&org.id).copied(), now),
                    ssid_statuses: false,
                },
            });
        }
        if !tiers.contains(&MerakiTier::Wireless) {
            return None;
        }
        let read_at = self.ssid_statuses_at.get(&org.id).copied();
        if !ssid_statuses_due(read_at, now) {
            return None;
        }
        let room = !(switch_ports && self.due_within(org, MerakiTier::SwitchPorts, SSID_HOLD, now));
        let waited_out = read_at
            .map(|at| now.duration_since(at))
            .is_none_or(|e| e >= SSID_STATUSES_EVERY + SSID_DEFER);
        (room || waited_out).then_some(MerakiWork {
            tier: MerakiTier::Wireless,
            slow: SlowReads {
                port_names: false,
                ssid_statuses: true,
            },
        })
    }

    /// Whether `tier` is due: its cadence has passed since its last turn, or it never had one.
    fn is_due(&self, org: &MerakiOrg, tier: MerakiTier, now: Instant) -> bool {
        self.due_within(org, tier, Duration::ZERO, now)
    }

    /// Whether `tier` falls due within `window` of `now` — or already has, or never had a turn.
    fn due_within(
        &self,
        org: &MerakiOrg,
        tier: MerakiTier,
        window: Duration,
        now: Instant,
    ) -> bool {
        let cadence = Duration::from_secs(u64::from(org.tier_cadence(tier)));
        self.last
            .get(&(org.id, tier))
            .is_none_or(|&at| now.duration_since(at) + window >= cadence)
    }

    /// `work` had its turn at `now`: it was published — or there was nothing to send it about
    /// (no imported device of its kind), which must count as a turn too, or it stays "never had
    /// one", and is picked on every tick ahead of the work that does have something to ask
    /// (ADR-167 決定 10; for the SSID read, ADR-169 決定 3). Never call it for a publish that
    /// failed.
    ///
    /// The SSID read moves its own clock and not the wireless rounds' — it is not a round (決定 2).
    pub fn dispatched(&mut self, org: Uuid, work: &MerakiWork, now: Instant) {
        if work.is_ssid_read() {
            self.ssid_statuses_at.insert(org, now);
            return;
        }
        self.last.insert((org, work.tier), now);
        if work.slow.port_names {
            self.port_names_at.insert(org, now);
        }
    }

    /// Whether a failure core found itself for `(org, tier)` should be counted now — at most once
    /// per `cadence` ([`crate::meraki_health::count_once_per_cadence`]). Both lanes can pick the
    /// same tier on different ticks — a wireless round and an SSID read — and they share this
    /// record, so it is still counted once.
    pub fn count_core_failure(
        &mut self,
        org: Uuid,
        tier: MerakiTier,
        cadence: Duration,
        now: Instant,
    ) -> bool {
        crate::meraki_health::count_once_per_cadence(
            &mut self.core_failures,
            org,
            tier,
            cadence,
            now,
        )
    }

    /// `(org, tier)` got past everything core checks before a job is sent.
    pub fn clear_core_failure(&mut self, org: Uuid, tier: MerakiTier) {
        self.core_failures.remove(&(org, tier));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::meraki::{MerakiInflight, PORT_NAMES_EVERY};

    const ALL_CAPS: PoolCaps = PoolCaps {
        switch_ports: true,
        wireless: true,
    };

    fn org(tiers: &[&str]) -> MerakiOrg {
        MerakiOrg {
            id: Uuid::from_u128(0x169),
            org_id: "123456".into(),
            name: "Acme".into(),
            base_url: "https://api.meraki.com".into(),
            credential_id: Uuid::nil(),
            availability_secs: 60,
            uplink_secs: 60,
            traffic_secs: 300,
            inventory_secs: 300,
            switch_ports_secs: 300,
            wireless_secs: 300,
            enabled_tiers: tiers.iter().map(|t| (*t).to_owned()).collect(),
            target_rps: 2.0,
            group_id: None,
            enabled: true,
            last_sync_at: None,
            last_sync_ok: None,
            last_sync_error: None,
            import_devices: true,
            file_by_prefix: true,
            max_devices: 1000,
            devices_over_cap: 0,
            collect_failures: Vec::new(),
            full_sync_requested_at: None,
            full_sync: None,
        }
    }

    fn every_tier() -> MerakiOrg {
        org(&[
            "availability",
            "uplink",
            "traffic",
            "switch_ports",
            "wireless",
        ])
    }

    fn both_free(_: MerakiLane) -> bool {
        true
    }

    fn work(tier: MerakiTier) -> MerakiWork {
        MerakiWork {
            tier,
            slow: SlowReads::default(),
        }
    }

    const SSID_ROUND: MerakiWork = MerakiWork {
        tier: MerakiTier::Wireless,
        slow: SlowReads {
            port_names: false,
            ssid_statuses: true,
        },
    };

    /// 決定 5 / ADR-164 決定 18: after a restart everything is due at once; the fast lane starts
    /// with availability — the one tier whose failures raise the organization's alert — whatever
    /// order the tiers were stored in, and the slow work does not delay it.
    #[test]
    fn a_restart_puts_availability_first_in_the_fast_lane() {
        let s = MerakiSchedule::new();
        let o = org(&[
            "wireless",
            "switch_ports",
            "traffic",
            "uplink",
            "availability",
        ]);
        let plan = s.plan(&o, Instant::now(), ALL_CAPS, both_free);
        assert_eq!(plan.fast, Some(work(MerakiTier::Availability)));
        assert_eq!(
            plan.slow.map(|w| w.tier),
            Some(MerakiTier::SwitchPorts),
            "the slow lane starts with the switch ports, whose lateness is a gap in a chart"
        );
    }

    /// 決定 2: the SSID read is a slow-lane job of its own, and it moves only its own clock — the
    /// wireless rounds keep theirs, so no round comes early or late because of it.
    #[test]
    fn the_ssid_read_is_a_job_of_its_own_and_leaves_the_wireless_rounds_alone() {
        let mut s = MerakiSchedule::new();
        let o = every_tier();
        let t0 = Instant::now();
        for tier in [
            MerakiTier::Availability,
            MerakiTier::Uplink,
            MerakiTier::Traffic,
            MerakiTier::SwitchPorts,
            MerakiTier::Wireless,
        ] {
            s.dispatched(o.id, &work(tier), t0);
        }
        let t1 = t0 + Duration::from_secs(15);
        let plan = s.plan(&o, t1, ALL_CAPS, both_free);
        assert_eq!(plan.slow, Some(SSID_ROUND));
        assert_eq!(plan.fast, None, "nothing fast is due yet");
        assert_eq!(SSID_ROUND.lane(), MerakiLane::Slow);
        s.dispatched(o.id, &SSID_ROUND, t1);

        // The round is due 300 s after the last round, not after the SSID read.
        let wireless_at = |s: &mut MerakiSchedule, t: Instant| {
            for tier in [
                MerakiTier::Availability,
                MerakiTier::Uplink,
                MerakiTier::Traffic,
                MerakiTier::SwitchPorts,
            ] {
                s.dispatched(o.id, &work(tier), t);
            }
            s.plan(&o, t, ALL_CAPS, both_free)
                .works()
                .find(|w| w.tier == MerakiTier::Wireless)
        };
        assert_eq!(
            wireless_at(&mut s, t0 + Duration::from_secs(300)),
            Some(work(MerakiTier::Wireless)),
            "the SSID read moved the wireless rounds"
        );
    }

    /// 決定 2: where the switch ports never leave room — a switch-port collect slower than its
    /// interval minus SSID_HOLD — the read waits SSID_DEFER and then goes anyway, before the
    /// SSID values it keeps on screen have expired.
    #[test]
    fn an_ssid_read_that_finds_no_room_goes_before_its_values_expire() {
        let mut s = MerakiSchedule::new();
        let o = every_tier();
        let t0 = Instant::now();
        s.dispatched(o.id, &SSID_ROUND, t0);
        // Due, and the switch ports are due in 100 s: no room.
        let t1 = t0 + SSID_STATUSES_EVERY;
        s.dispatched(
            o.id,
            &work(MerakiTier::SwitchPorts),
            t1 - Duration::from_secs(200),
        );
        assert_eq!(s.plan(&o, t1, ALL_CAPS, both_free).slow, None);
        // Still no room at the end of the wait — it goes anyway.
        let t2 = t0 + SSID_STATUSES_EVERY + SSID_DEFER;
        s.dispatched(
            o.id,
            &work(MerakiTier::SwitchPorts),
            t2 - Duration::from_secs(250),
        );
        assert_eq!(
            s.plan(&o, t2, ALL_CAPS, both_free).slow,
            Some(SSID_ROUND),
            "the SSID read waited past the point its values expire"
        );
        assert!(
            SSID_STATUSES_EVERY + SSID_DEFER + Duration::from_secs(90) < Duration::from_secs(1800),
            "a read that has waited this long, and then takes 90 s, lets the SSID count go blank"
        );
    }

    /// 決定 3: an organization with no access point has its wireless rounds skipped for want of a
    /// device. Each skip is a turn — or the SSID read, never made, stays the most overdue work of
    /// all and holds the slow lane on every tick ahead of the switch ports.
    #[test]
    fn an_organization_without_access_points_does_not_hold_the_slow_lane() {
        let mut s = MerakiSchedule::new();
        let o = every_tier();
        let t0 = Instant::now();
        s.dispatched(o.id, &work(MerakiTier::SwitchPorts), t0);
        assert_eq!(s.plan(&o, t0, ALL_CAPS, both_free).slow, Some(SSID_ROUND));
        // The scheduler finds no access point to send it about, and records the turn.
        s.dispatched(o.id, &SSID_ROUND, t0);
        for step in 1..=19u64 {
            let now = t0 + Duration::from_secs(step * 15);
            assert_ne!(
                s.plan(&o, now, ALL_CAPS, both_free).slow,
                Some(SSID_ROUND),
                "an SSID read with nothing to read asked for the slow lane again at +{} s",
                step * 15
            );
        }
        assert_eq!(
            s.plan(&o, t0 + Duration::from_secs(300), ALL_CAPS, both_free)
                .slow
                .map(|w| w.tier),
            Some(MerakiTier::SwitchPorts)
        );
    }

    /// 決定 2: an SSID read never starts where it would make the switch ports late — within
    /// [`SSID_HOLD`] of their next collect it waits — and they are never kept waiting for it.
    #[test]
    fn the_ssid_read_does_not_start_where_the_switch_ports_would_wait_for_it() {
        let mut s = MerakiSchedule::new();
        let o = every_tier();
        let t0 = Instant::now();
        s.dispatched(o.id, &work(MerakiTier::SwitchPorts), t0);
        s.dispatched(o.id, &SSID_ROUND, t0);
        s.dispatched(
            o.id,
            &work(MerakiTier::SwitchPorts),
            t0 + SSID_STATUSES_EVERY,
        );
        // Twenty minutes on, the read and a round are due — but the switch ports are due in
        // less than SSID_HOLD: the round goes fast, and the slow lane waits for the ports.
        let near = t0 + SSID_STATUSES_EVERY + Duration::from_secs(300) - SSID_HOLD;
        let plan = s.plan(&o, near, ALL_CAPS, both_free);
        assert_eq!(plan.slow, None);
        assert_eq!(plan.fast.map(|w| w.tier), Some(MerakiTier::Availability));
        assert!(
            plan.works().all(|w| !w.slow.ssid_statuses),
            "the SSID read started where the switch ports would wait for it"
        );
        // Both due at once: the switch ports first.
        let both = t0 + SSID_STATUSES_EVERY + Duration::from_secs(300);
        assert_eq!(
            s.plan(&o, both, ALL_CAPS, both_free).slow.map(|w| w.tier),
            Some(MerakiTier::SwitchPorts)
        );
    }

    /// ADR-167 決定 9, ADR-168 決定 7: a tier the pool cannot run is never offered — in either lane —
    /// and the tiers it can run carry on.
    #[test]
    fn a_tier_the_pool_cannot_run_is_offered_in_neither_lane() {
        let s = MerakiSchedule::new();
        let o = every_tier();
        let now = Instant::now();
        let old = PoolCaps::default();
        let plan = s.plan(&o, now, old, both_free);
        assert_eq!(plan.slow, None, "an old pool was offered slow work");
        assert_eq!(plan.fast, Some(work(MerakiTier::Availability)));
        for w in [plan.fast, plan.slow].into_iter().flatten() {
            assert!(pool_can_run(w.tier, old), "{w:?}");
        }

        let no_wireless = PoolCaps {
            switch_ports: true,
            wireless: false,
        };
        let plan = s.plan(&o, now, no_wireless, both_free);
        assert_eq!(plan.slow.map(|w| w.tier), Some(MerakiTier::SwitchPorts));
        assert!(plan.works().all(|w| w.tier != MerakiTier::Wireless));
    }

    /// A lane that is not free is sent nothing, and the other one carries on.
    #[test]
    fn a_busy_lane_is_sent_nothing() {
        let s = MerakiSchedule::new();
        let o = every_tier();
        let now = Instant::now();
        let plan = s.plan(&o, now, ALL_CAPS, |lane| lane == MerakiLane::Slow);
        assert_eq!(plan.fast, None);
        assert!(plan.slow.is_some());
        let plan = s.plan(&o, now, ALL_CAPS, |lane| lane == MerakiLane::Fast);
        assert_eq!(plan.slow, None);
        assert_eq!(
            plan.fast,
            Some(work(MerakiTier::Availability)),
            "the fast lane waited for a slow one"
        );
    }

    /// A lane is given only work that belongs in it, and the two never carry the same tier.
    #[test]
    fn each_lane_is_given_only_its_own_work() {
        let mut s = MerakiSchedule::new();
        let o = every_tier();
        let t0 = Instant::now();
        for step in 0..200u64 {
            let now = t0 + Duration::from_secs(step * 15);
            let plan = s.plan(&o, now, ALL_CAPS, both_free);
            if let Some(w) = plan.fast {
                assert_eq!(w.lane(), MerakiLane::Fast, "{w:?}");
            }
            if let Some(w) = plan.slow {
                assert_eq!(w.lane(), MerakiLane::Slow, "{w:?}");
            }
            if let (Some(f), Some(sl)) = (plan.fast, plan.slow) {
                assert_ne!(f.tier, sl.tier, "both lanes were sent {:?}", f.tier);
            }
            for w in plan.works() {
                s.dispatched(o.id, &w, now);
            }
        }
    }

    /// Both lanes can pick the wireless tier on different ticks; a failure core finds itself is
    /// still counted once per cadence, not once per lane.
    #[test]
    fn a_core_failure_is_counted_once_when_both_lanes_pick_wireless() {
        let mut s = MerakiSchedule::new();
        let org = Uuid::from_u128(1);
        let now = Instant::now();
        let cadence = Duration::from_secs(300);
        assert!(s.count_core_failure(org, MerakiTier::Wireless, cadence, now));
        assert!(!s.count_core_failure(
            org,
            MerakiTier::Wireless,
            cadence,
            now + Duration::from_secs(15)
        ));
        assert!(s.count_core_failure(org, MerakiTier::Wireless, cadence, now + cadence));
        s.clear_core_failure(org, MerakiTier::Wireless);
        assert!(s.count_core_failure(
            org,
            MerakiTier::Wireless,
            cadence,
            now + cadence + Duration::from_secs(15)
        ));
    }

    /// How long each collect holds its lane, as measured on the lab deployment (a mock replaying a
    /// recorded organization with the recording's delays): availability 2–4 s, uplink about 8 s at
    /// half the rate, the switch ports about 140 s (220 s with the names), a wireless round about
    /// 5 s (90 s with the SSIDs), the inventory sync about 9 s at half the rate.
    fn holds(w: &MerakiWork) -> Duration {
        Duration::from_secs(match (w.tier, w.slow.port_names, w.slow.ssid_statuses) {
            (MerakiTier::SwitchPorts, true, _) => 220,
            (MerakiTier::SwitchPorts, false, _) => 140,
            (MerakiTier::Wireless, _, true) => 90,
            (MerakiTier::Wireless, _, false) => 5,
            (MerakiTier::Uplink, _, _) => 8,
            (MerakiTier::Availability, _, _) => 4,
            (MerakiTier::Traffic, _, _) => 2,
            (MerakiTier::Inventory, _, _) => 9,
        })
    }

    /// 🚨 **The measurement this ADR was made from, as a test.** Three simulated hours of the lab
    /// organization's settings (availability and uplink every 60 s, the rest every 300 s), with the
    /// real scheduler, the real flight tracker and the inventory sync taking the fast lane every
    /// five minutes; each collect holds its lane for as long as it was measured to. Before ADR-169
    /// the same organization ran availability 185 times in six hours instead of 360.
    #[test]
    fn three_simulated_hours_keep_every_tier_on_cadence() {
        const TICK: Duration = Duration::from_secs(15);
        const LEASE: Duration = Duration::from_secs(300);
        let o = every_tier();
        let t0 = Instant::now();
        let mut s = MerakiSchedule::new();
        let f = MerakiInflight::new();
        let mut running: Vec<(Instant, Uuid)> = Vec::new(); // (ends, job)
        let mut sent: HashMap<&'static str, Vec<u64>> = HashMap::new();
        let mut next_sync = t0 + Duration::from_secs(7);
        let mut job = 0u128;
        let at = |now: Instant| now.duration_since(t0).as_secs();

        for step in 0..(3 * 3600 / 15) {
            let now = t0 + TICK * step;
            running.retain(|&(ends, id)| {
                if ends <= now {
                    f.complete(id);
                    false
                } else {
                    true
                }
            });
            // The sync loop is its own task; it takes the slow lane when it is free (決定 1).
            if now >= next_sync {
                job += 1;
                let id = Uuid::from_u128(job);
                if f.acquire_sync(o.id, MerakiLane::Slow, id, LEASE, now) {
                    running.push((now + holds(&work(MerakiTier::Inventory)), id));
                    next_sync = now + Duration::from_secs(300);
                }
            }
            let plan = s.plan(&o, now, ALL_CAPS, |lane| !f.is_inflight(o.id, lane, now));
            for w in plan.works() {
                job += 1;
                let id = Uuid::from_u128(job);
                assert!(
                    f.acquire_collect(o.id, w.lane(), id, w.tier, LEASE, now),
                    "the plan offered {w:?} a lane that was taken"
                );
                running.push((now + holds(&w), id));
                s.dispatched(o.id, &w, now);
                let key = match (w.tier, w.slow.ssid_statuses) {
                    (MerakiTier::Wireless, true) => "ssid",
                    (tier, _) => tier.as_str(),
                };
                sent.entry(key).or_default().push(at(now));
                if w.slow.port_names {
                    sent.entry("port_names").or_default().push(at(now));
                }
                if w.tier == MerakiTier::Wireless && !w.is_ssid_read() {
                    // Every wireless round publishes the client count; the SSID read on its own
                    // publishes none (決定 2).
                    sent.entry("wireless_samples").or_default().push(at(now));
                }
            }
        }

        let gaps = |key: &str| -> Vec<u64> {
            let times = sent.get(key).unwrap_or_else(|| panic!("{key} never ran"));
            times.windows(2).map(|w| w[1] - w[0]).collect()
        };
        let max = |key: &str| gaps(key).into_iter().max().unwrap_or(0);
        let tick = TICK.as_secs();

        // VictoriaMetrics draws an interval over about 1.125× the usual one as a gap: 67.5 s at
        // 60 s, so at a 15 s tick availability may not be late at all.
        assert!(
            max("availability") <= 60,
            "availability waited {} s: {:?}",
            max("availability"),
            gaps("availability")
        );
        assert!(max("uplink") <= 60, "{:?}", gaps("uplink"));
        assert!(
            sent["availability"].len() >= 3 * 3600 / 75,
            "availability ran {} times in three hours",
            sent["availability"].len()
        );
        // No wireless sample arrives late enough to be drawn as a gap (VictoriaMetrics: about 1.125×
        // the interval) — and none so early that it shrinks the interval the next one is judged by.
        // The first interval is the restart's: everything is due at once and the first rounds queue
        // behind each other and the sync, once (measured here: 345 s). After that the rounds keep
        // the phase they settled on.
        let wireless = gaps("wireless_samples");
        assert!(
            wireless.iter().skip(1).all(|&g| g <= 300 + tick),
            "a wireless round came late: {wireless:?}"
        );
        let short = wireless.iter().filter(|&&g| g < 300).count();
        assert!(
            short * 10 <= wireless.len(),
            "{short} of {} wireless intervals were short: {wireless:?}",
            wireless.len()
        );
        assert!(
            max("ssid") < 1800,
            "the SSID values went unread for {} s: {:?}",
            max("ssid"),
            gaps("ssid")
        );
        // The first interval is the restart's again (measured: 330 s, the SSID read and the sync
        // both being due at once). After it, never more than a tick: 315 s is under the 337.5 s a
        // chart would draw as a gap at a 300 s interval.
        let ports = gaps("switch_ports");
        assert!(
            ports.iter().skip(1).all(|&g| g <= 300 + tick),
            "the switch ports waited: {ports:?}"
        );
        assert!(
            sent["switch_ports"].len() * 300 >= 3 * 3600 * 3 / 4,
            "the switch ports ran {} times",
            sent["switch_ports"].len()
        );
        // The port names are still read about hourly: at once, then once per PORT_NAMES_EVERY.
        let names = gaps("port_names");
        assert_eq!(names.len(), 2, "{names:?}");
        assert!(
            names
                .iter()
                .all(|&g| g >= PORT_NAMES_EVERY.as_secs() && g <= PORT_NAMES_EVERY.as_secs() + 400),
            "{names:?}"
        );
    }
}
