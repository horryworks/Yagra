// SPDX-License-Identifier: AGPL-3.0-only
//! **Moving this whole deployment to another host** — the core-side half (ADR-121).
//!
//! A relocation copies everything: the KEK, the full PostgreSQL database, the metrics, optionally
//! the event and flow stores, the `.env` and the composition. The new host is a bare Linux box —
//! Docker is installed there if it is missing — and what runs there afterwards has the same keys,
//! the same users and the same history as what runs here.
//!
//! ## What this module is, and what it is not
//!
//! It is **stateless**, like [`crate::upgrade`] and for the same reason: core holds no Docker
//! socket, so it can neither build the archive nor restore it. What core does is write a request
//! and read back a status file. The work is a shell procedure in the `yagra-updater` sidecar's
//! `command:` block (`docker-compose.deploy.yml`), and the restore is
//! `scripts/yagra-relocate.sh`, which travels *inside the archive* so the new host runs the
//! procedure that shipped with the deployment being moved.
//!
//! It shares the hand-off volume with the upgrade mechanism and **nothing else**. Everything here
//! lives under [`SUBDIR`], and the status file is [`STATUS_FILE`], deliberately not `status.json`:
//! that name means "an upgrade run" to three readers, one of which (`settle_finished_run`) would
//! write an `upgrade succeeded` audit row for an upgrade that never happened.
//!
//! ## The secrets
//!
//! 🚨 A push carries an SSH password or private key, and possibly a sudo password. They are
//! **never** in the request file, the status file, the log, a trace or an audit row — the request
//! file's charset ([`crate::upgrade::is_request_value`]) could not hold most of them anyway. Core
//! writes them as 0600 files under `relocation/secret/` *before* the request appears; the sidecar
//! hands them to `ssh` and `sudo` as files and never reads them into a variable; and a `trap`
//! removes them however the run ends. [`clear_secrets`] is the belt to that braces — it runs at
//! startup, so a core that died mid-relocation does not leave them behind.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::upgrade::UpgradeRepo;

/// Everything this feature keeps lives under this directory of the hand-off volume.
pub const SUBDIR: &str = "relocation";
/// The one file that says what a relocation is doing. Deliberately not `status.json`.
pub const STATUS_FILE: &str = "relocation.json";
/// Free text from the procedure and from the restore on the other host.
pub const LOG_FILE: &str = "relocation.log";
/// Where the SSH and sudo secrets live for the length of one run, and nowhere else.
pub const SECRET_DIR: &str = "secret";

/// The most lines of the log any one request can ask for.
const MAX_LOG_LINES: usize = 2000;

/// What a relocation request is asking for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum RelocationMode {
    /// Build the archive and stop. The operator downloads it and carries it over themselves.
    #[default]
    Archive,
    /// Connect to the target, run the host checks there, report, and stop. Writes nothing on
    /// either machine — it is the button an operator presses before the one that matters.
    Preflight,
    /// The whole thing: check, install Docker if allowed, build, send, restore, verify.
    Push,
}

impl RelocationMode {
    /// The token written into the request file and into the status file.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Archive => "archive",
            Self::Preflight => "preflight",
            Self::Push => "push",
        }
    }

    /// Whether this mode needs somewhere to connect to.
    #[must_use]
    pub fn needs_target(self) -> bool {
        match self {
            Self::Archive => false,
            Self::Preflight | Self::Push => true,
        }
    }
}

/// How the operator authenticates to the target host.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum SshAuthKind {
    /// A password, handed to `ssh` through `SSH_ASKPASS` and to `sudo` on stdin.
    Password,
    /// An OpenSSH private key, handed to `ssh` as `-i`.
    Key,
}

impl SshAuthKind {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Password => "password",
            Self::Key => "key",
        }
    }
}

/// `relocation.json` as the sidecar writes it.
///
/// Every field but the first four is `#[serde(default)]`, for the reason
/// [`UpgradeRepo::read_json`](crate::upgrade) is lenient: a file written by a newer sidecar must
/// degrade to "less detail", never to a 500 on the one screen an operator has open while something
/// is already going wrong. It holds **no secret** — see the module doc.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct RelocationRun {
    /// The run id core minted.
    pub id: String,
    /// The mode the request asked for, as its token.
    pub mode: String,
    /// `requested` · `running` · `done` · `failed`.
    pub state: String,
    /// Which part of the work this is: `start`, `preflight`, `docker`, `backup`, `files`,
    /// `images`, `tier2`, `archive`, `push`, or `validate` for a request refused before it began.
    pub stage: String,
    /// What to show the operator about this stage.
    #[serde(default)]
    pub message: Option<String>,
    /// The host being moved to, absent for a plain archive.
    #[serde(default)]
    pub target_host: Option<String>,
    /// The target's SSH host-key fingerprint, once it has been seen.
    #[serde(default)]
    pub host_key_fingerprint: Option<String>,
    /// Whether this run installed Docker on the target.
    #[serde(default)]
    pub docker_installed: bool,
    /// Where the new deployment answers, once it does.
    #[serde(default)]
    pub target_url: Option<String>,
    /// The archive on this host, while there is one.
    #[serde(default)]
    pub filename: Option<String>,
    /// Its size in bytes.
    #[serde(default)]
    pub size_bytes: Option<u64>,
    /// Unix seconds.
    pub started_at: i64,
    /// Unix seconds, once it has ended.
    #[serde(default)]
    pub finished_at: Option<i64>,
    /// The account that asked for it.
    #[serde(default)]
    pub requested_by: Option<String>,
    /// Whether metrics were included.
    #[serde(default)]
    pub metrics: bool,
    /// Whether the three Yagra images were included.
    #[serde(default)]
    pub images: bool,
    /// Whether the event and flow stores were included.
    #[serde(default)]
    pub tier2: bool,
}

/// The two states that mean "something is happening on this host right now".
///
/// Spelled here once because three things ask it: the 409 that stops a second relocation, the 409
/// that stops an upgrade starting on top of one, and the refusal to delete an archive out from
/// under a run. `state_tokens_match_the_sidecar` pins the two strings to the procedure that
/// writes them.
pub const RUNNING_STATES: [&str; 2] = ["requested", "running"];

