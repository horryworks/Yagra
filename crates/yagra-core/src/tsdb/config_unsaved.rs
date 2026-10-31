// SPDX-License-Identifier: AGPL-3.0-only
//! Adding `config_unsaved_seconds` to a poll result before it is stored and judged (ADR-222).
//!
//! Two sources, one metric (decision 4):
//!
//! - **SNMP** ([`add_to`]): Cisco IOS answers when running last changed and when startup was last
//!   written. The arithmetic is `yagra_common::config_unsaved`; this file only finds its three
//!   inputs among a result's node-level samples. A result missing any of them gains nothing — no
//!   sample is a gap, never a "saved" reading, so an open alert is not closed by a poll that did
//!   not ask.
//! - **The command line** ([`UnsavedCache`]): for a model the save script compares (VRP), what it
//!   last saw is in `config_unsaved`, read on a timer and added to that node's SNMP scalar result
//!   (the one carrying `sysUpTime`), once per interval. A node the script never checked gains
//!   nothing, for the same reason.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use std::time::Duration;

use chrono::{DateTime, Utc};
use uuid::Uuid;
use yagra_bus::{PollResult, Sample};
use yagra_common::config_unsaved::{self, RUNNING_CHANGED, STARTUP_CHANGED, UPTIME};

/// How often [`run_refresh`] re-reads what the save script saw. The script runs once per fetch,
/// at most a few times a day per device, so a minute adds nothing an operator would notice.
const REFRESH: Duration = Duration::from_secs(60);

/// What the save script last saw per node: `Some(since)` while running and startup differ,
/// `None` while they agree. Absent: never checked.
#[derive(Default)]
pub(crate) struct UnsavedCache {
    nodes: RwLock<HashMap<Uuid, Option<DateTime<Utc>>>>,
}

impl UnsavedCache {
    fn replace(&self, nodes: HashMap<Uuid, Option<DateTime<Utc>>>) {
        if let Ok(mut held) = self.nodes.write() {
            *held = nodes;
        }
    }

    /// Append `config_unsaved_seconds` to a result of a node the script has checked. A result
    /// that already carries one (SNMP answered) keeps its own.
    ///
    /// Only the result carrying the node's `sysUpTime` gains it — the SNMP scalar poll, one per
    /// interval, the same result the SNMP source rides. The rule's dwell counts observations per
    /// check, so a reading added to every result of the node (ICMP, walks, …) would satisfy a
    /// dwell of two inside a single interval.
    pub(crate) fn add_to(&self, result: &mut PollResult, now: DateTime<Utc>) {
        if result
            .samples
            .iter()
            .any(|s| s.metric == config_unsaved::METRIC)
            || !result
                .samples
                .iter()
                .any(|s| s.ifindex.is_none() && s.metric == UPTIME)
        {
            return;
        }
        let Ok(held) = self.nodes.read() else {
            return;
        };
        if let Some(since) = held.get(&result.node_id.as_uuid()) {
            result.samples.push(Sample::gauge(
                config_unsaved::METRIC,
                seconds_since(*since, now),
            ));
        }
    }
}

/// The value for a node the script checked: 0 while saved.
fn seconds_since(since: Option<DateTime<Utc>>, now: DateTime<Utc>) -> f64 {
    since.map_or(0.0, |s| (now - s).num_seconds().max(0) as f64)
}

/// Keep `cache` current. A failed read keeps what it held: an empty map would read as "never
/// checked", which only stops the metric, but the last reading is still the better one.
pub(crate) async fn run_refresh(
    cache: Arc<UnsavedCache>,
    saves: Arc<yagra_base::config_save::ConfigSaveRepo>,
) {
    let mut every = tokio::time::interval(REFRESH);
    every.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        every.tick().await;
        match saves.unsaved_all().await {
            Ok(nodes) => cache.replace(nodes),
            Err(e) => tracing::warn!(error = %e, "config save: unsaved states could not be read"),
        }
    }
}

/// Append the computed sample to `result` when all three inputs are present.
pub(crate) fn add_to(result: &mut PollResult) {
    if let Some(sample) = sample_of(&result.samples) {
        result.samples.push(sample);
    }
}

