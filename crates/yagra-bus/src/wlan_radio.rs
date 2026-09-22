// SPDX-License-Identifier: AGPL-3.0-only
//! One radio of one access point, as the AP node reads it: the samples keyed by the radio's slot,
//! and the `interfaces` row that slot stands for (ADR-064 R6/R9/増分 C, ADR-168 決定 6).
//!
//! Two paths build radios, on two sides of the bus. Core fans a wireless controller's AP walk out
//! to its AP nodes (`yagra-core`'s `wireless_fanout.rs`), and the poller turns a Meraki wireless
//! collect into results for the MR nodes (`yagra-poller`'s `worker/meraki.rs`). Both publish a
//! radio the same way — the same metric names, the same slot numbers, the same row — because that
//! is what lets one screen, one threshold rule and one ranking compare access points across vendors.
//! Written twice, the two would drift: the first thing to go would be the rule below about the
//! channel width, which is the kind of thing a second copy is written without.
//!
//! This lives in `yagra-bus` because both of those crates depend on it and on nothing else they
//! share: the types it builds ([`Sample`], [`DiscoveredInterface`]) are the bus's own.

use yagra_common::{
    IfIndex, MetricKind, WlanBand, WlanRadioObservation, METRIC_IF_HC_IN_OCTETS,
    METRIC_IF_HC_OUT_OCTETS, METRIC_IF_OPER_STATUS, METRIC_WLAN_RADIO_CHANNEL,
    METRIC_WLAN_RADIO_CHANNEL_UTIL_PCT, METRIC_WLAN_RADIO_CLIENT_COUNT,
    METRIC_WLAN_RADIO_CLIENT_SIGNAL_DBM, METRIC_WLAN_RADIO_INTERFERENCE_PCT,
    METRIC_WLAN_RADIO_NOISE_DBM, METRIC_WLAN_RADIO_NON_WIFI_UTIL_PCT,
    METRIC_WLAN_RADIO_TX_POWER_DBM,
};

use crate::{DiscoveredInterface, Sample};

/// IANAifType for a radio: `ieee80211(71)`. What lets a reader tell "this is not an Ethernet port"
/// from "we could not read it".
pub const IF_TYPE_IEEE80211: i32 = 71;

/// What one radio reported, in the form every path publishes it from.
///
/// ⚠️ **Its own type, not [`WlanRadioObservation`].** That one travels on the bus inside a
/// controller's inventory and holds whole percents (`u32`); the Meraki Dashboard reports
/// utilization to two decimals (0–86.22% on a real organization, most of them fractional).
/// Widening the bus type would make a core one release older refuse every inventory a newer poller
/// sends — a float cannot be read into a `u32` — so the bus type stays and this one takes fractions.
///
/// A reading the source did not have is `None`, never zero: a 0 dBm noise floor reads as a radio
/// being drowned, and 0% utilization as an idle channel (ADR-064 改訂 R10).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct RadioReadings {
    /// The slot this radio occupies on its AP node — its `ifindex` in every series and the row key
    /// of its `interfaces` row ([`yagra_common::assign_radio_slots`]).
    pub slot: u32,
    /// The band it is working in; `None` only where a caller has not decided it yet, which
    /// [`RadioReadings::interface`] then leaves unnamed rather than guessing.
    pub band: Option<WlanBand>,
    /// Up (`true`), down (`false`), or unknown (`None`). A Meraki radio is always `None`: nothing
    /// the Dashboard answers says whether a radio is on (ADR-168 決定 3).
    pub up: Option<bool>,
    /// Clients online through this radio.
    pub clients: Option<f64>,
    /// The working channel.
    pub channel: Option<u32>,
    /// Channel utilization, percent — everything that kept the channel busy.
    pub channel_util_pct: Option<f64>,
    /// The part of [`Self::channel_util_pct`] that was not Wi-Fi, percent (Meraki only).
    pub non_wifi_util_pct: Option<f64>,
    /// Interference ratio, percent (a Huawei controller's own measure).
    pub interference_pct: Option<f64>,
    /// Noise floor, dBm.
    pub noise_dbm: Option<f64>,
    /// Average client signal strength, dBm.
    pub client_signal_dbm: Option<f64>,
    /// Transmit power, dBm.
    pub tx_power_dbm: Option<f64>,
    /// Bytes received on the air interface, raw (ADR-012).
    pub in_octets: Option<u64>,
    /// Bytes sent on the air interface, raw (ADR-012).
    pub out_octets: Option<u64>,
}