impl RelocationRun {
    /// Is this run still going?
    #[must_use]
    pub fn is_running(&self) -> bool {
        RUNNING_STATES.contains(&self.state.as_str())
    }
}

/// Where to connect, for the two modes that connect.
#[derive(Debug, Clone)]
pub struct SshTarget {
    pub host: String,
    pub port: u16,
    pub user: String,
    /// The directory to create under the target user's home. Relative by construction — a name,
    /// not a path.
    pub dir: String,
    pub auth: SshAuthKind,
}

/// What one request asks for, validated.
#[derive(Debug, Clone)]
pub struct RelocationOptions {
    pub mode: RelocationMode,
    pub include_metrics: bool,
    pub include_tier2: bool,
    pub include_images: bool,
    pub install_docker: bool,
    pub target: Option<SshTarget>,
}

/// The secrets one push needs, held for exactly as long as it takes to write them to disk.
///
/// **No `Debug`, deliberately.** A key provider must never have one either
/// ([`crate::secrets`]) — the derive is how a secret ends up in a log line nobody wrote.
pub struct SshSecrets {
    pub ssh_password: Option<String>,
    pub ssh_key: Option<String>,
    pub sudo_password: Option<String>,
}

impl SshSecrets {
    /// Whether there is anything to write at all.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.ssh_password.is_none() && self.ssh_key.is_none() && self.sudo_password.is_none()
    }

    /// `(file name, contents)` for each secret that was supplied.
    fn files(&self) -> Vec<(&'static str, &str)> {
        let mut out = Vec::new();
        if let Some(v) = self.ssh_password.as_deref() {
            out.push(("ssh_password", v));
        }
        if let Some(v) = self.ssh_key.as_deref() {
            out.push(("ssh_key", v));
        }
        if let Some(v) = self.sudo_password.as_deref() {
            out.push(("sudo_password", v));
        }
        out
    }
}

impl RelocationOptions {
    /// The `key=value` lines that go into the request file after the standard header.
    ///
    /// Every value here passes [`crate::upgrade::is_request_value`], which is asserted by
    /// `request_fields_pass_the_request_file_rules` rather than assumed — the file is read as root
    /// by the sidecar, and a value carrying a newline is one field forging another.
    #[must_use]
    pub fn to_request_fields(&self) -> Vec<(&'static str, String)> {
        let flag = |b: bool| if b { "1".to_owned() } else { "0".to_owned() };
        let mut out = vec![
            ("mode", self.mode.as_str().to_owned()),
            ("include_metrics", flag(self.include_metrics)),
            ("include_tier2", flag(self.include_tier2)),
            ("include_images", flag(self.include_images)),
            ("install_docker", flag(self.install_docker)),
        ];
        if let Some(t) = &self.target {
            out.push(("target_host", t.host.clone()));
            out.push(("target_port", t.port.to_string()));
            out.push(("target_user", t.user.clone()));
            out.push(("target_dir", t.dir.clone()));
            out.push(("auth", t.auth.as_str().to_owned()));
        }
        out
    }

    /// Refuse a request the sidecar would refuse, at the edge where it can still be explained.
    ///
    /// The charsets are the sidecar's own, character for character. That duplication is
    /// deliberate and is the same arrangement `bus` has: this side gives the operator a message,
    /// that side is the one that must be true, and neither is a substitute for the other.
    ///
    /// # Errors
    /// A message naming the field that is wrong.
    pub fn validate(&self) -> Result<(), &'static str> {
        let Some(t) = &self.target else {
            return if self.mode.needs_target() {
                Err("this mode needs a target host")
            } else {
                Ok(())
            };
        };
        if t.host.is_empty()
            || t.host.len() > 253
            || !t
                .host
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b".:-".contains(&b))
        {
            return Err("the target host is not a host name or an IP address");
        }
        if t.port == 0 {
            return Err("the target port must be between 1 and 65535");
        }
        let mut user = t.user.bytes();
        let first_ok = user
            .next()
            .is_some_and(|b| b.is_ascii_lowercase() || b == b'_');
        if !first_ok
            || t.user.len() > 32
            || !user.all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"_-".contains(&b))
        {
            return Err("the target user is not a Linux user name");
        }
        if t.dir.is_empty()
            || t.dir.len() > 64
            || !t
                .dir
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
        {
            return Err("the target directory must be a plain name, not a path");
        }
        Ok(())
    }
}

/// This deployment's relocation directory, when there is a hand-off volume at all.
#[must_use]
pub fn dir(repo: &UpgradeRepo) -> Option<PathBuf> {
    Some(repo.hand_off_dir()?.join(SUBDIR))
}

/// The current run, or `None` when there has never been one.
#[must_use]
pub fn status(repo: &UpgradeRepo) -> Option<RelocationRun> {
    read_status(dir(repo)?.as_path())
}

fn read_status(dir: &Path) -> Option<RelocationRun> {
    let text = std::fs::read_to_string(dir.join(STATUS_FILE)).ok()?;
    match serde_json::from_str(&text) {
        Ok(v) => Some(v),
        Err(e) => {
            // Same leniency, and the same reason, as the upgrade state files: a status written by
            // a newer sidecar reads as "nothing to report", never as a broken page.
            tracing::warn!(error = %e, "unreadable relocation state file");
            None
        }
    }
}

/// Is a relocation happening right now?
///
/// Asked by the relocation endpoints *and* by the upgrade edge — the two must not run at once, in
/// either order, because both drive `docker compose` on this deployment.
#[must_use]
pub fn is_running(repo: &UpgradeRepo) -> bool {
    status(repo).is_some_and(|r| r.is_running())
}

/// The last `tail` lines of this run's log, newest last.
#[must_use]
pub fn log_tail(repo: &UpgradeRepo, tail: usize) -> Vec<String> {
    let Some(dir) = dir(repo) else {
        return Vec::new();
    };
    let tail = tail.clamp(1, MAX_LOG_LINES);
    let Ok(text) = std::fs::read_to_string(dir.join(LOG_FILE)) else {
        return Vec::new();
    };
    let lines: Vec<&str> = text.lines().collect();
    lines[lines.len().saturating_sub(tail)..]
        .iter()
        .map(|s| (*s).to_owned())
        .collect()
}

