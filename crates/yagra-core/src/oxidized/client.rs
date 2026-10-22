// SPDX-License-Identifier: AGPL-3.0-only
//! A reader for oxidized's REST API (oxidized-web 0.18, bundled in the oxidized 0.37 image).
//!
//! The shapes were read from that version's source, not from documentation, which says little:
//! `/nodes.json` is the node list with `status` and `time` (both the word `never` until the first
//! fetch), `/node/version.json` is a node's Git history, and `/node/version/view` with
//! `format=text` is one version's text. A field this does not recognise is ignored rather than
//! failed on, so a newer oxidized degrades to "fewer details" instead of "nothing".

use chrono::{DateTime, Utc};
use serde::Deserialize;
use std::time::Duration;
use uuid::Uuid;

/// How long one call to oxidized may take. Its API answers from memory and a Git object read, so
/// anything near this is a stuck container rather than a slow answer.
const TIMEOUT: Duration = Duration::from_secs(15);

/// One entry of `/nodes.json`, as much of it as Yagra reads.
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct OxNode {
    pub name: String,
    #[serde(default)]
    pub group: Option<String>,
    /// `success`, `no_connection`, `fail`, `timelimit`, or `never`.
    #[serde(default)]
    pub status: Option<String>,
    /// When the last fetch ended, or `never`.
    #[serde(default)]
    pub time: Option<serde_json::Value>,
}

/// One entry of a node's history.
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct OxVersion {
    pub oid: String,
    /// The commit time as oxidized prints a Ruby `Time`; see [`parse_time`].
    #[serde(default)]
    pub date: Option<String>,
}

/// A Ruby `Time` as oxidized's JSON carries it: `2026-10-22 10:00:00 +0000` or
/// `2026-10-22 10:00:00 UTC`. Anything else (including `never`) is `None`.
#[must_use]
pub(crate) fn parse_time(text: &str) -> Option<DateTime<Utc>> {
    let text = text.trim();
    let normalised = text
        .strip_suffix(" UTC")
        .map_or_else(|| text.to_owned(), |t| format!("{t} +0000"));
    DateTime::parse_from_str(&normalised, "%Y-%m-%d %H:%M:%S %z")
        .ok()
        .map(|t| t.with_timezone(&Utc))
}

/// A client for one oxidized.
pub(crate) struct OxidizedClient {
    base: String,
    http: reqwest::Client,
}

impl OxidizedClient {
    #[must_use]
    pub(crate) fn new(base: &str) -> Self {
        Self {
            base: base.trim_end_matches('/').to_owned(),
            // Never follows: the address is static configuration, and there is no reason for it
            // to send core anywhere else.
            http: yagra_base::http::client(TIMEOUT, yagra_base::http::Redirects::None),
        }
    }

    async fn get(&self, path: &str, query: &[(&str, &str)]) -> anyhow::Result<reqwest::Response> {
        let res = self
            .http
            .get(format!("{}{path}", self.base))
            .query(query)
            .send()
            .await?;
        let status = res.status();
        if !status.is_success() {
            anyhow::bail!("oxidized answered {status} for {path}");
        }
        Ok(res)
    }

    /// Ask oxidized to read the node list from Yagra again.
    pub(crate) async fn reload(&self) -> anyhow::Result<()> {
        self.get("/reload.json", &[]).await.map(|_| ())
    }

    /// Ask oxidized to fetch one node now.
    pub(crate) async fn fetch_now(&self, node: Uuid) -> anyhow::Result<()> {
        self.get(&format!("/node/next/{node}.json"), &[])
            .await
            .map(|_| ())
    }

    /// Every node oxidized knows, with its last result.
    pub(crate) async fn nodes(&self) -> anyhow::Result<Vec<OxNode>> {
        Ok(self.get("/nodes.json", &[]).await?.json().await?)
    }

    /// A node's versions, newest first as oxidized lists them.
    pub(crate) async fn versions(&self, node: Uuid) -> anyhow::Result<Vec<OxVersion>> {
        let full = format!("{}/{node}", super::GROUP);
        Ok(self
            .get("/node/version.json", &[("node_full", &full)])
            .await?
            .json()
            .await?)
    }

    /// One version's text.
    pub(crate) async fn version_text(&self, node: Uuid, oid: &str) -> anyhow::Result<String> {
        let name = node.to_string();
        Ok(self
            .get(
                "/node/version/view",
                &[
                    ("node", &name),
                    ("group", super::GROUP),
                    ("oid", oid),
                    ("format", "text"),
                ],
            )
            .await?
            .text()
            .await?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn both_spellings_of_a_ruby_time_parse_and_never_does_not() {
        let a = parse_time("2026-10-22 10:00:00 +0900").expect("offset");
        assert_eq!(a.to_rfc3339(), "2026-10-22T01:00:00+00:00");
        let b = parse_time("2026-10-22 10:00:00 UTC").expect("utc");
        assert_eq!(b.to_rfc3339(), "2026-10-22T10:00:00+00:00");
        assert!(parse_time("never").is_none());
    }

    #[test]
    fn a_node_list_entry_with_extra_fields_still_reads() {
        let raw = r#"[{"name":"00000000-0000-4000-8000-000000000001","full_name":"yagra/x",
            "ip":"192.0.2.1","group":"yagra","model":"IOS","last":null,"vars":{},
            "mtime":"unknown","status":"never","time":"never"}]"#;
        let nodes: Vec<OxNode> = serde_json::from_str(raw).expect("parse");
        assert_eq!(nodes[0].status.as_deref(), Some("never"));
        assert_eq!(nodes[0].group.as_deref(), Some("yagra"));
    }
}
