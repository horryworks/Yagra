// SPDX-License-Identifier: AGPL-3.0-only
//! What a node *is*, and therefore how it gets polled.
//!
//! A node's kind is not a stored column — it is derived from which single-purpose side table
//! carries a row for it (`meraki_devices` / `url_checks` / `dns_checks`), with an ordinary device
//! as the fallthrough. Deriving it is cheap; deriving it *consistently* is what needed a type.

use serde::{Deserialize, Serialize};

/// A node's monitoring kind: the thing that decides which poll jobs it produces.
///
/// Variants are declared in **precedence order**, which is the order [`NodeKind::resolve`] applies.
/// A node should carry at most one single-purpose row — the API edge refuses the second — but a row
/// can predate that guard, so resolution has to be deterministic rather than depend on which lookup
/// happened to run first.
///
/// Before this type the same question was answered in three places that could disagree: the
/// scheduler's Meraki short-circuit, the scheduler's URL-beats-DNS resolution, and the node-detail
/// API — which applied no precedence at all and handed the UI every config it found, so a node the
/// scheduler polled as Meraki could render a URL-monitor health card next to it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum NodeKind {
    /// Bound to a Cisco Meraki device: polled by the org collector, so it emits **no** per-node job.
    Meraki,
    /// An HTTP(S) endpoint monitor: one HTTP job and no ICMP — a URL target may be unpingable
    /// (behind a CDN), and SNMP does not apply.
    Url,
    /// A DNS name-resolution monitor (ADR-033): one DNS job. A name has no address of its own.
    Dns,
    /// An ordinary device: ICMP liveness always, plus SNMP when a credential and collection set
    /// resolve. The fallthrough — a node is this whenever it is nothing more specific.
    Device,
}

/// Which single-purpose rows a node carries, as the input to [`NodeKind::resolve`].
///
/// Named fields rather than three positional `bool`s: the arguments are otherwise
/// indistinguishable at a call site, and getting two of them the wrong way round is a silent
/// misclassification, not a type error. Adding a kind adds a field here and an arm in `resolve`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct NodeRows {
    /// The node has a `meraki_devices` row.
    pub meraki: bool,
    /// The node has a `url_checks` row.
    pub url: bool,
    /// The node has a `dns_checks` row.
    pub dns: bool,
}

impl NodeKind {
    /// Every kind, in precedence order — the same order [`Self::resolve`] applies.
    ///
    /// For anything that must present all of them: a filter's vocabulary, a per-kind tally. A kind
    /// missing here drops out of those silently, so the test below pins it against the tokens.
    pub const ALL: [NodeKind; 4] = [
        NodeKind::Meraki,
        NodeKind::Url,
        NodeKind::Dns,
        NodeKind::Device,
    ];