/// Is this the name of an archive this deployment built?
///
/// The name is the only thing standing between a download endpoint and the rest of the volume, so
/// it is matched rather than trusted: `yagra-relocation-<20 chars of UTC stamp>.tar.gz` and
/// nothing else. A `..` or a `/` fails on the first character it reaches.
#[must_use]
pub fn is_archive_name(name: &str) -> bool {
    let Some(rest) = name.strip_prefix("yagra-relocation-") else {
        return false;
    };
    let Some(stamp) = rest.strip_suffix(".tar.gz") else {
        return false;
    };
    // `YYYYMMDDTHHMMSSZ`
    stamp.len() == 16
        && stamp.as_bytes()[8] == b'T'
        && stamp.as_bytes()[15] == b'Z'
        && stamp
            .bytes()
            .enumerate()
            .all(|(i, b)| i == 8 || i == 15 || b.is_ascii_digit())
}

/// The archive waiting on this host: its path, size and modification time.
///
/// There is at most one — the procedure removes the previous one before it writes a new one — but
/// this picks the newest rather than assuming, because "there happen to be two" must not become
/// "serve whichever the directory listed first".
#[must_use]
pub fn archive(repo: &UpgradeRepo) -> Option<(PathBuf, u64, i64)> {
    let dir = dir(repo)?;
    let mut best: Option<(PathBuf, u64, i64)> = None;
    for entry in std::fs::read_dir(&dir).ok()?.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if !is_archive_name(name) {
            continue;
        }
        let Ok(meta) = entry.metadata() else { continue };
        if !meta.is_file() {
            continue;
        }
        let modified = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(0));
        if best.as_ref().is_none_or(|(_, _, m)| modified >= *m) {
            best = Some((entry.path(), meta.len(), modified));
        }
    }
    best
}

/// Delete the archive, if there is one. `Ok(false)` means there was nothing to delete.
///
/// # Errors
/// The file exists and could not be removed.
pub fn delete_archive(repo: &UpgradeRepo) -> std::io::Result<bool> {
    let Some((path, _, _)) = archive(repo) else {
        return Ok(false);
    };
    std::fs::remove_file(path)?;
    Ok(true)
}

/// Write the SSH and sudo secrets a push needs, 0600, into `relocation/secret/`.
///
/// Called **before** the request file exists, so the sidecar can never see a request whose
/// secrets have not landed. Any previous run's secrets are removed first: a stale password left
/// behind is one this run would silently authenticate with.
///
/// # Errors
/// The directory or a file could not be written.
pub fn write_secrets(repo: &UpgradeRepo, secrets: &SshSecrets) -> anyhow::Result<()> {
    let dir = dir(repo).ok_or_else(|| anyhow::anyhow!("the upgrade mechanism is not enabled"))?;
    write_secrets_in(&dir, secrets)
}

/// [`write_secrets`] against a directory, so it is testable without a database.
pub(crate) fn write_secrets_in(dir: &Path, secrets: &SshSecrets) -> anyhow::Result<()> {
    let sec = dir.join(SECRET_DIR);
    let _ = std::fs::remove_dir_all(&sec);
    std::fs::create_dir_all(&sec)?;
    restrict(&sec, 0o700);
    for (name, value) in secrets.files() {
        let path = sec.join(name);
        write_private(&path, value)?;
    }
    Ok(())
}

/// Remove whatever secrets are lying around.
///
/// Called from two places, and the second is the interesting one: `run_live`, at startup. A core
/// that was killed between writing the secrets and the sidecar's `trap` firing would otherwise
/// leave a password on the volume until the next relocation overwrote it.
pub fn clear_secrets(repo: &UpgradeRepo) {
    let Some(dir) = dir(repo) else { return };
    match std::fs::remove_dir_all(dir.join(SECRET_DIR)) {
        Ok(()) => tracing::info!("removed relocation secrets left behind by an earlier run"),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => tracing::warn!(error = %e, "could not remove the relocation secrets"),
    }
}

#[cfg(unix)]
fn write_private(path: &Path, contents: &str) -> anyhow::Result<()> {
    use std::io::Write as _;
    use std::os::unix::fs::OpenOptionsExt as _;
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?;
    f.write_all(contents.as_bytes())?;
    Ok(())
}

#[cfg(not(unix))]
fn write_private(path: &Path, contents: &str) -> anyhow::Result<()> {
    // Windows is a development platform for this crate, not a deployment one — the whole mechanism
    // needs a Docker socket. The file is still written so the flow can be exercised.
    std::fs::write(path, contents)?;
    Ok(())
}

#[cfg(unix)]
fn restrict(path: &Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt as _;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode));
}

#[cfg(not(unix))]
fn restrict(_path: &Path, _mode: u32) {}

/// Ask the sidecar for a relocation.
///
/// The last thing between an HTTP request and a file a root-privileged container reads, which is
/// why it re-validates rather than trusting the caller.
///
/// # Errors
/// An invalid option, or anything [`UpgradeRepo::request_with`] can fail on.
pub fn request(
    repo: &UpgradeRepo,
    id: &str,
    requested_by: &str,
    now: i64,
    opts: &RelocationOptions,
) -> anyhow::Result<()> {
    opts.validate().map_err(|e| anyhow::anyhow!("{e}"))?;
    let fields = opts.to_request_fields();
    let borrowed: Vec<(&str, &str)> = fields.iter().map(|(k, v)| (*k, v.as_str())).collect();
    repo.request_with(
        crate::upgrade::Command::Relocate,
        id,
        None,
        requested_by,
        now,
        &borrowed,
    )
}

/// Where the composition mounts the metrics volume into core, read-only.
///
/// It is there for the disk watcher (`YAGRA_DISK_WATCH_PATHS`), and this borrows it rather than
/// asking for a mount of its own: core has no business being able to *write* to `vmdata`, and a
/// second mount would be a second thing to keep in step.
///
/// `None` when it is not there — a development run, or a composition that does not mount it — and
/// then the estimate simply omits the metrics rather than guessing at them.
#[must_use]
pub fn metrics_dir() -> Option<PathBuf> {
    let path = PathBuf::from(VM_MOUNT_IN_CORE);
    path.is_dir().then_some(path)
}

