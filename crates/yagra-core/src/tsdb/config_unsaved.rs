// SPDX-License-Identifier: AGPL-3.0-only
//! Adding `config_unsaved_seconds` to a poll result before it is stored and judged (ADR-222).
//!
//! The arithmetic is `yagra_common::config_unsaved`; this file only finds its three inputs among a
//! result's node-level samples. A result missing any of them gains nothing — no sample is a gap,
//! never a "saved" reading, so an open alert is not closed by a poll that did not ask.

use yagra_bus::{PollResult, Sample};
use yagra_common::config_unsaved::{self, RUNNING_CHANGED, STARTUP_CHANGED, UPTIME};

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

    #[test]
    fn a_missing_input_makes_none_rather_than_a_saved_reading() {
        assert!(sample_of(&samples(&[(UPTIME, 1.0), (RUNNING_CHANGED, 1.0)])).is_none());
        assert!(sample_of(&samples(&[(UPTIME, 1.0), (STARTUP_CHANGED, 1.0)])).is_none());
        assert!(sample_of(&samples(&[(RUNNING_CHANGED, 1.0), (STARTUP_CHANGED, 1.0)])).is_none());
    }
}
