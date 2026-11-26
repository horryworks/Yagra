// SPDX-License-Identifier: AGPL-3.0-only
//! Why an SNMP scalar GET produced no value — the reason behind `snmp_up = 0` (ADR-268).
//!
//! `snmp_up` says only that nothing came back. Two faults with different owners look the same on
//! that metric: a device whose SNMP is not answering at all, and an SNMPv3 device that now and then
//! misses the engine-discovery exchange every v3 request opens with. The poller knows which branch
//! it took, so it names it here and the token rides the poll result to core, which puts it on the
//! alert.
//!
//! On the bus the reason travels as the token string rather than this enum, the way
//! `MerakiCollectReport::failure` does: a core that does not know a newer token loses the reason,
//! never the result it rides on.

use serde::{Deserialize, Serialize};

/// One stage at which an SNMP scalar GET came back empty.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum SnmpFailure {
    /// SNMPv3: the engine-discovery exchange every v3 session opens with got no answer in time.
    HandshakeTimeout,
    /// SNMPv3: the engine-discovery exchange was answered with an error (an authentication
    /// complaint, a report PDU, an engine the device would not agree to).
    HandshakeRefused,
    /// The request got no answer at all. On v2c a wrong community looks like this too: the agent
    /// drops the packet without saying anything.
    NoAnswer,
    /// SNMPv3: the session opened, and every request was answered with an error rather than a
    /// value (a report PDU, an authentication failure).
    Refused,
    /// The agent answered, and none of the objects asked for exist on it (`noSuchObject` and the
    /// like) — a healthy agent and a profile whose scalars this device does not implement.
    NoValues,
    /// The request could not be sent: the socket would not open.
    SendFailed,
}

impl SnmpFailure {
    /// Every reason, for the UI's per-reason strings and any full tally.
    pub const ALL: [SnmpFailure; 6] = [
        Self::HandshakeTimeout,
        Self::HandshakeRefused,
        Self::NoAnswer,
        Self::Refused,
        Self::NoValues,
        Self::SendFailed,
    ];

    /// The stable snake_case token — the bus value, the JSON tag and the metric label.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::HandshakeTimeout => "handshake_timeout",
            Self::HandshakeRefused => "handshake_refused",
            Self::NoAnswer => "no_answer",
            Self::Refused => "refused",
            Self::NoValues => "no_values",
            Self::SendFailed => "send_failed",
        }
    }

    /// The inverse of [`Self::as_str`]: an exact token, or `None` for one this build does not
    /// know (a newer poller's), which costs only the reason.
    #[must_use]
    pub fn from_token(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|v| v.as_str() == s)
    }

    /// The reason in plain English, for a notification's details.
    #[must_use]
    pub const fn english(self) -> &'static str {
        match self {
            Self::HandshakeTimeout => "SNMPv3 handshake timed out",
            Self::HandshakeRefused => "SNMPv3 handshake refused",
            Self::NoAnswer => "No answer to the request",
            Self::Refused => "Answered with an error (authentication or access)",
            Self::NoValues => "Answered, but none of the assigned values exist",
            Self::SendFailed => "Could not send the request",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_reason_round_trips_through_its_token_and_through_serde() {
        for r in SnmpFailure::ALL {
            assert_eq!(SnmpFailure::from_token(r.as_str()), Some(r));
            let json = serde_json::to_string(&r).expect("serialize");
            assert_eq!(json, format!("\"{}\"", r.as_str()));
        }
        assert_eq!(SnmpFailure::from_token("a_newer_reason"), None);
    }
}