/// The path in [`metrics_dir`], pinned to the composition by
/// `the_metrics_mount_this_reads_is_the_one_the_composition_makes`.
const VM_MOUNT_IN_CORE: &str = "/hostfs/vm";

/// How much free space building an archive of `estimate` bytes needs.
///
/// Twice, for the same reason the sidecar's own check doubles: the backup lands on the volume and
/// then the tar of it lands beside it. The sidecar's measurement is the real one — this is what
/// the page shows *before* anything is asked for, so it may be an estimate, but it must not be an
/// optimistic one.
#[must_use]
pub fn space_needed(estimate: u64) -> u64 {
    estimate.saturating_mul(2)
}

/// Roughly how large the archive will be: the database, plus the metrics when they are included.
///
/// ⚠️ **Deliberately partial, and the UI says so.** The event and flow stores and the image
/// archive are not counted — core cannot see those volumes, only the sidecar can. It is a number
/// to plan with, not the one that decides: the check that can stop a run is the sidecar's, taken
/// against the real volumes at the moment it starts.
pub async fn estimate_bytes(
    pool: &sqlx::PgPool,
    vm_dir: Option<&Path>,
    opts: &RelocationOptions,
) -> Option<u64> {
    let db: i64 = sqlx::query_scalar("SELECT pg_database_size(current_database())")
        .fetch_one(pool)
        .await
        .ok()?;
    let mut total = u64::try_from(db).unwrap_or(0);
    if opts.include_metrics {
        if let Some(dir) = vm_dir {
            let dir = dir.to_path_buf();
            if let Ok(bytes) = tokio::task::spawn_blocking(move || dir_size(&dir)).await {
                total = total.saturating_add(bytes);
            }
        }
    }
    Some(total)
}