    /// The serialized token — the same string `#[serde(rename_all = "snake_case")]` produces.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Meraki => "meraki",
            Self::Url => "url",
            Self::Dns => "dns",
            Self::Device => "device",
        }
    }

    /// The inverse of [`Self::as_str`]: an exact token, or `None`.
    ///
    /// Deliberately not lenient. Unlike a *stored* token, this parses what a caller **asked for**,
    /// and quietly widening an unrecognised filter to "every kind" would answer a different
    /// question than the one asked.
    #[must_use]
    pub fn from_token(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|v| v.as_str() == s)
    }

    /// **The** precedence: decide a node's kind from the rows it carries.
    ///
    /// This is the only place the order lives. Every surface that answers "what is this node"
    /// — the scheduler that builds its jobs, the API that describes it, the writer that decides
    /// whether a new check may be attached — must come through here, or a node is one kind to the
    /// poller and another to the operator looking at it.
    #[must_use]
    pub const fn resolve(rows: NodeRows) -> Self {
        if rows.meraki {
            Self::Meraki
        } else if rows.url {
            Self::Url
        } else if rows.dns {
            Self::Dns
        } else {
            Self::Device
        }
    }

    /// Whether the scheduler produces per-node poll jobs for this kind.
    ///
    /// Only Meraki is `false`: those nodes are polled by the org collector, so a per-node job would
    /// poll them twice. Both the sweep (which preloads Meraki ids to skip) and the on-demand
    /// "poll now" path (which short-circuits per node) are stating this one fact.
    #[must_use]
    pub const fn is_polled_per_node(self) -> bool {
        match self {
            Self::Meraki => false,
            Self::Url | Self::Dns | Self::Device => true,
        }
    }

    /// How to name this kind in a message to an operator.
    ///
    /// Distinct from the serialized token (`"url"`) because that one is an API contract and this
    /// one is prose — "node is already a URL monitor" reads as English, `"url"` does not.
    #[must_use]
    pub const fn display_name(self) -> &'static str {
        match self {
            Self::Meraki => "Meraki",
            Self::Url => "URL",
            Self::Dns => "DNS",
            Self::Device => "device",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn meraki_outranks_a_url_check_which_outranks_a_dns_check() {
        // A node holding several rows resolves to exactly one kind, and always the same one. The
        // rows are not supposed to coexist, but the API guard that stops them is younger than some
        // of the rows, so the resolution has to answer for the ones already out there.
        let all = NodeRows {
            meraki: true,
            url: true,
            dns: true,
        };
        assert_eq!(NodeKind::resolve(all), NodeKind::Meraki);
        assert_eq!(
            NodeKind::resolve(NodeRows {
                meraki: false,
                ..all
            }),
            NodeKind::Url
        );
        assert_eq!(
            NodeKind::resolve(NodeRows {
                meraki: false,
                url: false,
                dns: true
            }),
            NodeKind::Dns
        );
        assert_eq!(NodeKind::resolve(NodeRows::default()), NodeKind::Device);
    }

    #[test]
    fn only_meraki_is_polled_by_something_other_than_the_scheduler() {
        assert!(!NodeKind::Meraki.is_polled_per_node());
        for kind in [NodeKind::Url, NodeKind::Dns, NodeKind::Device] {
            assert!(kind.is_polled_per_node(), "{kind:?} should produce jobs");
        }
    }

    #[test]
    fn all_lists_every_kind_once_and_agrees_with_the_tokens() {
        // `as_str` is exhaustive, so a new variant is forced to get a token — but nothing forces
        // it into `ALL`, and a kind missing from `ALL` silently drops out of the inventory filter's
        // vocabulary. Pinning the two together fails either way round.
        let expected = ["meraki", "url", "dns", "device"];
        assert_eq!(NodeKind::ALL.len(), expected.len());
        let tokens: Vec<&str> = NodeKind::ALL.iter().map(|k| k.as_str()).collect();
        assert_eq!(tokens, expected, "ALL is precedence order");
        for k in NodeKind::ALL {
            assert_eq!(NodeKind::from_token(k.as_str()), Some(k));
        }
        // Not lenient: a filter naming something else is refused, never widened.
        assert_eq!(NodeKind::from_token("switch"), None);
        assert_eq!(NodeKind::from_token("URL"), None);
    }

    #[test]
    fn the_token_and_the_serde_tag_are_the_same_string() {
        // Two mechanisms produce this one value — `as_str` and `#[serde(rename_all)]` — and
        // nothing else makes them agree. The REST query parses with serde and the MCP tool parses
        // with `from_token`, so a disagreement is one surface accepting what the other refuses.
        for k in NodeKind::ALL {
            assert_eq!(
                serde_json::to_string(&k).unwrap(),
                format!("\"{}\"", k.as_str())
            );
        }
    }

    #[test]
    fn the_serialized_token_is_the_snake_case_variant_name() {
        // The token is an API contract — `NodeDetail.kind` — so pin it rather than trusting the
        // derive to keep meaning what it means today.
        for (kind, token) in [
            (NodeKind::Meraki, "\"meraki\""),
            (NodeKind::Url, "\"url\""),
            (NodeKind::Dns, "\"dns\""),
            (NodeKind::Device, "\"device\""),
        ] {
            assert_eq!(serde_json::to_string(&kind).unwrap(), token);
        }
    }
}
