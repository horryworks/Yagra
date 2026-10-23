// SPDX-License-Identifier: AGPL-3.0-only
//! Where config backups are pushed (ADR-210 stage 4): one Git repository for the whole deployment,
//! in the one-row table `config_backup_remote` (migration 0155).
//!
//! The push itself runs inside the bundled oxidized — a small script on its `node_success` hook
//! asks core whether to push, pushes once, and reports (decision 29). Every decision is here, so it
//! can be tested without a database: whether a head still needs pushing ([`should_push`]), what a
//! failed push's output means ([`classify_push_output`]), when a run of failures is an alert
//! ([`alert_due`]), and which destinations and branch names are accepted at all ([`check_url`],
//! [`check_branch`]). The SSH key a destination is reached with is generated here too
//! ([`generate_ssh_key`]): the private half is sealed as a `git_remote` credential and never shown.

use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::{PgPool, Row};
use uuid::Uuid;

/// How the destination is reached.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum GitAuth {
    /// HTTPS, with a user name and an access token.
    HttpsToken,
    /// SSH, with a key Yagra generated and the person registered as a deploy key.
    SshKey,
    /// A token a newer core wrote. Nothing is pushed with it, and it is refused as input.
    Unknown,
}

crate::token_enum!(GitAuth, Unknown, "config_backup_remote.auth", [
    HttpsToken => "https_token",
    SshKey => "ssh_key",
    Unknown => "unknown",
]);

/// Why a push failed, read from git's output by [`classify_push_output`]. The output itself is
/// never reported: it can carry what the destination printed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum PushFailure {
    /// The destination refused the login, or it does not exist for this login (GitHub answers a
    /// repository the token cannot reach exactly as one that does not exist).
    Auth,
    /// The destination's SSH host key is not the one learned on the first push.
    HostKey,
    /// The destination holds a version Yagra does not have, or refuses the branch.
    Rejected,
    /// The destination did not answer.
    Unreachable,
    /// Anything else.
    Other,
    /// A token a newer core wrote.
    Unknown,
}

crate::token_enum!(PushFailure, Unknown, "config_backup_remote.failure", [
    Auth => "auth",
    HostKey => "host_key",
    Rejected => "rejected",
    Unreachable => "unreachable",
    Other => "other",
    Unknown => "unknown",
]);

impl PushFailure {
    /// Whether the next push will fail the same way unless a person changes something, so the
    /// first one is already worth an alert (decision 33).
    #[must_use]
    pub const fn is_permanent(self) -> bool {
        match self {
            Self::Auth | Self::HostKey | Self::Rejected => true,
            Self::Unreachable | Self::Other | Self::Unknown => false,
        }
    }
}

/// What git prints for each failure, lower-cased, matched in this order against git's lower-cased
/// output; the first that appears wins. Sent to the push script with every job, so the script
/// carries no rule of its own and this table is the one that is tested.
///
/// The order is load-bearing: a host-key failure ends in "could not read from remote repository",
/// which on its own is the auth line's company, and a rejected push still names the remote it
/// reached.
pub const PUSH_PATTERNS: &[(PushFailure, &str)] = &[
    (PushFailure::HostKey, "host key verification failed"),
    (
        PushFailure::HostKey,
        "remote host identification has changed",
    ),
    (PushFailure::Rejected, "[rejected]"),
    (PushFailure::Rejected, "[remote rejected]"),
    (PushFailure::Rejected, "non-fast-forward"),
    (PushFailure::Rejected, "fetch first"),
    (PushFailure::Auth, "authentication failed"),
    (PushFailure::Auth, "could not read username"),
    (PushFailure::Auth, "could not read password"),
    (PushFailure::Auth, "permission denied"),
    (PushFailure::Auth, "repository not found"),
    (PushFailure::Auth, "the requested url returned error: 401"),
    (PushFailure::Auth, "the requested url returned error: 403"),
    (PushFailure::Auth, "the requested url returned error: 404"),
    (PushFailure::Unreachable, "could not resolve host"),
    (PushFailure::Unreachable, "could not resolve hostname"),
    (PushFailure::Unreachable, "failed to connect"),
    (PushFailure::Unreachable, "connection timed out"),
    (PushFailure::Unreachable, "operation timed out"),
    (PushFailure::Unreachable, "connection refused"),
    (PushFailure::Unreachable, "no route to host"),
    (PushFailure::Unreachable, "network is unreachable"),
    (PushFailure::Unreachable, "connection reset"),
    (PushFailure::Unreachable, "timed out"),
];

/// The failure git's `output` describes, by [`PUSH_PATTERNS`]. What the push script does with the
/// same table, in Rust so it can be tested.
#[must_use]
pub fn classify_push_output(output: &str) -> PushFailure {
    let lower = output.to_lowercase();
    PUSH_PATTERNS
        .iter()
        .find(|(_, needle)| lower.contains(needle))
        .map_or(PushFailure::Other, |(kind, _)| *kind)
}

/// How long after a failed push the next one waits. Pushes are asked for on every fetch that
/// succeeds, so without this a broken destination would be dialled once per device per day.
pub const RETRY_AFTER: Duration = Duration::from_secs(10 * 60);

/// Failures in a row before one that is not [`PushFailure::is_permanent`] is an alert: a network
/// that drops one push is not worth a page (decision 33).
pub const TRANSIENT_FAILURES_BEFORE_ALERT: u32 = 2;

/// The destination, as stored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigRemote {
    pub url: String,
    pub branch: String,
    pub auth: GitAuth,
    pub credential_id: Uuid,
    pub ssh_public_key: Option<String>,
    pub known_hosts: Option<String>,
    pub generation: i64,
    /// The head of oxidized's repository that was last pushed.
    pub pushed_head: Option<String>,
    /// The digest of the file-name map that was last pushed ([`layout_digest`]).
    pub pushed_layout: Option<String>,
    /// The commit the destination holds after the last push: what a person finds there.
    pub pushed_commit: Option<String>,
    pub pushed_at: Option<DateTime<Utc>>,
    pub failure: Option<PushFailure>,
    pub failed_at: Option<DateTime<Utc>>,
    pub failures: u32,
    pub updated_at: DateTime<Utc>,
}