fn sample_of(samples: &[Sample]) -> Option<Sample> {
    let node_level = |name: &str| {
        samples
            .iter()
            .find(|s| s.ifindex.is_none() && s.metric == name)
            .map(|s| s.value)
    };
    let running = node_level(RUNNING_CHANGED)?;
    let startup = node_level(STARTUP_CHANGED)?;
    let uptime = node_level(UPTIME)?;
    Some(Sample::gauge(
        config_unsaved::METRIC,
        config_unsaved::unsaved_seconds(uptime, running, startup),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn samples(pairs: &[(&str, f64)]) -> Vec<Sample> {
        pairs.iter().map(|(m, v)| Sample::gauge(*m, *v)).collect()
    }

    #[test]
    fn three_inputs_make_one_sample() {
        let got = sample_of(&samples(&[
            (UPTIME, 500_000.0),
            (RUNNING_CHANGED, 400_000.0),
            (STARTUP_CHANGED, 300_000.0),
        ]))
        .expect("a sample");
        assert_eq!(got.metric, config_unsaved::METRIC);
        assert_eq!(got.value, 1_000.0);
        assert!(got.ifindex.is_none());
    }

    fn result_of(node: Uuid, samples: Vec<Sample>) -> PollResult {
        let mut r = PollResult::new(
            Uuid::nil(),
            node.into(),
            0,
            yagra_bus::CheckOutcome::Reachable,
        );
        r.samples = samples;
        r
    }

    #[test]
    fn the_cache_adds_a_reading_only_for_a_checked_node_and_never_twice() {
        let checked = Uuid::new_v4();
        let saved = Uuid::new_v4();
        let cache = UnsavedCache::default();
        let now = Utc::now();
        cache.replace(HashMap::from([
            (checked, Some(now - chrono::Duration::seconds(90))),
            (saved, None),
        ]));
        let scalar = || vec![Sample::gauge(UPTIME, 1.0)];
        let mut r = result_of(checked, scalar());
        cache.add_to(&mut r, now);
        assert_eq!(r.samples.len(), 2);
        assert_eq!(r.samples[1].value, 90.0);
        let mut r = result_of(saved, scalar());
        cache.add_to(&mut r, now);
        assert_eq!(r.samples[1].value, 0.0);
        let mut r = result_of(Uuid::new_v4(), scalar());
        cache.add_to(&mut r, now);
        assert_eq!(r.samples.len(), 1);
        // SNMP already answered: its reading stands.
        let mut r = result_of(
            checked,
            vec![
                Sample::gauge(UPTIME, 1.0),
                Sample::gauge(config_unsaved::METRIC, 5.0),
            ],
        );
        cache.add_to(&mut r, now);
        assert_eq!(r.samples.len(), 2);
    }

    /// The node's other results (ICMP, a walk) gain nothing: the rule's dwell counts one reading
    /// per interval, not one per result.
    #[test]
    fn the_cache_adds_a_reading_only_to_the_result_carrying_sys_uptime() {
        let checked = Uuid::new_v4();
        let cache = UnsavedCache::default();
        let now = Utc::now();
        cache.replace(HashMap::from([(checked, Some(now))]));
        let mut icmp = result_of(checked, vec![]);
        cache.add_to(&mut icmp, now);
        assert!(icmp.samples.is_empty());
        let mut walk = result_of(checked, vec![Sample::gauge("if_hc_in_octets", 1.0)]);
        cache.add_to(&mut walk, now);
        assert_eq!(walk.samples.len(), 1);
    }

    #[test]
    fn a_missing_input_makes_none_rather_than_a_saved_reading() {
        assert!(sample_of(&samples(&[(UPTIME, 1.0), (RUNNING_CHANGED, 1.0)])).is_none());
        assert!(sample_of(&samples(&[(UPTIME, 1.0), (STARTUP_CHANGED, 1.0)])).is_none());
        assert!(sample_of(&samples(&[(RUNNING_CHANGED, 1.0), (STARTUP_CHANGED, 1.0)])).is_none());
    }
}