impl From<&WlanRadioObservation> for RadioReadings {
    fn from(r: &WlanRadioObservation) -> Self {
        Self {
            slot: r.slot,
            band: Some(r.band),
            up: r.up,
            clients: r.clients.map(f64::from),
            channel: r.channel,
            channel_util_pct: r.channel_util_pct.map(f64::from),
            // A controller's walk has no such column; Huawei's interference ratio is a different
            // quantity and keeps its own name.
            non_wifi_util_pct: None,
            interference_pct: r.interference_pct.map(f64::from),
            noise_dbm: r.noise_dbm.map(f64::from),
            client_signal_dbm: r.client_signal_dbm.map(f64::from),
            tx_power_dbm: r.tx_power_dbm.map(f64::from),
            in_octets: r.in_octets,
            out_octets: r.out_octets,
        }
    }
}

impl RadioReadings {
    /// This radio's samples, keyed by its slot so the AP node reads it as a port (ADR-064 R6/R9).
    ///
    /// The traffic counters and the operational status go out under the **IF-MIB** names, which is
    /// what makes a radio draw on every screen that already draws a port — throughput, the up/down
    /// dot, the interface-tier threshold rules — without any of them learning what a radio is.
    #[must_use]
    pub fn samples(&self) -> Vec<Sample> {
        let slot = IfIndex(self.slot);
        let gauge = |metric: &'static str, value: Option<f64>| {
            value.map(move |v| Sample::interface(metric, slot, v, MetricKind::Gauge))
        };
        let mut out: Vec<Sample> = [
            gauge(METRIC_WLAN_RADIO_CLIENT_COUNT, self.clients),
            gauge(METRIC_WLAN_RADIO_CHANNEL_UTIL_PCT, self.channel_util_pct),
            gauge(METRIC_WLAN_RADIO_NON_WIFI_UTIL_PCT, self.non_wifi_util_pct),
            gauge(METRIC_WLAN_RADIO_INTERFERENCE_PCT, self.interference_pct),
            gauge(METRIC_WLAN_RADIO_NOISE_DBM, self.noise_dbm),
            gauge(METRIC_WLAN_RADIO_CLIENT_SIGNAL_DBM, self.client_signal_dbm),
            gauge(METRIC_WLAN_RADIO_TX_POWER_DBM, self.tx_power_dbm),
            gauge(METRIC_WLAN_RADIO_CHANNEL, self.channel.map(f64::from)),
            self.up.map(|up| {
                Sample::interface(
                    METRIC_IF_OPER_STATUS,
                    slot,
                    if up { 1.0 } else { 2.0 },
                    MetricKind::Gauge,
                )
            }),
        ]
        .into_iter()
        .flatten()
        .collect();
        #[allow(clippy::cast_precision_loss)]
        for (metric, value) in [
            (METRIC_IF_HC_IN_OCTETS, self.in_octets),
            (METRIC_IF_HC_OUT_OCTETS, self.out_octets),
        ] {
            if let Some(v) = value {
                out.push(Sample::interface(
                    metric,
                    slot,
                    v as f64,
                    MetricKind::Counter,
                ));
            }
        }
        out
    }

    /// The `interfaces` row this radio stands for.
    ///
    /// 🚨 **`if_speed` stays `None`, and the channel width must never be put there.** A radio
    /// reports a 20 MHz channel, which is not a line rate: stored as a speed it would make
    /// `if_in_util_pct` out of twenty bits per second, and every radio would read as thousands of
    /// percent utilised. `if_type` is IANAifType 71, `ieee80211`.
    ///
    /// The alias is the channel when this reading has one and `None` otherwise — which the
    /// multi-writer upsert reads as "keep what is stored", so a Meraki collect that read only the
    /// utilization this time leaves the channel the last full read wrote.
    #[must_use]
    pub fn interface(&self) -> DiscoveredInterface {
        DiscoveredInterface {
            ifindex: IfIndex(self.slot),
            if_name: self.band.map(|b| b.label().to_owned()),
            if_alias: self.channel.map(|c| format!("channel {c}")),
            if_speed: None,
            if_duplex: None,
            if_type: Some(IF_TYPE_IEEE80211),
            // A radio has no pluggable and no optical window; every one of these is a question
            // about a wired port, and answering it would be inventing an answer.
            if_media: None,
            transceiver_model: None,
            rx_power_low_dbm: None,
            rx_power_high_dbm: None,
            tx_power_low_dbm: None,
            tx_power_high_dbm: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(samples: &[Sample], metric: &str) -> Option<(f64, MetricKind)> {
        samples
            .iter()
            .find(|s| s.metric == metric)
            .map(|s| (s.value, s.kind))
    }

    /// A controller's radio arrives exactly as it did before this module existed: the same names,
    /// the same slot, the status and the traffic under the IF-MIB names.
    #[test]
    fn a_walked_radio_publishes_what_it_always_did() {
        let walked = WlanRadioObservation {
            slot: 2,
            band: WlanBand::Band5G,
            up: Some(false),
            clients: Some(7),
            channel: Some(44),
            channel_util_pct: Some(14),
            interference_pct: Some(3),
            noise_dbm: Some(-96),
            client_signal_dbm: Some(-61),
            tx_power_dbm: Some(17),
            in_octets: Some(2_555_209_504),
            out_octets: None,
        };
        let r = RadioReadings::from(&walked);
        let s = r.samples();
        assert!(s.iter().all(|x| x.ifindex == Some(IfIndex(2))));
        assert_eq!(
            at(&s, "wlan_radio_client_count"),
            Some((7.0, MetricKind::Gauge))
        );
        assert_eq!(
            at(&s, "wlan_radio_noise_dbm"),
            Some((-96.0, MetricKind::Gauge))
        );
        assert_eq!(at(&s, "if_oper_status"), Some((2.0, MetricKind::Gauge)));
        assert_eq!(
            at(&s, "if_hc_in_octets"),
            Some((2_555_209_504.0, MetricKind::Counter))
        );
        assert_eq!(at(&s, "if_hc_out_octets"), None, "absent is not zero");
        assert_eq!(
            at(&s, "wlan_radio_non_wifi_util_pct"),
            None,
            "a controller's walk has no such column"
        );
        assert_eq!(s.len(), 9);

        let row = r.interface();
        assert_eq!(row.ifindex, IfIndex(2));
        assert_eq!(row.if_name.as_deref(), Some("5 GHz"));
        assert_eq!(row.if_alias.as_deref(), Some("channel 44"));
        assert_eq!(row.if_type, Some(IF_TYPE_IEEE80211));
        assert_eq!(
            row.if_speed, None,
            "a channel width is not a line rate and must never become one"
        );
    }

    /// ADR-168: a Meraki radio's utilization keeps its fraction, carries the non-Wi-Fi part under
    /// its own name, and says nothing about whether the radio is on.
    #[test]
    fn a_meraki_radio_keeps_its_fractions_and_claims_no_status() {
        let r = RadioReadings {
            slot: 1,
            band: Some(WlanBand::Band2G4),
            channel_util_pct: Some(37.25),
            non_wifi_util_pct: Some(0.5),
            ..RadioReadings::default()
        };
        let s = r.samples();
        assert_eq!(
            at(&s, "wlan_radio_channel_util_pct"),
            Some((37.25, MetricKind::Gauge))
        );
        assert_eq!(
            at(&s, "wlan_radio_non_wifi_util_pct"),
            Some((0.5, MetricKind::Gauge))
        );
        assert_eq!(at(&s, "if_oper_status"), None);
        assert_eq!(s.len(), 2);
        // No channel this time: the alias is left for the stored one to stand.
        assert_eq!(r.interface().if_alias, None);
        assert_eq!(r.interface().if_name.as_deref(), Some("2.4 GHz"));
    }
}