/// Whether the repository at `head`, filed under the names `layout` digests, still needs pushing
/// to `remote`: not when both are what was last pushed, and not within [`RETRY_AFTER`] of a
/// failure. A rename or a deletion changes `layout` without a new version (decision 40).
#[must_use]
pub fn should_push(remote: &ConfigRemote, head: &str, layout: &str, now: DateTime<Utc>) -> bool {
    if remote.auth == GitAuth::Unknown {
        return false;
    }
    if remote.pushed_head.as_deref() == Some(head)
        && remote.pushed_layout.as_deref() == Some(layout)
    {
        return false;
    }
    let retry =
        chrono::Duration::from_std(RETRY_AFTER).unwrap_or_else(|_| chrono::Duration::zero());
    remote.failed_at.is_none_or(|at| now - at >= retry)
}

/// The run of failures to report when the destination's alert should be open, `None` when it
/// should be closed (decision 33).
#[must_use]
pub fn alert_due(remote: Option<&ConfigRemote>) -> Option<u32> {
    let remote = remote?;
    let failure = remote.failure?;
    (failure.is_permanent() || remote.failures >= TRANSIENT_FAILURES_BEFORE_ALERT)
        .then_some(remote.failures)
}

/// A node a configuration may be filed under: its id, the name it had (its current one while it
/// exists), and whether it still exists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NameEntry {
    pub node_id: Uuid,
    pub name: String,
    pub registered: bool,
}

/// The folder a deleted node's configuration is filed in (decision 38).
pub const DELETED_DIR: &str = "deleted";
/// The longest file name stem kept, in characters.
pub const FILE_STEM_MAX: usize = 100;

/// A node name made safe as one file name on every system a clone may land on: letters, digits,
/// `-`, `_` and `.` are kept, anything else (a path separator, a colon, a space) becomes `_`, a
/// leading `.` does too (a hidden file), it is cut to [`FILE_STEM_MAX`] characters, and a name
/// Windows reserves gets a leading `_`. `None` when nothing is left.
#[must_use]
pub fn file_stem(name: &str) -> Option<String> {
    let mut stem: String = name
        .trim()
        .chars()
        .take(FILE_STEM_MAX)
        .map(|c| {
            if c.is_ascii_alphanumeric()
                || matches!(c, '-' | '_' | '.')
                || (!c.is_ascii() && c.is_alphanumeric())
            {
                c
            } else {
                '_'
            }
        })
        .collect();
    if stem.starts_with('.') {
        stem.replace_range(0..1, "_");
    }
    while stem.ends_with('.') {
        stem.pop();
    }
    if stem.is_empty() {
        return None;
    }
    const RESERVED: &[&str] = &[
        "con", "prn", "aux", "nul", "com1", "com2", "com3", "com4", "com5", "com6", "com7", "com8",
        "com9", "lpt1", "lpt2", "lpt3", "lpt4", "lpt5", "lpt6", "lpt7", "lpt8", "lpt9",
    ];
    let lower = stem.to_lowercase();
    let base = lower.split('.').next().unwrap_or("");
    if RESERVED.contains(&base) {
        stem.insert(0, '_');
    }
    Some(stem)
}

/// The path each node's configuration is pushed under (decision 38): `<name>.txt`, or
/// `deleted/<name>.txt` for a node that no longer exists. Names that clash in one folder — compared
/// without case, since a clone onto Windows or macOS would fold them — all get `__<first 8 of the
/// id>`, so no device silently takes another's file. A name with nothing usable in it is the id.
/// Sorted by node id, so the same inventory always gives the same map.
#[must_use]
pub fn export_paths(entries: &[NameEntry]) -> Vec<(Uuid, String)> {
    use std::collections::HashMap;
    let stems: Vec<(Uuid, bool, String)> = entries
        .iter()
        .map(|e| {
            let stem = file_stem(&e.name).unwrap_or_else(|| e.node_id.to_string());
            (e.node_id, e.registered, stem)
        })
        .collect();
    let mut counts: HashMap<(bool, String), usize> = HashMap::new();
    for (_, registered, stem) in &stems {
        *counts
            .entry((*registered, stem.to_lowercase()))
            .or_default() += 1;
    }
    let mut stems = stems;
    stems.sort_by_key(|(id, _, _)| *id);
    // The suffix can itself collide — a node may be *named* `sw__aaaaaaaa` — so every path is
    // checked against the ones already given out, case-folded, and a taken one falls back to the
    // whole id, then to a counter. Two ids never share a file.
    let mut taken: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut out: Vec<(Uuid, String)> = Vec::with_capacity(stems.len());
    for (id, registered, stem) in stems {
        let clash = counts[&(registered, stem.to_lowercase())] > 1;
        let short = &id.simple().to_string()[..8];
        let folder = |file: String| {
            if registered {
                file
            } else {
                format!("{DELETED_DIR}/{file}")
            }
        };
        let first = if clash {
            format!("{stem}__{short}.txt")
        } else {
            format!("{stem}.txt")
        };
        // Built one at a time, as far as the first free one: almost always the first.
        let path = std::iter::once(first)
            .chain(std::iter::once_with(|| format!("{stem}__{id}.txt")))
            .chain((1..=entries.len() + 1).map(|n| format!("{id}-{n}.txt")))
            .map(&folder)
            .find(|p| !taken.contains(&p.to_lowercase()))
            .unwrap_or_else(|| folder(format!("{id}.txt")));
        taken.insert(path.to_lowercase());
        out.push((id, path));
    }
    out
}