/// Recursive size of a directory, ignoring anything it cannot read.
fn dir_size(dir: &Path) -> u64 {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    let mut total = 0u64;
    for entry in entries.flatten() {
        let Ok(meta) = entry.metadata() else { continue };
        if meta.is_dir() {
            total = total.saturating_add(dir_size(&entry.path()));
        } else {
            total = total.saturating_add(meta.len());
        }
    }
    total
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::upgrade::tests::{docker_run_invocations, without_comments};

    fn compose() -> String {
        std::fs::read_to_string("../../docker-compose.deploy.yml")
            .expect("the deploy composition holds the updater's script")
    }

    /// The restore that runs on the *other* host, comments stripped for the usual reason.
    fn restore_script() -> String {
        let raw = std::fs::read_to_string("../../scripts/yagra-relocate.sh")
            .expect("the restore script ships beside the composition");
        without_comments(&raw)
    }

    /// A local overlay may name things that exist only on the host it came from, so it is checked
    /// before it is obeyed — and set aside, never deleted.
    ///
    /// 🚨 Measured 2026-09-08: the source declared the external network `yagra-sim`, and the
    /// restore failed on `docker compose up` — its very last command — **after** the key, the
    /// database, the metrics and both tier-2 stores were already in place. Everything irreversible
    /// had succeeded and the deployment still would not start.
    #[test]
    fn an_overlay_naming_a_network_this_host_lacks_is_set_aside_before_anything_starts() {
        let s = restore_script();
        let check = s
            .find("docker network inspect")
            .expect("the restore checks the overlay's external networks against this host");
        assert!(
            s.contains("docker-compose.local.yml.needs-review"),
            "the overlay must be renamed, not deleted — it is the operator's own configuration"
        );
        assert!(
            !s.contains("rm -f docker-compose.local.yml")
                && !s.contains("rm docker-compose.local.yml"),
            "the overlay is set aside, never removed"
        );
        for (needle, what) in [
            ("docker load -i images.tar", "loading the images"),
            ("dc create", "creating the volumes"),
            ("pg_restore", "restoring the database"),
        ] {
            let at = s
                .find(needle)
                .unwrap_or_else(|| panic!("the restore still performs {what}"));
            assert!(
                check < at,
                "the overlay is checked after {what}; the point is to find this out before the \
                 irreversible half, not after it"
            );
        }
    }

    /// Images that arrived in the archive must leave the new host able to start itself again.
    ///
    /// 🚨 `pull_policy: always` plus a `YAGRA_IMAGE_REPO` that answered only on the *old* host is a
    /// deployment that runs exactly once — the restore's own `--pull missing` is the only command
    /// that ever works. The operator's next restart, their next upgrade and any compose line they
    /// type by hand all fail on a deployment that is complete, running and looks healthy.
    /// Measured 2026-09-08 relocating off `localhost:5000`.
    #[test]
    fn carrying_the_images_leaves_a_host_that_can_start_itself() {
        let c = compose();
        let policies = c.matches("pull_policy:").count();
        assert!(
            policies >= 4,
            "only {policies} pull_policy lines found; the scan is looking in the wrong place"
        );
        assert_eq!(
            c.matches("pull_policy: ${YAGRA_PULL_POLICY:-always}").count(),
            policies,
            "a pull_policy is still hardcoded, so a relocated deployment cannot be told to use the \
             images it already has"
        );

        let s = restore_script();
        let load = s
            .find("docker load -i images.tar")
            .expect("the restore loads the archive's images");
        let pin = s
            .find("YAGRA_PULL_POLICY=missing")
            .expect("loading images pins a pull policy this host can satisfy");
        assert!(
            load < pin,
            "the policy is pinned before the images are loaded"
        );
        assert!(
            s[load..].contains("grep -v '^YAGRA_PULL_POLICY='"),
            "the pin appends without removing what is there, so a policy already in .env survives \
             — which is the defect this exists to fix"
        );
    }

    /// The relocation procedure's body, with its comment lines removed.
    ///
    /// Cut the same way `updater_body_without_its_procedures` cuts, and for the same reason every
    /// source-text check here strips comments first: the prose explaining why a defect was avoided
    /// reads, to a substring search, exactly like the defect.
    fn relocate_procedure(compose: &str) -> String {
        let open = "<<'RELOCATE'";
        let close = "\n        RELOCATE\n";
        let start = compose
            .find(open)
            .expect("the RELOCATE procedure is in this file");
        let end = compose[start..]
            .find(close)
            .map(|i| start + i)
            .expect("the RELOCATE heredoc ends where this expects");
        let body = &compose[start + open.len()..end];
        assert!(
            body.len() > 4_000,
            "only {} bytes of relocation procedure found; the slice has stopped matching and \
             every check below would pass over nothing",
            body.len()
        );
        without_comments(body)
    }

    /// The `relocate)` arm of the sidecar's command `case`, comments removed.
    fn relocate_arm(compose: &str) -> String {
        let body = without_comments(compose);
        let start = body
            .find("            relocate)")
            .expect("the sidecar has a relocate arm");
        let end = body[start..]
            .find("\n              ;;")
            .map(|i| start + i)
            .expect("the relocate arm is terminated");
        body[start..end].to_owned()
    }

    #[test]
    fn mode_and_auth_tokens_round_trip_through_serde() {
        for mode in [
            RelocationMode::Archive,
            RelocationMode::Preflight,
            RelocationMode::Push,
        ] {
            let json = serde_json::to_string(&mode).unwrap();
            assert_eq!(json, format!("\"{}\"", mode.as_str()));
            assert_eq!(
                serde_json::from_str::<RelocationMode>(&json).unwrap(),
                mode,
                "the token the sidecar is sent must be the token serde reads back"
            );
        }
        for kind in [SshAuthKind::Password, SshAuthKind::Key] {
            let json = serde_json::to_string(&kind).unwrap();
            assert_eq!(json, format!("\"{}\"", kind.as_str()));
            assert_eq!(serde_json::from_str::<SshAuthKind>(&json).unwrap(), kind);
        }
    }

    fn push_options() -> RelocationOptions {
        RelocationOptions {
            mode: RelocationMode::Push,
            include_metrics: true,
            include_tier2: true,
            include_images: false,
            install_docker: true,
            target: Some(SshTarget {
                host: "192.0.2.10".to_owned(),
                port: 22,
                user: "ubuntu".to_owned(),
                dir: "yagra".to_owned(),
                auth: SshAuthKind::Password,
            }),
        }
    }

    /// The request file is read as root by a container holding the Docker socket. Every field this
    /// module writes has to be one `request_with` will accept — otherwise the failure surfaces as
    /// a 500 on a request the operator has already been told was accepted.
    #[test]
    fn request_fields_pass_the_request_file_rules() {
        for opts in [
            push_options(),
            RelocationOptions {
                mode: RelocationMode::Archive,
                target: None,
                ..push_options()
            },
        ] {
            let fields = opts.to_request_fields();
            assert!(fields.len() >= 5, "the option flags are always written");
            for (key, value) in &fields {
                assert!(
                    crate::upgrade::is_request_key(key),
                    "`{key}` is not a field name request_with will write"
                );
                assert!(
                    crate::upgrade::is_request_value(value),
                    "`{key}={value}` is not a value request_with will write"
                );
            }
        }
    }

    #[test]
    fn a_target_the_sidecar_would_refuse_is_refused_here_first() {
        assert!(push_options().validate().is_ok());
        let bad = |f: fn(&mut SshTarget)| {
            let mut o = push_options();
            f(o.target.as_mut().unwrap());
            o.validate()
        };
        assert!(bad(|t| t.host = "a host".to_owned()).is_err());
        assert!(bad(|t| t.host = String::new()).is_err());
        assert!(bad(|t| t.user = "Root".to_owned()).is_err());
        assert!(bad(|t| t.user = "1st".to_owned()).is_err());
        assert!(bad(|t| t.dir = "../etc".to_owned()).is_err());
        assert!(bad(|t| t.dir = "a/b".to_owned()).is_err());
        assert!(bad(|t| t.port = 0).is_err());
        // A push with nowhere to push to is the one shape that is refused for a missing field
        // rather than a malformed one.
        let mut nowhere = push_options();
        nowhere.target = None;
        assert!(nowhere.validate().is_err());
        let archive = RelocationOptions {
            mode: RelocationMode::Archive,
            target: None,
            ..push_options()
        };
        assert!(archive.validate().is_ok(), "an archive needs no target");
    }

    /// The download endpoint serves whatever this accepts, so it accepts a name and never a path.
    #[test]
    fn an_archive_name_outside_the_pattern_is_refused() {
        assert!(is_archive_name("yagra-relocation-20260908T101530Z.tar.gz"));
        for bad in [
            "yagra-relocation-x.tar.gz",
            "yagra-relocation-20260908T101530Z.tar",
            "../yagra-relocation-20260908T101530Z.tar.gz",
            "yagra-relocation-/2609/8T101530Z.tar.gz",
            "yagra-backup-20260908T101530Z.tar.gz",
            "yagra-relocation-20260908T101530.tar.gz",
        ] {
            assert!(!is_archive_name(bad), "`{bad}` must not be servable");
        }
    }

    #[test]
    fn only_a_well_named_archive_is_found_on_disk() {
        let dir = std::env::temp_dir().join(format!("yagra-reloc-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("yagra-relocation-nope.tar.gz"), b"x").unwrap();
        std::fs::write(dir.join("notes.txt"), b"x").unwrap();
        assert!(find_archive(&dir).is_none());
        let good = dir.join("yagra-relocation-20260908T101530Z.tar.gz");
        std::fs::write(&good, b"hello").unwrap();
        let (path, size, _) = find_archive(&dir).expect("the well-named one is found");
        assert_eq!(path, good);
        assert_eq!(size, 5);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// [`archive`] without an `UpgradeRepo`, which needs a pool. Same body, one indirection.
    fn find_archive(dir: &Path) -> Option<(PathBuf, u64, i64)> {
        let mut best: Option<(PathBuf, u64, i64)> = None;
        for entry in std::fs::read_dir(dir).ok()?.flatten() {
            let name = entry.file_name();
            let Some(name) = name.to_str() else { continue };
            if !is_archive_name(name) {
                continue;
            }
            let Ok(meta) = entry.metadata() else { continue };
            best = Some((entry.path(), meta.len(), 0));
        }
        best
    }

    /// The secrets land as files with no read access for anyone else, and — the half that matters —
    /// nothing about them reaches the request file.
    #[test]
    fn secrets_land_as_private_files_and_never_in_the_request() {
        let dir = std::env::temp_dir().join(format!("yagra-reloc-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let secrets = SshSecrets {
            ssh_password: Some("hunter2".to_owned()),
            ssh_key: None,
            sudo_password: Some("hunter3".to_owned()),
        };
        assert!(!secrets.is_empty());
        write_secrets_in(&dir, &secrets).unwrap();
        let sec = dir.join(SECRET_DIR);
        assert_eq!(
            std::fs::read_to_string(sec.join("ssh_password")).unwrap(),
            "hunter2"
        );
        assert_eq!(
            std::fs::read_to_string(sec.join("sudo_password")).unwrap(),
            "hunter3"
        );
        assert!(
            !sec.join("ssh_key").exists(),
            "a secret that was not supplied must not be written as an empty file — ssh would then \
             offer an empty key instead of falling back to the password"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = std::fs::metadata(sec.join("ssh_password"))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600, "the SSH password must be 0600");
        }
        // A second run's secrets replace the first's rather than joining them.
        write_secrets_in(
            &dir,
            &SshSecrets {
                ssh_password: Some("later".to_owned()),
                ssh_key: None,
                sudo_password: None,
            },
        )
        .unwrap();
        assert!(
            !sec.join("sudo_password").exists(),
            "a previous run's sudo password left behind is one this run would authenticate with"
        );
        // Nothing that could carry a secret is in what the request file would hold.
        let written = push_options().to_request_fields();
        for (_, value) in &written {
            assert!(
                !value.contains("hunter"),
                "a secret reached the request file"
            );
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The refusal path writes the relocation's own file and never the upgrade's.
    ///
    /// `status.json` means "an upgrade run" to three readers, and the worst of them is core's
    /// `settle_finished_run`: it would claim a refused relocation, write an `upgrade failed` audit
    /// row for an upgrade nobody asked for, and close a maintenance window it never opened. Modelled
    /// on `the_refresh_arm_writes_no_status_file`, which exists for exactly this.
    #[test]
    fn the_relocate_arm_writes_no_status_file() {
        let arm = relocate_arm(&compose());
        assert!(
            arm.contains("relocation.json"),
            "the relocate arm writes no state at all"
        );
        assert!(
            arm.contains("reject_relocation"),
            "the relocate arm must refuse through its own writer"
        );
        for forbidden in ["status.json", "reject ", "say "] {
            assert!(
                !arm.contains(forbidden),
                "the relocate arm touches `{forbidden}`, which belongs to the upgrade mechanism \
                 and is claimed by core's settle_finished_run:\n{arm}"
            );
        }
    }

    /// The backup is the procedure of record, not a second copy of it (ADR-050 decision 9).
    #[test]
    fn the_relocate_procedure_runs_the_backup_of_record() {
        let body = relocate_procedure(&compose());
        assert!(
            body.contains("/usr/share/yagra/yagra-backup.sh"),
            "the relocation must take its backup with the script that ships in the core image"
        );
        assert!(
            body.contains("backup/kek/kek"),
            "nothing checks that the archive carries the KEK, so a relocation could deliver a \
             database of secrets nobody can ever open"
        );
    }

    /// The archive is written under a temporary name, made readable by core, and only then moved
    /// into place. The other order hands core a file it cannot open, or one that is still growing.
    #[test]
    fn the_relocate_procedure_hands_the_archive_to_core() {
        let body = relocate_procedure(&compose());
        let chown = body
            .find(r#"chown 10001:10001 "$$R/$$FILE.tmp""#)
            .expect("the archive is chowned to core before it is published");
        let publish = body
            .find(r#"mv "$$R/$$FILE.tmp" "$$R/$$FILE""#)
            .expect("the archive is moved into place");
        assert!(
            chown < publish,
            "the archive is published before core can read it, so the download endpoint would \
             serve a permission error"
        );
        assert!(
            body.contains(r#"chmod 0600 "$$R/$$FILE.tmp""#),
            "the archive holds the KEK and every credential; it must not be world-readable"
        );
    }

    /// The version pin must overwrite what `.env` already says, never defer to it.
    ///
    /// 🚨 A deployment's `.env` routinely carries a stale pin — `/flashdeploy` passes
    /// `YAGRA_IMAGE_TAG` on the command line and never writes it back — so a `pin_env` that skipped
    /// an existing key sent the new host after a version this one has not run for weeks. Measured
    /// 2026-09-09: the archive named `7da517f8` while core was `139e279f`, and the target's pull
    /// printed both in one line. Decision 6 says the new host runs exactly what this one runs, and
    /// "unless .env disagrees" is not a reading of that.
    #[test]
    fn the_version_pin_replaces_what_the_env_already_says() {
        let body = relocate_procedure(&compose());
        let start = body
            .find("pin_env()")
            .expect("the procedure pins the version");
        let end = start
            + body[start..]
                .find("\n        }")
                .expect("pin_env is a closed function");
        let f = &body[start..end];
        assert!(
            f.contains(r#"grep -v "^$$1=""#),
            "pin_env does not remove the existing line, so a stale pin in .env survives into the \
             archive and the new host comes up on the wrong version:\n{f}"
        );
        assert!(
            !f.contains(r#"grep -q "^$$1=""#),
            "pin_env still defers to an existing key; that is the defect, not the guard:\n{f}"
        );
        assert!(
            f.contains(r#"chmod 600 "$$WORK/.env""#),
            ".env holds POSTGRES_PASSWORD; rewriting it must not widen its mode:\n{f}"
        );
    }

    /// A private registry named `localhost:*` cannot be reached from anywhere but this host, so
    /// carrying no images is a combination with no successful outcome — and it must be refused
    /// before the work, not after it.
    ///
    /// 🚨 Measured 2026-09-09: without this the run took the tier-1 backup, **stopped the event and
    /// flow stores** to copy them, built a 730 MB archive, pushed it over SSH, and failed on the
    /// target's first pull. Every one of those steps is minutes and the stop is a real gap in
    /// ingest, all spent on a request that could not have succeeded.
    #[test]
    fn a_registry_only_this_host_can_reach_is_refused_before_any_work() {
        let body = relocate_procedure(&compose());
        let refusal = body
            .find("localhost:*|127.0.0.1:*")
            .expect("the procedure refuses a host-local registry when images are not carried");
        for (needle, what) in [
            (r#"sshx true"#, "the first SSH"),
            (r#"yagra-backup.sh"#, "the backup"),
            (
                r#"dc stop victorialogs"#,
                "stopping the event and flow stores",
            ),
            (r#"-czf "$$R/$$FILE.tmp""#, "building the archive"),
        ] {
            let at = body
                .find(needle)
                .unwrap_or_else(|| panic!("the procedure still performs {what}"));
            assert!(
                refusal < at,
                "the host-local-registry refusal comes after {what}; a request that cannot succeed \
                 would pay for it first"
            );
        }
        assert!(
            body[refusal..refusal + 400].contains(r#"[ "$$IMAGES" = 1 ] ||"#),
            "the refusal does not depend on the images option, so it would also refuse the \
             combination that works"
        );
    }

    /// The disk check runs before the first byte is written, which is the whole point of it: a host
    /// that is already short of space must not be given several GB and then told it failed.
    #[test]
    fn the_relocate_disk_check_runs_before_anything_is_written() {
        let body = relocate_procedure(&compose());
        let check = body.find("df -P -k").expect("there is a free-space check");
        let write = body
            .find(r#"mkdir -p "$$WORK""#)
            .expect("the work directory is created");
        assert!(
            check < write,
            "the free-space check runs after the work has started, which is the failure it exists \
             to prevent"
        );
    }

    /// N/N-1: core asks only when the sidecar says it understands the command.
    #[test]
    fn the_sidecar_declares_relocation_in_its_heartbeat() {
        let compose = compose();
        let start = compose
            .find("        heartbeat() {")
            .expect("the sidecar writes a heartbeat");
        let end = compose[start..]
            .find("\n        }")
            .map(|i| start + i)
            .expect("the heartbeat function is terminated");
        assert!(
            compose[start..end].contains(r#""relocate":true"#),
            "this sidecar has the relocate arm but does not say so, so core will refuse every \
             relocation with 503 relocation_unsupported"
        );
    }

    /// However the run ends, the secrets go with it.
    #[test]
    fn the_relocate_procedure_deletes_its_secrets_on_exit() {
        let body = relocate_procedure(&compose());
        assert!(
            body.contains("trap cleanup EXIT"),
            "nothing removes the secrets when the procedure exits early"
        );
        let cleanup = body
            .find("cleanup() {")
            .expect("there is a cleanup function");
        let end = body[cleanup..]
            .find('}')
            .map(|i| cleanup + i)
            .expect("cleanup is terminated");
        assert!(
            body[cleanup..end].contains(r#"rm -rf "$$SEC""#),
            "cleanup does not remove the secret directory: {}",
            &body[cleanup..end]
        );
        // The refusal path never reaches the procedure, so it has to remove them itself.
        assert!(
            relocate_arm(&compose()).contains(r#"rm -rf "$$D/relocation/secret""#)
                || without_comments(&compose()).contains(r#"rm -rf "$$D/relocation/secret""#),
            "a refused request leaves core's staged secrets on the volume forever"
        );
    }

    /// 🚨 The rule this feature turns on: a secret is a **file**, passed to `ssh` and `sudo` as a
    /// file. Reading one into a shell variable puts it in the process table, in `set -x` output and
    /// one careless `echo` away from the log — and the log is shown in the WebUI.
    ///
    /// ⚠️ The needles are all negations, so both consumers are asserted **present** as well. A
    /// procedure that had stopped using the secrets entirely would satisfy every negation.
    #[test]
    fn the_relocate_procedure_never_reads_a_secret_into_a_variable() {
        let body = relocate_procedure(&compose());
        // The accept side: the two ways a secret is legitimately consumed.
        assert!(
            body.contains(r#"printf '#!/bin/sh\ncat %s/ssh_password\n' "$$SEC""#),
            "the askpass helper is gone, so every negation below passes over nothing"
        );
        assert!(
            body.contains(r#"< "$$SUDOPW""#),
            "sudo is no longer fed its password on stdin, so every negation below passes over \
             nothing"
        );
        for forbidden in [
            r#"$$(cat "$$SEC"#,
            "$$(cat $$SEC",
            r#"$$(cat "$$SUDOPW"#,
            "$$(cat $$SUDOPW",
            "read -r SSHPW",
            "PASSWORD=$$",
        ] {
            assert!(
                !body.contains(forbidden),
                "`{forbidden}` reads a secret into a shell variable, which puts it in the process \
                 table and one `echo` away from a log the WebUI displays"
            );
        }
        // And nothing echoes the secret directory's contents.
        for line in body.lines() {
            let line = line.trim();
            assert!(
                !(line.starts_with("echo") && line.contains("$$SEC")),
                "this line echoes something out of the secret directory: {line}"
            );
        }
    }

    /// Installing Docker on someone else's machine is a root action, so it happens only when it was
    /// asked for and only when Docker is actually missing.
    #[test]
    fn docker_installation_is_gated_on_the_option() {
        let body = relocate_procedure(&compose());
        let gate = body
            .find(r#"[ "$$INSTALL_DOCKER" = 1 ]"#)
            .expect("the install option is consulted");
        let install = body
            .find("wget -qO /tmp/get-docker.sh")
            .expect("the installer is fetched");
        assert!(
            gate < install,
            "get.docker.com is fetched before the option that permits it is read"
        );
        // Both the fetch and the run sit inside a branch taken only when docker is not ok.
        let branch = body
            .find(r#"if [ "$$(pf docker)" != ok ]"#)
            .expect("the install is behind a docker-is-missing test");
        assert!(
            branch < gate,
            "the option is read before the host has even been asked whether it has Docker"
        );
        assert!(
            body.contains("sudox 'sh /tmp/get-docker.sh'"),
            "the installer must run through the sudo helper, which is the only thing that feeds a \
             password without putting it in an argument"
        );
    }

    /// Both image build paths ship the restore script, and `/flashdeploy` uses the one CI never
    /// evaluates. Forgetting it there is invisible until a relocation is attempted on a flash box.
    #[test]
    fn the_image_ships_the_relocation_script_on_both_build_paths() {
        let dockerfile = std::fs::read_to_string("../../docker/yagra-rust.Dockerfile")
            .expect("the Rust image Dockerfile is beside this crate");
        for needle in ["yagra-relocate.sh", "RELOCATION-README.md"] {
            let n = dockerfile.matches(needle).count();
            assert!(
                n >= 3,
                "`{needle}` appears {n} times in the Dockerfile; it needs the build stage's `cp`, \
                 the prebuilt stage's COPY and the runtime stage's COPY --from=bins"
            );
        }
        assert!(
            dockerfile.contains("openssh-client"),
            "the core image carries the ssh client the relocation runs (ADR-121 decision 13)"
        );
        let flash = std::fs::read_to_string("../../scripts/flash-build.sh")
            .expect("the flash build script is in scripts/");
        for needle in ["yagra-relocate.sh", "RELOCATION-README.md"] {
            assert!(
                flash.matches(needle).count() >= 2,
                "`{needle}` must be staged by flash-build.sh AND listed in its closing `ls -l`, \
                 which is what turns a missing file into a failed build rather than a core image \
                 that cannot relocate"
            );
        }
    }

    /// The states core branches on are the states the procedure writes.
    #[test]
    fn state_tokens_match_the_sidecar() {
        let compose = compose();
        let arm = relocate_arm(&compose);
        assert!(
            arm.contains(r#""state":"requested""#),
            "the arm no longer writes the state core waits on"
        );
        assert!(
            relocate_procedure(&compose).contains("rsay running "),
            "the procedure no longer reports the running state core polls on"
        );
        for state in RUNNING_STATES {
            let run = RelocationRun {
                id: "x".to_owned(),
                mode: "push".to_owned(),
                state: state.to_owned(),
                stage: "start".to_owned(),
                message: None,
                target_host: None,
                host_key_fingerprint: None,
                docker_installed: false,
                target_url: None,
                filename: None,
                size_bytes: None,
                started_at: 0,
                finished_at: None,
                requested_by: None,
                metrics: false,
                images: false,
                tier2: false,
            };
            assert!(run.is_running(), "`{state}` must read as still running");
        }
    }

    /// The relocation container gets the same three mounts every other launch does — and the third
    /// is the one that made "Accept remote pollers" fail for its whole life (ADR-065 Inc.5).
    #[test]
    fn the_relocation_launches_are_mounted_like_every_other() {
        let compose = compose();
        let relocation_launches: Vec<String> = docker_run_invocations(&compose)
            .into_iter()
            .filter(|l| l.contains("yagra-relocate-") || l.contains("$$CORE_IMG"))
            .collect();
        assert!(
            relocation_launches.len() >= 3,
            "expected the relocation container, the ssh helper and the fingerprint reader; found {}",
            relocation_launches.len()
        );
        for launch in &relocation_launches {
            assert!(
                launch.contains(r#"-v "$$WORKDIR:$$WORKDIR""#),
                "this launch cannot see the deployment directory:\n{launch}"
            );
        }
    }

    /// A status file from a newer sidecar must read as "less detail", never as a broken page.
    #[test]
    fn a_status_file_tolerates_missing_and_unknown_fields() {
        let minimal = r#"{"id":"a","mode":"push","state":"running","stage":"push","future":42}"#;
        let run: RelocationRun = serde_json::from_str(
            &minimal.replace(r#""stage":"push""#, r#""stage":"push","started_at":7"#),
        )
        .expect("the four required fields plus started_at are enough");
        assert!(run.is_running());
        assert_eq!(run.started_at, 7);
        assert!(run.message.is_none());
        assert!(!run.docker_installed);
    }

    /// The path core reads its metrics size from is the path the composition mounts them at.
    ///
    /// Two files holding one fact, so it gets the test that stops them drifting: a rename in the
    /// composition would leave this reading a directory that does not exist, and the estimate
    /// would silently lose the largest thing in the archive.
    #[test]
    fn the_metrics_mount_this_reads_is_the_one_the_composition_makes() {
        assert!(
            compose().contains(&format!("vmdata:{VM_MOUNT_IN_CORE}:ro")),
            "the composition no longer mounts the metrics volume at {VM_MOUNT_IN_CORE}, so the \
             archive-size estimate silently drops the metrics"
        );
    }

    #[test]
    fn space_needed_doubles_and_never_wraps() {
        assert_eq!(space_needed(10), 20);
        assert_eq!(space_needed(u64::MAX), u64::MAX);
    }

    #[test]
    fn the_log_tail_is_bounded_in_both_directions() {
        let dir = std::env::temp_dir().join(format!("yagra-reloc-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let text: String = (0..50).map(|i| format!("line {i}\n")).collect();
        std::fs::write(dir.join(LOG_FILE), text).unwrap();
        let read = |tail: usize| -> Vec<String> {
            let tail = tail.clamp(1, MAX_LOG_LINES);
            let text = std::fs::read_to_string(dir.join(LOG_FILE)).unwrap();
            let lines: Vec<&str> = text.lines().collect();
            lines[lines.len().saturating_sub(tail)..]
                .iter()
                .map(|s| (*s).to_owned())
                .collect()
        };
        assert_eq!(read(0).len(), 1, "a tail of zero still returns a line");
        assert_eq!(
            read(5),
            ["line 45", "line 46", "line 47", "line 48", "line 49"]
        );
        assert_eq!(read(9_999).len(), 50, "asking for more than exists is fine");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The status file is read leniently, and an unreadable one is not an error.
    #[test]
    fn an_unparseable_status_file_reads_as_no_run() {
        let dir = std::env::temp_dir().join(format!("yagra-reloc-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        assert!(read_status(&dir).is_none(), "no file means no run");
        std::fs::write(dir.join(STATUS_FILE), b"{ not json").unwrap();
        assert!(read_status(&dir).is_none());
        std::fs::write(
            dir.join(STATUS_FILE),
            br#"{"id":"a","mode":"archive","state":"done","stage":"archive","started_at":1}"#,
        )
        .unwrap();
        let run = read_status(&dir).expect("a well-formed file is read");
        assert!(!run.is_running());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
