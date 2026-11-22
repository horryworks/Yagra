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
    /// The commit time as oxidized 0.37 and earlier print a Ruby `Time`; see [`parse_time`].
    #[serde(default)]
    date: Option<String>,
    /// The same commit time from oxidized 0.38 on, which dropped `date` for `time`. Read
    /// through [`OxVersion::committed`], never directly.
    #[serde(default)]
    time: Option<String>,
}

impl OxVersion {
    /// When this version was committed, whichever field the running oxidized spells it in.
    #[must_use]
    pub(crate) fn committed(&self) -> Option<DateTime<Utc>> {
        self.date
            .as_deref()
            .or(self.time.as_deref())
            .and_then(parse_time)
    }
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

/// The versions in a `/node/version.json` body.
///
/// 🚨 A node with no stored version yet does not get an empty array: oxidized answers something that
/// is not one, measured on a lab deployment in the seconds between "fetch now" and the first commit.
/// Read as an error, that put "the config backup service did not answer" on the screen of every node
/// just switched on. Anything that is not an array is "no versions yet"; an entry that does not read
/// is skipped.
#[must_use]
pub(crate) fn versions_of(body: &str) -> Vec<OxVersion> {
    match serde_json::from_str::<serde_json::Value>(body) {
        Ok(serde_json::Value::Array(items)) => items
            .into_iter()
            .filter_map(|v| serde_json::from_value(v).ok())
            .collect(),
        _ => Vec::new(),
    }
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
        let body = self
            .get("/node/version.json", &[("node_full", &full)])
            .await?
            .text()
            .await?;
        Ok(versions_of(&body))
    }

    /// One version's text, `None` when `oid` is not one of `node`'s versions.
    ///
    /// 🚨 The list is asked first because oxidized's view reads `<group>/<node>` out of whatever
    /// commit `oid` names, and its repository holds commits that are not the node's history: the
    /// config-backup destination is fetched into it before every push (ADR-210 stage 9). Anyone who
    /// can write to that destination could commit a file at `yagra/<node id>` and have a crafted
    /// link show it as the device's configuration.
    pub(crate) async fn version_text(
        &self,
        node: Uuid,
        oid: &str,
    ) -> anyhow::Result<Option<String>> {
        if !self.versions(node).await?.iter().any(|v| v.oid == oid) {
            return Ok(None);
        }
        let name = node.to_string();
        Ok(Some(
            self.get(
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
            .await?,
        ))
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
    fn a_node_with_no_history_has_no_versions_rather_than_an_error() {
        assert!(versions_of("null").is_empty());
        assert!(versions_of("").is_empty());
        assert!(versions_of("<html>").is_empty());
        let one = versions_of(
            r#"[{"oid":"abc","date":"2026-10-22 10:00:00 +0000","author":{"name":"Yagra"}},{"x":1}]"#,
        );
        assert_eq!(one.len(), 1);
        assert_eq!(one[0].oid, "abc");
    }

    /// oxidized 0.38 dropped a version's `date` for `time` (its CHANGELOG, "output/git"); 0.37 sends
    /// `date`. Either one dates the version, so moving the pinned image does not blank every stored
    /// version's time on the Config tab.
    #[test]
    fn a_version_is_dated_by_date_or_by_time() {
        let at = "2026-11-19T10:00:00+00:00";
        let old = versions_of(r#"[{"oid":"a","date":"2026-11-19 10:00:00 UTC"}]"#);
        let new = versions_of(r#"[{"oid":"b","time":"2026-11-19 10:00:00 +0000","message":"m"}]"#);
        let both = versions_of(
            r#"[{"oid":"c","date":"2026-11-19 10:00:00 UTC","time":"2026-11-19 10:00:00 UTC"}]"#,
        );
        for v in [&old[0], &new[0], &both[0]] {
            assert_eq!(
                v.committed().map(|t| t.to_rfc3339()).as_deref(),
                Some(at),
                "{}",
                v.oid
            );
        }
        assert!(versions_of(r#"[{"oid":"d"}]"#)[0].committed().is_none());
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

    /// ADR-210 stage 9: a commit that is not one of the node's versions (the fetched backup
    /// destination is in the same repository) is never handed to oxidized's view.
    #[tokio::test]
    async fn a_version_outside_the_nodes_list_is_never_read() {
        let mine = "a".repeat(40);
        let theirs = "b".repeat(40);
        let list = format!(r#"[{{"oid":"{mine}","date":"2026-11-19 10:00:00 UTC"}}]"#);
        let (addr, seen) = yagra_base::httpfake::serve(vec![
            (200, list.clone()),
            (200, list),
            (200, "hostname sw-01".to_owned()),
        ])
        .await;
        let client = OxidizedClient::new(&format!("http://{addr}"));
        let node = Uuid::nil();
        assert_eq!(
            client.version_text(node, &theirs).await.expect("asked"),
            None
        );
        assert_eq!(
            client
                .version_text(node, &mine)
                .await
                .expect("asked")
                .as_deref(),
            Some("hostname sw-01")
        );
        let paths: Vec<String> = seen
            .lock()
            .unwrap()
            .iter()
            .map(|s| s.path.clone())
            .collect();
        assert_eq!(paths.len(), 3, "{paths:?}");
        assert!(paths[0].starts_with("/node/version.json"), "{paths:?}");
        assert!(
            paths[2].contains(&mine) && !paths.iter().any(|p| p.contains(&theirs)),
            "{paths:?}"
        );
    }
}