/// One digest of the file-name map, so a rename or a deletion is noticed without storing the map.
#[must_use]
pub fn layout_digest(paths: &[(Uuid, String)]) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    for (id, path) in paths {
        hasher.update(id.as_bytes());
        hasher.update(path.as_bytes());
        hasher.update(b"\n");
    }
    data_encoding::HEXLOWER.encode(&hasher.finalize())
}

/// The longest destination URL accepted.
pub const URL_MAX: usize = 512;

/// Whether `url` is a destination Yagra pushes to (decision 35): `https://host/path`,
/// `ssh://[user@]host[:port]/path` or `user@host:path`. Refused: plain `http://` (the token would
/// cross the network in the clear), `file://` and local paths (a push into the container itself),
/// a user name or password inside an HTTPS URL (it would sit in the row and in every log of the
/// command), anything starting with `-` (git would read it as an option), and whitespace or control
/// characters. `Err` is a sentence for the person who typed it.
pub fn check_url(url: &str) -> Result<(), &'static str> {
    if url.is_empty() || url.len() > URL_MAX {
        return Err("the URL must be between 1 and 512 characters");
    }
    if url.starts_with('-') || url.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err("the URL must not start with '-' or contain spaces");
    }
    host_of(url).map(|_| ()).ok_or(
        "the URL must look like https://host/owner/repo.git, ssh://git@host/owner/repo.git or \
         git@host:owner/repo.git",
    )
}

/// The host part of an accepted destination URL, for a screen or a tool result that should not
/// carry the whole path. `None` for a URL [`check_url`] refuses.
#[must_use]
pub fn host_of(url: &str) -> Option<&str> {
    let (host, path) = if let Some(rest) = url.strip_prefix("https://") {
        let (authority, path) = rest.split_once('/')?;
        if authority.contains('@') {
            return None;
        }
        (authority, path)
    } else if let Some(rest) = url.strip_prefix("ssh://") {
        let (authority, path) = rest.split_once('/')?;
        let authority = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
        (authority, path)
    } else if url.contains("://") {
        return None;
    } else {
        // scp-like: `user@host:path`. The user is required: a bare `host:path` and a local path
        // look the same to git when the host part contains a slash, and a local path is refused.
        let (user_host, path) = url.split_once(':')?;
        let (user, host) = user_host.split_once('@')?;
        if user.is_empty() || host.contains('/') {
            return None;
        }
        // An absolute path is the scp form's own spelling (`git@host:/srv/git/x.git`), not an
        // empty segment.
        (host, path.strip_prefix('/').unwrap_or(path))
    };
    // The host, without a port. A port, when there is a `:`, is a number from 1 to 65535 — an empty
    // one is how `https://https://host/…` used to pass, read as the host `https` (2026-10-23).
    let port_ok = |port: &str| port.parse::<u16>().is_ok_and(|p| p > 0);
    let name = if let Some(rest) = host.strip_prefix('[') {
        // An IPv6 literal: `[addr]` or `[addr]:port`.
        let (addr, after) = rest.split_once(']')?;
        match after {
            "" => {}
            _ => port_ok(after.strip_prefix(':')?).then_some(())?,
        }
        if addr.is_empty() || !addr.chars().all(|c| c.is_ascii_hexdigit() || c == ':') {
            return None;
        }
        &host[..addr.len() + 2]
    } else {
        match host.split_once(':') {
            Some((h, port)) => port_ok(port).then_some(h)?,
            None => host,
        }
    };
    let ok_host = !name.is_empty()
        && (name.starts_with('[')
            || name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_')));
    // A path starting with `/` is an empty segment: what a scheme typed twice leaves behind.
    (ok_host && !path.is_empty() && !path.starts_with('/') && !path.contains("..")).then_some(name)
}

/// The scheme and authority of an HTTPS destination (`https://host[:port]`, lower-cased) — what a
/// sealed token was given for. `None` for any other URL.
#[must_use]
pub fn https_origin(url: &str) -> Option<String> {
    let rest = url.strip_prefix("https://")?;
    let authority = rest.split('/').next()?;
    Some(format!("https://{}", authority.to_ascii_lowercase()))
}

/// The longest branch name accepted.
pub const BRANCH_MAX: usize = 100;

/// Whether `branch` is a branch name pushed to: letters, digits, `.`, `_`, `-` and `/`, not
/// starting with `-`, `.` or `/`, no `..` or `//`, not ending in `/` or `.lock`. Narrower than git's
/// own rule on purpose — it is interpolated into a refspec.
pub fn check_branch(branch: &str) -> Result<(), &'static str> {
    let ok = !branch.is_empty()
        && branch.len() <= BRANCH_MAX
        && branch
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '/'))
        && !branch.starts_with(['-', '.', '/'])
        && !branch.ends_with('/')
        && !branch.ends_with(".lock")
        && !branch.contains("..")
        && !branch.contains("//");
    if ok {
        Ok(())
    } else {
        Err("the branch must be a plain name such as main or backups/configs")
    }
}

/// The most host-key lines kept for one destination.
const KNOWN_HOSTS_LINES_MAX: usize = 8;
/// The longest host-key line kept.
const KNOWN_HOSTS_LINE_MAX: usize = 2048;

/// The known_hosts text the push script reports after the first SSH push, checked and normalised:
/// every line `host keytype base64`, the key type one of SSH's, the key valid base64. `None` when
/// any line is not, so nothing half-read is pinned.
#[must_use]
pub fn check_known_hosts(text: &str) -> Option<String> {
    let lines: Vec<&str> = text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    if lines.is_empty() || lines.len() > KNOWN_HOSTS_LINES_MAX {
        return None;
    }
    for line in &lines {
        if line.len() > KNOWN_HOSTS_LINE_MAX {
            return None;
        }
        let mut parts = line.split(' ');
        let (Some(host), Some(kind), Some(key), None) =
            (parts.next(), parts.next(), parts.next(), parts.next())
        else {
            return None;
        };
        let kind_ok =
            kind.starts_with("ssh-") || kind.starts_with("ecdsa-") || kind.starts_with("sk-");
        if host.is_empty() || !kind_ok || data_encoding::BASE64.decode(key.as_bytes()).is_err() {
            return None;
        }
    }
    Some(lines.join("\n") + "\n")
}

/// Each pinned host key as `keytype SHA256:<fingerprint>`, the form `ssh-keygen -l` prints and
/// GitHub, Bitbucket and GitLab publish.
#[must_use]
pub fn fingerprints(known_hosts: &str) -> Vec<String> {
    use sha2::{Digest, Sha256};
    known_hosts
        .lines()
        .filter_map(|line| {
            let mut parts = line.split(' ');
            let _host = parts.next()?;
            let kind = parts.next()?;
            let blob = data_encoding::BASE64
                .decode(parts.next()?.as_bytes())
                .ok()?;
            let digest = Sha256::digest(&blob);
            Some(format!(
                "{kind} SHA256:{}",
                data_encoding::BASE64_NOPAD.encode(&digest)
            ))
        })
        .collect()
}

/// The comment the generated public key carries, so it is recognisable in a deploy-key list.
pub const SSH_KEY_COMMENT: &str = "yagra-config-backup";

/// A fresh ed25519 key: the private half in OpenSSH form (what `ssh -i` reads; never shown) and the
/// public half as one `ssh-ed25519 … comment` line (what a deploy key is registered with).
#[must_use]
pub fn generate_ssh_key() -> (String, String) {
    use rand::RngCore;
    let mut seed = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut seed);
    let key = openssh_ed25519(&seed, rand::thread_rng().next_u32());
    seed.fill(0);
    key
}

/// The OpenSSH encoding of the ed25519 key `seed`, unencrypted (`PROTOCOL.key`). `check` is the
/// random pair of equal words OpenSSH uses to tell a wrong passphrase; with no passphrase it is
/// still required.
fn openssh_ed25519(seed: &[u8; 32], check: u32) -> (String, String) {
    fn string(out: &mut Vec<u8>, bytes: &[u8]) {
        let len = u32::try_from(bytes.len()).expect("an SSH string is short");
        out.extend_from_slice(&len.to_be_bytes());
        out.extend_from_slice(bytes);
    }
    let signing = ed25519_dalek::SigningKey::from_bytes(seed);
    let public = signing.verifying_key().to_bytes();

    let mut blob = Vec::new();
    string(&mut blob, b"ssh-ed25519");
    string(&mut blob, &public);

    let mut private = Vec::new();
    private.extend_from_slice(&check.to_be_bytes());
    private.extend_from_slice(&check.to_be_bytes());
    string(&mut private, b"ssh-ed25519");
    string(&mut private, &public);
    let mut pair = Vec::with_capacity(64);
    pair.extend_from_slice(seed);
    pair.extend_from_slice(&public);
    string(&mut private, &pair);
    pair.fill(0);
    string(&mut private, SSH_KEY_COMMENT.as_bytes());
    let mut pad = 1u8;
    while private.len() % 8 != 0 {
        private.push(pad);
        pad += 1;
    }

    let mut body = b"openssh-key-v1\0".to_vec();
    string(&mut body, b"none");
    string(&mut body, b"none");
    string(&mut body, b"");
    body.extend_from_slice(&1u32.to_be_bytes());
    string(&mut body, &blob);
    string(&mut body, &private);
    private.fill(0);

    let encoded = data_encoding::BASE64.encode(&body);
    body.fill(0);
    let mut pem = String::from("-----BEGIN OPENSSH PRIVATE KEY-----\n");
    for chunk in encoded.as_bytes().chunks(70) {
        pem.push_str(std::str::from_utf8(chunk).expect("base64 is ASCII"));
        pem.push('\n');
    }
    pem.push_str("-----END OPENSSH PRIVATE KEY-----\n");
    let public_line = format!(
        "ssh-ed25519 {} {SSH_KEY_COMMENT}",
        data_encoding::BASE64.encode(&blob)
    );
    (pem, public_line)
}

/// What a successful push delivered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Pushed<'a> {
    /// The head of oxidized's repository it was made from.
    pub head: &'a str,
    /// The digest of the file-name map it was made with.
    pub layout: &'a str,
    /// The commit the destination now holds.
    pub commit: &'a str,
}

/// What a person sets.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteSettings {
    pub url: String,
    pub branch: String,
    pub auth: GitAuth,
    pub credential_id: Uuid,
    pub ssh_public_key: Option<String>,
}

const COLUMNS: &str = "url, branch, auth, credential_id, ssh_public_key, known_hosts, generation, \
                       pushed_head, pushed_layout, pushed_commit, pushed_at, failure, failed_at, \
                       failures, updated_at";

fn remote_of(r: &sqlx::postgres::PgRow) -> Result<ConfigRemote, sqlx::Error> {
    Ok(ConfigRemote {
        url: r.try_get("url")?,
        branch: r.try_get("branch")?,
        auth: GitAuth::from_stored(&r.try_get::<String, _>("auth")?),
        credential_id: r.try_get("credential_id")?,
        ssh_public_key: r.try_get("ssh_public_key")?,
        known_hosts: r.try_get("known_hosts")?,
        generation: r.try_get("generation")?,
        pushed_head: r.try_get("pushed_head")?,
        pushed_layout: r.try_get("pushed_layout")?,
        pushed_commit: r.try_get("pushed_commit")?,
        pushed_at: r.try_get("pushed_at")?,
        failure: r
            .try_get::<Option<String>, _>("failure")?
            .map(|f| PushFailure::from_stored(&f)),
        failed_at: r.try_get("failed_at")?,
        failures: u32::try_from(r.try_get::<i32, _>("failures")?).unwrap_or(0),
        updated_at: r.try_get("updated_at")?,
    })
}

/// The `config_backup_remote` table.
#[derive(Clone)]
pub struct RemoteRepo {
    pool: PgPool,
}

impl RemoteRepo {
    #[must_use]
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// The destination, or `None` when none is set.
    pub async fn get(&self) -> anyhow::Result<Option<ConfigRemote>> {
        let row = sqlx::query(&format!("SELECT {COLUMNS} FROM config_backup_remote"))
            .fetch_optional(&self.pool)
            .await?;
        Ok(row.as_ref().map(remote_of).transpose()?)
    }

    /// Set the destination, replacing one that was set. Moves the generation and forgets how the
    /// last push went, so the next fetch pushes to the new place. The learned host keys are kept
    /// only when the URL did not change. Returns the credential the replaced row named, when it
    /// named a different one (the caller deletes it).
    pub async fn put(&self, s: &RemoteSettings) -> anyhow::Result<Option<Uuid>> {
        let mut tx = self.pool.begin().await?;
        let previous: Option<Uuid> =
            sqlx::query_scalar("SELECT credential_id FROM config_backup_remote FOR UPDATE")
                .fetch_optional(&mut *tx)
                .await?;
        sqlx::query(
            "INSERT INTO config_backup_remote (id, url, branch, auth, credential_id, ssh_public_key) \
             VALUES (TRUE, $1, $2, $3, $4, $5) \
             ON CONFLICT (id) DO UPDATE SET url = EXCLUDED.url, branch = EXCLUDED.branch, \
                 auth = EXCLUDED.auth, credential_id = EXCLUDED.credential_id, \
                 ssh_public_key = EXCLUDED.ssh_public_key, \
                 known_hosts = CASE WHEN config_backup_remote.url = EXCLUDED.url \
                                    THEN config_backup_remote.known_hosts END, \
                 generation = config_backup_remote.generation + 1, \
                 pushed_head = NULL, pushed_layout = NULL, pushed_at = NULL, failure = NULL, \
                 failed_at = NULL, failures = 0, updated_at = now()",
        )
        .bind(&s.url)
        .bind(&s.branch)
        .bind(s.auth.as_str())
        .bind(s.credential_id)
        .bind(s.ssh_public_key.as_deref())
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(previous.filter(|p| *p != s.credential_id))
    }

    /// Remove the destination. Returns the credential it named, for the caller to delete.
    pub async fn delete(&self) -> anyhow::Result<Option<Uuid>> {
        Ok(
            sqlx::query_scalar("DELETE FROM config_backup_remote RETURNING credential_id")
                .fetch_optional(&self.pool)
                .await?,
        )
    }

    /// Forget the learned host keys, so the next SSH push learns them again. Returns whether a
    /// destination is set.
    pub async fn forget_host_keys(&self) -> anyhow::Result<bool> {
        let done = sqlx::query(
            "UPDATE config_backup_remote SET known_hosts = NULL, \
                 generation = generation + 1, failure = NULL, failed_at = NULL, failures = 0, \
                 updated_at = now()",
        )
        .execute(&self.pool)
        .await?;
        Ok(done.rows_affected() > 0)
    }

    /// Record how one push went, if it was made for the current `generation` — a push that was
    /// running when the destination changed is ignored. `pushed` is `None` for a push that failed
    /// with `failure`; for one that succeeded it is what was pushed. `learned` is the host keys an
    /// SSH push met, kept only when none are pinned yet. Returns whether it was recorded.
    pub async fn record_push(
        &self,
        generation: i64,
        pushed: Option<&Pushed<'_>>,
        failure: Option<PushFailure>,
        learned: Option<&str>,
    ) -> anyhow::Result<bool> {
        let done = match (pushed, failure) {
            (Some(p), None) => {
                sqlx::query(
                    "UPDATE config_backup_remote SET pushed_head = $2, pushed_layout = $3, \
                         pushed_commit = $4, pushed_at = now(), \
                         failure = NULL, failed_at = NULL, failures = 0, \
                         known_hosts = COALESCE(known_hosts, $5) \
                     WHERE generation = $1",
                )
                .bind(generation)
                .bind(p.head)
                .bind(p.layout)
                .bind(p.commit)
                .bind(learned)
                .execute(&self.pool)
                .await?
            }
            (None, None) => {
                anyhow::bail!("a push report names neither what was pushed nor why not")
            }
            (_, Some(f)) => {
                sqlx::query(
                    "UPDATE config_backup_remote SET failure = $2, failed_at = now(), \
                         failures = failures + 1, known_hosts = COALESCE(known_hosts, $3) \
                     WHERE generation = $1",
                )
                .bind(generation)
                .bind(f.as_str())
                .bind(learned)
                .execute(&self.pool)
                .await?
            }
        };
        Ok(done.rows_affected() > 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn remote() -> ConfigRemote {
        ConfigRemote {
            url: "https://example.com/acme/configs.git".to_owned(),
            branch: "main".to_owned(),
            auth: GitAuth::HttpsToken,
            credential_id: Uuid::from_u128(1),
            ssh_public_key: None,
            known_hosts: None,
            generation: 1,
            pushed_head: None,
            pushed_layout: None,
            pushed_commit: None,
            pushed_at: None,
            failure: None,
            failed_at: None,
            failures: 0,
            updated_at: Utc::now(),
        }
    }

    fn entry(n: u128, name: &str, registered: bool) -> NameEntry {
        NameEntry {
            node_id: Uuid::from_u128(n),
            name: name.to_owned(),
            registered,
        }
    }

    #[test]
    fn a_node_is_filed_under_its_name_and_a_deleted_one_apart() {
        let paths = export_paths(&[
            entry(1, "sw-01", true),
            entry(2, "a/b:c d", true),
            entry(3, "", true),
            entry(4, "sw-02", false),
            entry(5, ".hidden.", true),
            entry(6, "CON", true),
            entry(7, "東京-sw", true),
        ]);
        let by_id = |n: u128| {
            paths
                .iter()
                .find(|(id, _)| *id == Uuid::from_u128(n))
                .unwrap()
                .1
                .clone()
        };
        assert_eq!(by_id(1), "sw-01.txt");
        assert_eq!(by_id(2), "a_b_c_d.txt");
        assert_eq!(by_id(3), format!("{}.txt", Uuid::from_u128(3)));
        assert_eq!(by_id(4), "deleted/sw-02.txt");
        assert_eq!(by_id(5), "_hidden.txt");
        assert_eq!(by_id(6), "_CON.txt");
        assert_eq!(by_id(7), "東京-sw.txt");
        assert!(paths.windows(2).all(|w| w[0].0 < w[1].0), "sorted by id");
    }

    #[test]
    fn names_that_clash_in_one_folder_all_carry_their_id() {
        let a = Uuid::parse_str("aaaaaaaa-0000-4000-8000-000000000001").unwrap();
        let b = Uuid::parse_str("bbbbbbbb-0000-4000-8000-000000000002").unwrap();
        let c = Uuid::parse_str("cccccccc-0000-4000-8000-000000000003").unwrap();
        let paths = export_paths(&[
            NameEntry {
                node_id: a,
                name: "SW-01".to_owned(),
                registered: true,
            },
            NameEntry {
                node_id: b,
                name: "sw-01".to_owned(),
                registered: true,
            },
            // A deleted namesake is in another folder, so it clashes with nobody.
            NameEntry {
                node_id: c,
                name: "sw-01".to_owned(),
                registered: false,
            },
        ]);
        assert_eq!(
            paths.iter().map(|(_, p)| p.as_str()).collect::<Vec<_>>(),
            [
                "SW-01__aaaaaaaa.txt",
                "sw-01__bbbbbbbb.txt",
                "deleted/sw-01.txt"
            ]
        );
        // A node named like another's suffixed file does not take it, whatever the case.
        let squatter = Uuid::parse_str("dddddddd-0000-4000-8000-000000000004").unwrap();
        let paths = export_paths(&[
            NameEntry {
                node_id: a,
                name: "sw".to_owned(),
                registered: true,
            },
            NameEntry {
                node_id: b,
                name: "sw".to_owned(),
                registered: true,
            },
            NameEntry {
                node_id: squatter,
                name: "SW__AAAAAAAA".to_owned(),
                registered: true,
            },
        ]);
        let lower: std::collections::HashSet<String> =
            paths.iter().map(|(_, p)| p.to_lowercase()).collect();
        assert_eq!(lower.len(), 3, "{paths:?}");
        assert_eq!(paths[2].1, format!("SW__AAAAAAAA__{squatter}.txt"));
        let long = export_paths(&[entry(9, &"x".repeat(300), true)]);
        assert_eq!(long[0].1.len(), FILE_STEM_MAX + 4);
    }

    #[test]
    fn a_rename_or_a_deletion_changes_the_layout_and_so_is_pushed() {
        let before = export_paths(&[entry(1, "sw-01", true)]);
        let renamed = export_paths(&[entry(1, "sw-01a", true)]);
        let deleted = export_paths(&[entry(1, "sw-01", false)]);
        let digest = layout_digest(&before);
        assert_eq!(digest, layout_digest(&before));
        assert_ne!(digest, layout_digest(&renamed));
        assert_ne!(digest, layout_digest(&deleted));

        let mut r = remote();
        r.pushed_head = Some("a1".to_owned());
        r.pushed_layout = Some(digest.clone());
        let now = Utc::now();
        assert!(!should_push(&r, "a1", &digest, now), "nothing changed");
        assert!(
            should_push(&r, "a1", &layout_digest(&renamed), now),
            "a rename"
        );
        assert!(should_push(&r, "b2", &digest, now), "a new version");
    }

    #[test]
    fn every_token_round_trips_and_agrees_with_serde() {
        for a in GitAuth::ALL {
            assert_eq!(GitAuth::from_token(a.as_str()), Some(*a));
            assert_eq!(serde_json::to_value(a).unwrap(), a.as_str());
        }
        for f in PushFailure::ALL {
            assert_eq!(PushFailure::from_token(f.as_str()), Some(*f));
            assert_eq!(serde_json::to_value(f).unwrap(), f.as_str());
        }
    }

    #[test]
    fn a_head_is_pushed_once_and_a_failure_waits_ten_minutes() {
        let now = Utc::now();
        let mut r = remote();
        assert!(should_push(&r, "a1", "L", now));
        r.pushed_head = Some("a1".to_owned());
        r.pushed_layout = Some("L".to_owned());
        assert!(!should_push(&r, "a1", "L", now), "already there");
        assert!(should_push(&r, "b2", "L", now), "a newer head");
        r.failure = Some(PushFailure::Unreachable);
        r.failed_at = Some(now - chrono::Duration::minutes(3));
        assert!(!should_push(&r, "b2", "L", now), "inside the wait");
        r.failed_at = Some(now - chrono::Duration::minutes(11));
        assert!(should_push(&r, "b2", "L", now), "after it");
        r.auth = GitAuth::Unknown;
        assert!(
            !should_push(&r, "b2", "L", now),
            "a way of pushing this core does not know"
        );
    }

    #[test]
    fn a_permanent_failure_alerts_at_once_and_a_transient_one_on_the_second() {
        assert_eq!(alert_due(None), None);
        let mut r = remote();
        assert_eq!(alert_due(Some(&r)), None);
        r.failure = Some(PushFailure::Unreachable);
        r.failures = 1;
        assert_eq!(alert_due(Some(&r)), None);
        r.failures = 2;
        assert_eq!(alert_due(Some(&r)), Some(2));
        for permanent in [
            PushFailure::Auth,
            PushFailure::HostKey,
            PushFailure::Rejected,
        ] {
            r.failure = Some(permanent);
            r.failures = 1;
            assert_eq!(alert_due(Some(&r)), Some(1), "{permanent:?}");
        }
    }

    /// What git 2.47 printed in a lab deployment's oxidized container (2026-10-23) and what each
    /// common host is documented to print.
    #[test]
    fn git_output_is_read_as_the_failure_it_describes() {
        let cases = [
            (
                "fatal: could not read Username for 'https://github.com': terminal prompts disabled",
                PushFailure::Auth,
            ),
            (
                "remote: Invalid username or token.\nfatal: Authentication failed for 'https://example.com/a.git/'",
                PushFailure::Auth,
            ),
            (
                "git@github.com: Permission denied (publickey).\nfatal: Could not read from remote repository.",
                PushFailure::Auth,
            ),
            (
                "remote: Repository not found.\nfatal: repository 'https://example.com/a.git/' not found",
                PushFailure::Auth,
            ),
            (
                "fatal: unable to access 'https://nonexistent.invalid/x.git/': Could not resolve host: nonexistent.invalid",
                PushFailure::Unreachable,
            ),
            (
                "ssh: connect to host example.com port 22: Connection timed out\nfatal: Could not read from remote repository.",
                PushFailure::Unreachable,
            ),
            (
                "Host key verification failed.\nfatal: Could not read from remote repository.",
                PushFailure::HostKey,
            ),
            (
                "To https://example.com/a.git\n ! [rejected]        HEAD -> main (fetch first)\nerror: failed to push some refs",
                PushFailure::Rejected,
            ),
            (
                " ! [remote rejected] HEAD -> main (protected branch hook declined)",
                PushFailure::Rejected,
            ),
            ("error: something nobody wrote down", PushFailure::Other),
        ];
        for (output, want) in cases {
            assert_eq!(classify_push_output(output), want, "{output}");
        }
    }

    #[test]
    fn only_the_three_url_shapes_are_accepted() {
        for ok in [
            "https://github.com/acme/configs.git",
            "https://git.example.com:8443/team/configs",
            "ssh://git@bitbucket.org/acme/configs.git",
            "ssh://git@[2001:db8::1]:2222/acme/configs.git",
            "git@gitlab.com:acme/configs.git",
            "git@git.example.com:/srv/git/configs.git",
        ] {
            assert!(check_url(ok).is_ok(), "{ok}");
        }
        for bad in [
            "",
            "http://github.com/acme/configs.git",
            "https://user:token@github.com/acme/configs.git",
            "file:///home/oxidized/x.git",
            "/home/oxidized/x.git",
            "github.com:acme/configs.git",
            "-oProxyCommand=evil@host:x",
            "https://github.com/acme/../../x",
            "https://github.com/acme/configs .git",
            "https://github.com",
            "git@host/x:y",
            "https://https://github.com/acme/configs.git",
            "https://github.com:/acme/configs.git",
            "https://github.com:0/acme/configs.git",
            "https://github.com:99999/acme/configs.git",
            "ssh://git@host:/acme/configs.git",
            "https://[2001:db8::1/acme/configs.git",
            "https://[nothex]/acme/configs.git",
        ] {
            assert!(check_url(bad).is_err(), "{bad}");
        }
        assert_eq!(
            host_of("ssh://git@bitbucket.org/acme/c.git"),
            Some("bitbucket.org")
        );
        assert_eq!(
            host_of("https://git.example.com:8443/t/c"),
            Some("git.example.com")
        );
        assert_eq!(host_of("git@gitlab.com:acme/c.git"), Some("gitlab.com"));
    }

    #[test]
    fn a_token_belongs_to_the_host_and_port_it_was_given_for() {
        let a = https_origin("https://GitHub.com/acme/a.git");
        assert_eq!(a.as_deref(), Some("https://github.com"));
        assert_eq!(a, https_origin("https://github.com/other/b.git"));
        assert_ne!(a, https_origin("https://github.com:8443/acme/a.git"));
        assert_ne!(a, https_origin("https://example.com/acme/a.git"));
        assert_eq!(https_origin("git@github.com:acme/a.git"), None);
    }

    #[test]
    fn a_branch_is_a_plain_name() {
        for ok in ["main", "backups/configs", "v1.2_x-y"] {
            assert!(check_branch(ok).is_ok(), "{ok}");
        }
        for bad in [
            "", "-f", ".hidden", "/main", "main/", "a..b", "a//b", "x.lock", "a b", "a:b", "a~1",
        ] {
            assert!(check_branch(bad).is_err(), "{bad}");
        }
    }

    /// GitHub's published ed25519 host key and fingerprint: the arithmetic `ssh-keygen -l` does.
    #[test]
    fn a_host_key_is_shown_as_the_fingerprint_ssh_prints() {
        let line = "github.com ssh-ed25519 \
                    AAAAC3NzaC1lZDI1NTE5AAAAIOMqqnkVzrm0SdG6UOoqKLsabgH5C9okWi0dh2l9GKJl";
        let kept = check_known_hosts(&format!("{line}\n\n")).expect("a valid line");
        assert_eq!(kept, format!("{line}\n"));
        assert_eq!(
            fingerprints(&kept),
            ["ssh-ed25519 SHA256:+DiY3wvvV6TuJJhbpZisF/zLDA0zPMSvHdkr4UvCOqU"]
        );
        for bad in [
            "",
            "github.com ssh-ed25519",
            "github.com rsa AAAA",
            "github.com ssh-ed25519 not*base64",
            "github.com ssh-ed25519 AAAA extra",
        ] {
            assert_eq!(check_known_hosts(bad), None, "{bad}");
        }
    }

    /// The layout of `PROTOCOL.key`, checked field by field; `ssh-keygen -y` reading the result
    /// back was checked on a lab deployment's oxidized container.
    #[test]
    fn a_generated_key_is_an_unencrypted_openssh_ed25519_key() {
        let (pem, public) = openssh_ed25519(&[7u8; 32], 0x0102_0304);
        assert!(pem.starts_with("-----BEGIN OPENSSH PRIVATE KEY-----\n"));
        assert!(pem.ends_with("-----END OPENSSH PRIVATE KEY-----\n"));
        assert!(pem.lines().all(|l| l.len() <= 70 || l.starts_with("-----")));
        let b64: String = pem.lines().filter(|l| !l.starts_with("-----")).collect();
        let body = data_encoding::BASE64
            .decode(b64.as_bytes())
            .expect("base64");
        assert!(body.starts_with(b"openssh-key-v1\0"));
        let (kind, blob, comment) = {
            let mut parts = public.split(' ');
            (
                parts.next().unwrap(),
                parts.next().unwrap(),
                parts.next().unwrap(),
            )
        };
        assert_eq!((kind, comment), ("ssh-ed25519", SSH_KEY_COMMENT));
        let blob = data_encoding::BASE64
            .decode(blob.as_bytes())
            .expect("base64");
        // The public blob is embedded verbatim after the header, and the private section is a
        // multiple of eight long and starts with the check word twice.
        let header = 15 + (4 + 4) + (4 + 4) + 4 + 4;
        assert_eq!(&body[header + 4..header + 4 + blob.len()], &blob[..]);
        let private_at = header + 4 + blob.len();
        let private_len =
            u32::from_be_bytes(body[private_at..private_at + 4].try_into().unwrap()) as usize;
        assert_eq!(private_len % 8, 0);
        assert_eq!(
            &body[private_at + 4..private_at + 12],
            &[1, 2, 3, 4, 1, 2, 3, 4]
        );
        assert_eq!(body.len(), private_at + 4 + private_len);
        // Two generated keys differ.
        assert_ne!(generate_ssh_key().1, generate_ssh_key().1);
    }

    #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
    #[ignore = "needs DATABASE_URL"]
    async fn a_destination_is_set_pushed_to_and_replaced(pool: PgPool) {
        const PUSHED: Pushed<'static> = Pushed {
            head: "a1",
            layout: "L",
            commit: "c1",
        };
        let repo = RemoteRepo::new(pool.clone());
        assert_eq!(repo.get().await.unwrap(), None);
        let first = crate::pgtest::credential(&pool, "git-a", "git_remote").await;
        let second = crate::pgtest::credential(&pool, "git-b", "git_remote").await;
        let settings = RemoteSettings {
            url: "ssh://git@example.com/acme/configs.git".to_owned(),
            branch: "main".to_owned(),
            auth: GitAuth::SshKey,
            credential_id: first,
            ssh_public_key: Some("ssh-ed25519 AAAA yagra-config-backup".to_owned()),
        };
        assert_eq!(repo.put(&settings).await.unwrap(), None);
        let r = repo.get().await.unwrap().expect("set");
        assert_eq!((r.generation, r.auth), (1, GitAuth::SshKey));

        // A push for this generation is recorded, with the host key it met.
        let kh = "example.com ssh-ed25519 AAAA\n";
        assert!(repo
            .record_push(1, None, Some(PushFailure::Unreachable), Some(kh))
            .await
            .unwrap());
        let r = repo.get().await.unwrap().unwrap();
        assert_eq!((r.failure, r.failures), (Some(PushFailure::Unreachable), 1));
        assert_eq!(r.known_hosts.as_deref(), Some(kh));
        assert!(repo
            .record_push(1, Some(&PUSHED), None, Some("other"))
            .await
            .unwrap());
        let r = repo.get().await.unwrap().unwrap();
        assert_eq!(
            (r.pushed_head.as_deref(), r.failure, r.failures),
            (Some("a1"), None, 0)
        );
        assert_eq!(
            (r.pushed_layout.as_deref(), r.pushed_commit.as_deref()),
            (Some("L"), Some("c1"))
        );
        assert_eq!(
            r.known_hosts.as_deref(),
            Some(kh),
            "the first keys stay pinned"
        );

        // Replacing it with the same URL keeps the keys, moves the generation and forgets the push.
        let replaced = RemoteSettings {
            credential_id: second,
            ..settings.clone()
        };
        assert_eq!(repo.put(&replaced).await.unwrap(), Some(first));
        let r = repo.get().await.unwrap().unwrap();
        assert_eq!((r.generation, r.pushed_head), (2, None));
        assert_eq!(r.known_hosts.as_deref(), Some(kh));
        // A push from before the change is not recorded against the new one.
        assert!(!repo
            .record_push(1, Some(&PUSHED), None, None)
            .await
            .unwrap());

        // A new URL forgets the keys; forgetting by hand moves the generation too.
        let moved = RemoteSettings {
            url: "git@example.org:acme/configs.git".to_owned(),
            ..replaced.clone()
        };
        assert_eq!(repo.put(&moved).await.unwrap(), None);
        assert_eq!(repo.get().await.unwrap().unwrap().known_hosts, None);
        assert!(repo.forget_host_keys().await.unwrap());
        assert_eq!(repo.get().await.unwrap().unwrap().generation, 4);

        assert_eq!(repo.delete().await.unwrap(), Some(second));
        assert_eq!(repo.get().await.unwrap(), None);
        assert!(!repo.forget_host_keys().await.unwrap());
    }
}
