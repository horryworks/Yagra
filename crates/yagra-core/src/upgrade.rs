// SPDX-License-Identifier: AGPL-3.0-only
//! How far this deployment can move — the read half of ADR-050.
//!
//! Yagra could always go forward and could never answer "can I go back?". ADR-017 made upgrade
//! safety a top-tier policy and ADR-040 delivered the *don't lose data* half; this is the start of
//! the other half, and the question it answers first is the one an operator asks before pressing
//! anything: **if this goes wrong, can I return to the version I am on now?**
//!
//! The answer is a floor, not a flag. Migration 0078 gives every migration that narrows the schema
//! a place to declare the oldest core that can still run afterwards, and *saying nothing means
//! reversible* — which is the truth for all 77 migrations that predate it. The floor for a whole
//! database is then the highest declared `min_core` among the rows that have actually been applied
//! to it, which is why [`UpgradeRepo::schema_state`] joins `schema_compat` against
//! `_sqlx_migrations` rather than reading either alone: a floor declared by a migration this
//! database never received does not bind it.
//!
//! ⚠️ The comparison happens in Rust, never in SQL. `max(min_core)` over text puts `0.2.10` before
//! `0.2.9`, and this is the one value in the system that must not be subtly wrong — it decides
//! whether the WebUI offers a rollback button.

use std::path::{Path, PathBuf};

use semver::Version;
use sqlx::{PgPool, Row};

/// The request-file format version core writes and the updater checks (ADR-050 decision 14).
///
/// ⚠️ Not the bus version field `backlog.md` bans reintroducing. That one was written on every
/// message and read nowhere; this one is *branched on*, and it has to be, because the updater
/// container is the one component an upgrade does not replace — a new core paired with an old
/// updater is structural rather than a rollout window. An updater that does not recognise the
/// schema refuses and says why, instead of half-executing a request it misread.
///
/// Adding a *command* did not need a bump, and that is the property to preserve: the updater
/// matches the command against a closed list and refuses anything else by name, so an updater older
/// than [`Command::Bundle`] answers "unsupported command" rather than misreading it. A bump is for
/// a change to the *shape* of the file, where an old updater would read the wrong value out of a
/// line it thinks it understands.
pub const REQUEST_SCHEMA: u32 = 1;

/// Ceiling on an uploaded image archive unless `YAGRA_UPGRADE_BUNDLE_MAX_BYTES` overrides it.
///
/// Three release images saved together come to roughly a gigabyte, so this is not a working limit —
/// it is the one that catches the wrong file being dragged into the browser before it fills the
/// filesystem PostgreSQL is on.
pub const DEFAULT_BUNDLE_MAX_BYTES: u64 = 4 * 1024 * 1024 * 1024;

/// How stale the updater's heartbeat may be before it is reported as stopped rather than ready.
///
/// A multiple of its own check interval rather than a constant, so a deployment that checks for new
/// versions daily is not declared dead every hour. Three intervals tolerates one missed cycle plus
/// the run itself.
const HEARTBEAT_STALE_AFTER: u32 = 3;

/// The oldest core version that can still run this database, and which migration decided that.
#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
pub struct CompatFloor {
    /// Oldest core version that can start against this schema, as bare semver (`"0.3.0"`).
    pub min_core: String,
    /// Why that migration narrowed the schema.
    pub reason: String,
    /// The migration that imposed the floor.
    pub since_version: i64,
}

/// What this database's migration history says about itself.
#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
pub struct SchemaState {
    /// How many migrations have been applied.
    pub applied_count: i64,
    /// The newest applied migration version; `null` only on a database with none.
    pub latest_version: Option<i64>,
    /// The binding compatibility floor, or `null` when every applied migration is reversible.
    pub compat: Option<CompatFloor>,
}

/// The updater sidecar's heartbeat (`current.json`), written every check cycle.
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize, utoipa::ToSchema)]
pub struct UpdaterHeartbeat {
    /// Unix seconds when the sidecar last wrote this file.
    pub written_at: i64,
    /// The image repository the sidecar is pinned to. Fixed by the host, not settable over the API.
    pub repo: String,
    /// How often the sidecar re-checks for new versions, in seconds.
    pub check_interval_secs: u64,
    /// Whether the sidecar will install an uploaded image archive (`YAGRA_UPGRADE_ALLOW_BUNDLE`).
    ///
    /// Declared by the sidecar, never decided here. That is the whole point of the gate: an archive
    /// is the one path that can put an image core never named onto the host, so it is opened by
    /// editing the host's compose file and by nothing reachable over the API. `#[serde(default)]`
    /// makes an updater too old to know the field read as `false`, which is the safe direction.
    #[serde(default)]
    pub allow_bundle: bool,
    /// Whether the sidecar saw the operator's switch turned off on its last pass.
    ///
    /// Reported rather than assumed, for the same reason as `allow_bundle`: core stores the setting
    /// but the sidecar is what acts on it, so this is the sidecar confirming it read the same
    /// answer. A disagreement between this and the stored value means the two have not converged
    /// yet — at most one beat.
    #[serde(default)]
    pub paused: bool,
}

/// What core is asking the updater to do.
///
/// A closed set, because the value crosses into a container holding the Docker socket. The updater
/// compares it against these exact tokens and refuses anything else by name — including a command
/// newer than itself, which is why adding one needs no [`REQUEST_SCHEMA`] bump.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    /// Fetch the release from the registry and install it.
    Apply,
    /// Install from an image archive already uploaded to the shared volume (ADR-050 Increment 3).
    Bundle,
}

impl Command {
    /// The token written into the request file.
    ///
    /// ⚠️ A new variant also belongs in `tests::COMMANDS` below, which is what pins this list to
    /// the shell sidecar's own accept list. Rust cannot enumerate variants, so that array is
    /// hand-written — the same arrangement as `repo.rs`'s grandfathered-migration table.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Apply => "apply",
            Self::Bundle => "bundle",
        }
    }
}

/// Why an image-archive upload could not be staged.
#[derive(Debug, thiserror::Error)]
pub enum BundleError {
    /// No hand-off volume, so there is no updater to hand an archive to.
    #[error("the upgrade mechanism is not enabled")]
    Disabled,
    /// Not an id this core minted — which is also what makes the staged file name a name.
    #[error("invalid run id")]
    BadRunId,
    /// Past the byte cap.
    #[error("the archive is larger than the {cap}-byte limit")]
    TooLarge {
        /// The limit that was passed.
        cap: u64,
    },
    /// The filesystem refused. The cause is kept for the log and deliberately out of `Display`,
    /// which is what an API client would be shown (security.md).
    #[error("could not stage the archive")]
    Io(#[from] std::io::Error),
}

/// One release the sidecar found, with the digests it resolved.
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize, utoipa::ToSchema)]
pub struct AvailableRelease {
    /// The release tag (`v0.2.2`).
    pub tag: String,
    /// Resolved image digest for `yagra-core` at that tag; `null` when it could not be resolved.
    #[serde(default)]
    pub core_digest: Option<String>,
}

/// The versions the sidecar last saw in the registry (`available.json`).
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize, utoipa::ToSchema)]
pub struct AvailableVersions {
    /// Unix seconds when the list was refreshed.
    pub written_at: i64,
    /// Releases found, newest first.
    #[serde(default)]
    pub releases: Vec<AvailableRelease>,
    /// Why the list is empty or stale, when the sidecar could not reach the registry. A closed
    /// network has no registry, so this is an ordinary state rather than a fault.
    #[serde(default)]
    pub error: Option<String>,
}

/// The most recent run the sidecar performed (`status.json`).
///
/// Survives core restarting mid-operation, which is the whole reason it is a file: the process that
/// asked for the upgrade is not the process that sees it finish (ADR-050 decision 3).
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize, utoipa::ToSchema)]
pub struct RunStatus {
    /// The run id core generated when it wrote the request.
    pub id: String,
    /// `check`, `apply` or `rollback`.
    pub command: String,
    /// The release the run targets, when it targets one.
    #[serde(default)]
    pub target: Option<String>,
    /// `running`, `succeeded`, `failed` or `rejected`.
    pub state: String,
    /// Which phase the run reached — `backup`, `pull`, `compose`, `verify`.
    #[serde(default)]
    pub step: Option<String>,
    /// Human-readable detail, especially the reason for a failure or refusal.
    #[serde(default)]
    pub message: Option<String>,
    /// Unix seconds when the run began.
    pub started_at: i64,
    /// Unix seconds when it ended; `null` while it is still running.
    #[serde(default)]
    pub finished_at: Option<i64>,
    /// Who asked for it.
    #[serde(default)]
    pub requested_by: Option<String>,
}

impl RunStatus {
    /// Whether this run is still in flight.
    #[must_use]
    pub fn is_running(&self) -> bool {
        self.state == "running"
    }
}

/// Reads the migration history, and the shared volume the updater sidecar talks over.
///
/// `dir` is `None` when `YAGRA_UPGRADE_DIR` is unset, which is the default and means the whole
/// mechanism is absent: no request can be written and every read reports "disabled". Same shape as
/// [`crate::webtls::WebTlsRepo`]'s optional directory, for the same reason — an optional deployment
/// capability should be one `Option` at the edge rather than a flag threaded through the callers.
pub struct UpgradeRepo {
    pool: PgPool,
    dir: Option<PathBuf>,
    bundle_max_bytes: u64,
}

impl UpgradeRepo {
    #[must_use]
    pub fn new(pool: PgPool, dir: Option<PathBuf>, bundle_max_bytes: u64) -> Self {
        Self {
            pool,
            dir,
            bundle_max_bytes,
        }
    }

    /// The largest image archive this deployment will accept.
    #[must_use]
    pub fn bundle_max_bytes(&self) -> u64 {
        self.bundle_max_bytes
    }

    /// Free space on the filesystem holding the hand-off volume, when it can be measured.
    ///
    /// The volume lives under `/var/lib/docker`, so this measures the filesystem `docker load` will
    /// unpack onto as well — the one that matters. `None` off unix, where the mechanism does not
    /// run at all.
    #[must_use]
    pub fn free_bytes(&self) -> Option<u64> {
        yagra_hoststats::available_bytes(self.dir.as_deref()?)
    }

    /// The updater's heartbeat, or `None` when the sidecar is absent or has never run.
    #[must_use]
    pub fn heartbeat(&self) -> Option<UpdaterHeartbeat> {
        self.read_json("current.json")
    }

    /// Is the operator's switch on? (ADR-050 — Settings ▸ Upgrade.)
    ///
    /// **Fail-closed**, unlike `repo.rs`'s `get_meraki_polling_enabled`, which falls back to `true`
    /// on a missing row. The difference is what the two switches control: that one stops collection
    /// from a vendor API, this one opens a path from an Admin session to root on the host. A
    /// database core cannot read is not a reason to leave that open. A *fresh* deployment still
    /// starts enabled, because the column's own DEFAULT is `true` — the fallback here only covers
    /// skeleton mode and the moments before the settings row is seeded.
    pub async fn enabled(&self) -> bool {
        sqlx::query("SELECT upgrade_enabled FROM app_settings WHERE id = TRUE")
            .fetch_optional(&self.pool)
            .await
            .ok()
            .flatten()
            .and_then(|r| r.try_get::<bool, _>("upgrade_enabled").ok())
            .unwrap_or(false)
    }

    /// Store the switch, then republish it where the sidecar can see it.
    ///
    /// PostgreSQL is the source of truth and the file is a cache of it — the same relationship
    /// ADR-044 gave the WebUI certificate. That ordering matters here: if the volume were
    /// authoritative, wiping it would silently re-enable a privileged path, and the setting would
    /// not survive the upgrade it governs.
    pub async fn set_enabled(&self, enabled: bool) -> anyhow::Result<()> {
        sqlx::query(
            "INSERT INTO app_settings (id, upgrade_enabled, updated_at) \
             VALUES (TRUE, $1, now()) \
             ON CONFLICT (id) DO UPDATE SET upgrade_enabled = $1, updated_at = now()",
        )
        .bind(enabled)
        .execute(&self.pool)
        .await?;
        self.publish_enabled(enabled);
        Ok(())
    }

    /// Write the switch into the hand-off volume for the sidecar.
    ///
    /// Called on every change and again at startup, so a volume that was deleted, or one written by
    /// a core that has since been replaced, converges on what the database says.
    ///
    /// Best effort: a deployment with no sidecar has no directory to write to, and a failure here
    /// must not stop core from booting. The sidecar treats an absent file as enabled, matching the
    /// column default — the safe direction is that a fresh volume does not read as paused, since
    /// core refuses the request anyway when the stored setting is off.
    pub fn publish_enabled(&self, enabled: bool) {
        let Some(dir) = self.dir.as_deref() else {
            return;
        };
        let body = format!("{SETTINGS_ENABLED_KEY}={}\n", u8::from(enabled));
        let tmp = dir.join("settings.tmp");
        let write = std::fs::write(&tmp, body)
            .and_then(|()| std::fs::rename(&tmp, dir.join(SETTINGS_FILE)));
        if let Err(e) = write {
            tracing::warn!(error = %e, "could not publish the upgrade switch to the updater");
        }
    }

    /// The releases the updater last saw.
    #[must_use]
    pub fn available(&self) -> Option<AvailableVersions> {
        self.read_json("available.json")
    }

    /// The most recent run, finished or in flight.
    #[must_use]
    pub fn last_run(&self) -> Option<RunStatus> {
        self.read_json("status.json")
    }

    /// Ask the updater to carry out `command` against `tag`, under the run id `id`.
    ///
    /// The caller must have validated `tag` with [`is_valid_tag`] and taken `id` from
    /// [`new_run_id`] — this asserts both again rather than trusting them, because this function is
    /// the last thing between an HTTP request and a file a root-privileged container reads.
    ///
    /// Written to a temporary name and renamed, so the updater never sees a half-written request.
    pub fn request(
        &self,
        command: Command,
        id: &str,
        tag: &str,
        requested_by: &str,
        now: i64,
    ) -> anyhow::Result<()> {
        let dir = self
            .dir
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("the upgrade mechanism is not enabled"))?;
        anyhow::ensure!(is_valid_tag(tag), "invalid release tag");
        anyhow::ensure!(is_run_id(id), "invalid run id");
        let body = request_body(REQUEST_SCHEMA, id, command.as_str(), tag, requested_by, now);
        let tmp = dir.join("request.tmp");
        std::fs::write(&tmp, body)?;
        std::fs::rename(&tmp, dir.join("request"))?;
        Ok(())
    }

    /// Start receiving an image archive for run `id`.
    ///
    /// The archive is named after the run rather than after anything the client sent, so there is
    /// no filename to validate and none in the request file: the updater rebuilds the path from the
    /// id it already regex-checks.
    pub async fn begin_bundle(&self, id: &str) -> Result<BundleUpload, BundleError> {
        let dir = self.dir.as_deref().ok_or(BundleError::Disabled)?;
        stage_bundle(&dir.join(BUNDLE_SUBDIR), id, self.bundle_max_bytes).await
    }

    /// Delete staged archives no run is waiting on.
    ///
    /// An archive is a gigabyte sitting in a volume nothing else prunes, and every path that
    /// abandons one is a path where core is not around to tidy up: a request the updater rejects,
    /// an updater too old to know the command, core restarting between the upload and the run.
    /// "Keep only what the in-flight run needs" is the rule that stays true without bookkeeping.
    ///
    /// Best effort by design — a failure here must never stop an upgrade, so it is logged and
    /// dropped rather than returned.
    pub fn prune_stale_bundles(&self) {
        let Some(dir) = self.dir.as_deref() else {
            return;
        };
        let keep = self.last_run().filter(RunStatus::is_running).map(|r| r.id);
        let Ok(entries) = std::fs::read_dir(dir.join(BUNDLE_SUBDIR)) else {
            return;
        };
        for entry in entries.flatten() {
            let name = entry.file_name();
            let Some(name) = name.to_str() else { continue };
            let Some(run) = name.split('.').next() else {
                continue;
            };
            if keep.as_deref() == Some(run) {
                continue;
            }
            match std::fs::remove_file(entry.path()) {
                Ok(()) => tracing::info!(file = name, "removed an abandoned upgrade archive"),
                Err(e) => {
                    tracing::warn!(file = name, error = %e, "could not remove an abandoned upgrade archive")
                }
            }
        }
    }

    /// Record the outcome of a run that finished while this core was not running.
    ///
    /// The process that asked for the upgrade is never the process that sees it finish — core is
    /// restarted by the very operation it requested. Without this, the audit trail would show
    /// somebody requesting an upgrade and would never show whether it worked, which is the half an
    /// operator actually needs afterwards.
    ///
    /// Idempotent through a marker file beside the status: the run is claimed by renaming, so two
    /// cores racing at startup produce one row, and a core restarting for an unrelated reason does
    /// not re-log an old run.
    pub async fn record_finished_run(&self, audit: &crate::audit::AuditRepo) {
        let Some(dir) = self.dir.as_deref() else {
            return;
        };
        let Some(run) = self.last_run() else { return };
        if run.is_running() {
            return;
        }
        // Claim it. `rename` is atomic, so exactly one caller wins even across processes.
        let claim = dir.join(format!("status.{}.logged", run.id));
        if claim.exists() || std::fs::rename(dir.join("status.json"), &claim).is_err() {
            return;
        }
        let action = format!(
            "upgrade {} -> {} ({})",
            run.state,
            run.target.as_deref().unwrap_or("?"),
            run.message.as_deref().unwrap_or("no detail")
        );
        let status = if run.state == "succeeded" { 200 } else { 500 };
        let by = run.requested_by.clone().unwrap_or_else(|| "unknown".into());
        if let Err(e) = audit.record(&by, &action, status).await {
            tracing::warn!(error = %e, "could not record the completed upgrade in the audit log");
        } else {
            tracing::info!(run = %run.id, state = %run.state, "recorded the completed upgrade");
        }
    }

    fn read_json<T: serde::de::DeserializeOwned>(&self, name: &str) -> Option<T> {
        let path: &Path = self.dir.as_deref()?;
        let text = std::fs::read_to_string(path.join(name)).ok()?;
        match serde_json::from_str(&text) {
            Ok(v) => Some(v),
            Err(e) => {
                // A file written by a NEWER updater may carry fields this core cannot read. Report
                // "not available" rather than failing the page — the same lenient-read discipline
                // the bus uses, and the alternative is a 500 on the one screen an operator opens
                // when something is already wrong.
                tracing::warn!(file = name, error = %e, "unreadable upgrade state file");
                None
            }
        }
    }

    /// Applied-migration tally plus the binding floor, if any.
    pub async fn schema_state(&self) -> anyhow::Result<SchemaState> {
        let row = sqlx::query(
            "SELECT count(*)::bigint AS n, max(version) AS latest FROM _sqlx_migrations",
        )
        .fetch_one(&self.pool)
        .await?;
        let applied_count: i64 = row.try_get("n")?;
        let latest_version: Option<i64> = row.try_get("latest")?;

        // Only floors declared by migrations THIS database actually received. A floor from a
        // migration it never got does not constrain it.
        let rows = sqlx::query(
            "SELECT sc.migration_version, sc.min_core, sc.reason \
               FROM schema_compat sc \
               JOIN _sqlx_migrations m ON m.version = sc.migration_version",
        )
        .fetch_all(&self.pool)
        .await?;

        let declared: Vec<CompatFloor> = rows
            .into_iter()
            .map(|r| {
                Ok::<_, sqlx::Error>(CompatFloor {
                    since_version: r.try_get("migration_version")?,
                    min_core: r.try_get("min_core")?,
                    reason: r.try_get("reason")?,
                })
            })
            .collect::<Result<_, _>>()?;

        Ok(SchemaState {
            applied_count,
            latest_version,
            compat: binding_floor(declared),
        })
    }
}

/// Where staged image archives live inside the hand-off volume.
const BUNDLE_SUBDIR: &str = "bundle";

/// The file core materialises the operator's switch into, for the sidecar to read.
const SETTINGS_FILE: &str = "settings";

/// The key inside that file. Pinned to the sidecar's own `sed` expression by
/// `the_sidecar_reads_the_settings_key_core_writes`.
const SETTINGS_ENABLED_KEY: &str = "enabled";

/// A fresh run id.
///
/// Minted by the caller rather than inside [`UpgradeRepo::request`] because an archive upload names
/// its file after the run *before* the request exists — the transfer takes minutes and the request
/// is what starts the work.
#[must_use]
pub fn new_run_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

/// Is this one of our own run ids?
///
/// Also the guarantee that `bundle/<id>.tar` is a name and not a path: a value with no `/`, no `\`
/// and no `.` cannot climb out of the directory or shadow another run's file. Checked here rather
/// than trusted from the caller so the property holds at the point it is relied on.
#[must_use]
pub fn is_run_id(id: &str) -> bool {
    id.len() == 36 && id.bytes().all(|b| b.is_ascii_hexdigit() || b == b'-')
}

/// Bytes that must be free to accept an archive of `len` bytes.
///
/// Twice the archive plus a margin: the tar lands on the Docker filesystem and then `docker load`
/// unpacks its layers onto the same filesystem, so the peak holds both. Deliberately an estimate —
/// layers unpack larger than they store — and the margin is what keeps the estimate from being the
/// thing that fills the disk. Refusing early matters because a full Docker filesystem takes
/// PostgreSQL down with it, and this is the only endpoint that can write a gigabyte.
#[must_use]
pub fn bundle_space_needed(len: u64) -> u64 {
    // Saturating throughout: an absurd declared length must come out as "more space than exists",
    // never wrap into a small number that passes the check.
    len.saturating_mul(2).saturating_add(512 * 1024 * 1024)
}

/// Open the staging file for a run. Free function so the upload can be tested without a database.
async fn stage_bundle(dir: &Path, id: &str, cap: u64) -> Result<BundleUpload, BundleError> {
    if !is_run_id(id) {
        return Err(BundleError::BadRunId);
    }
    tokio::fs::create_dir_all(dir).await?;
    let final_path = dir.join(format!("{id}.tar"));
    let tmp = dir.join(format!("{id}.tar.part"));
    let file = tokio::fs::File::create(&tmp).await?;
    Ok(BundleUpload {
        tmp,
        final_path,
        file,
        written: 0,
        cap,
        done: false,
    })
}

/// An image archive being written into the hand-off volume.
///
/// Streamed to disk rather than buffered: the file is around a gigabyte and core shares a host with
/// PostgreSQL. Written under a `.part` name and renamed at the end, so the updater — which polls
/// for work every few seconds — can never pick up a half-received archive.
pub struct BundleUpload {
    tmp: PathBuf,
    final_path: PathBuf,
    file: tokio::fs::File,
    written: u64,
    cap: u64,
    done: bool,
}

impl BundleUpload {
    /// Append a chunk, refusing once the cap would be passed.
    ///
    /// Checked *before* the write, so the cap bounds what reaches the disk rather than describing
    /// what already did.
    pub async fn write(&mut self, chunk: &[u8]) -> Result<(), BundleError> {
        use tokio::io::AsyncWriteExt;
        let next = self.written.saturating_add(chunk.len() as u64);
        if next > self.cap {
            return Err(BundleError::TooLarge { cap: self.cap });
        }
        self.file.write_all(chunk).await?;
        self.written = next;
        Ok(())
    }

    /// Publish the archive under its final name and return how many bytes it holds.
    ///
    /// `sync_all` because the reader is another container and core is about to be replaced by the
    /// very work this file feeds — the bytes have to survive without this process.
    pub async fn finish(mut self) -> Result<u64, BundleError> {
        use tokio::io::AsyncWriteExt;
        self.file.flush().await?;
        self.file.sync_all().await?;
        tokio::fs::rename(&self.tmp, &self.final_path).await?;
        self.done = true;
        Ok(self.written)
    }
}

impl Drop for BundleUpload {
    /// Abandon a transfer that did not finish.
    ///
    /// Every failure path ends here — a client that disconnects mid-upload, the byte cap, a write
    /// error — so no caller has to remember to remove the partial file, and none of them can
    /// forget on some new error path added later.
    fn drop(&mut self) {
        if !self.done {
            let _ = std::fs::remove_file(&self.tmp);
        }
    }
}

/// The highest declared floor — the one that actually binds.
///
/// Ordered by parsed semver, not by string and not by migration number: a later migration may
/// declare a *lower* floor than an earlier one, and only the highest constrains the database.
/// A row whose `min_core` does not parse is kept as a candidate and treated as **higher than any
/// parseable version**, because an unreadable floor is not evidence that going back is safe.
#[must_use]
fn binding_floor(mut declared: Vec<CompatFloor>) -> Option<CompatFloor> {
    declared.sort_by(
        |a, b| match (parse_version(&a.min_core), parse_version(&b.min_core)) {
            (Some(x), Some(y)) => x.cmp(&y),
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, None) => a.since_version.cmp(&b.since_version),
        },
    );
    declared.pop()
}

/// Parse a version that may be written as a bare semver or as a `v`-prefixed release tag.
#[must_use]
fn parse_version(s: &str) -> Option<Version> {
    Version::parse(s.trim().trim_start_matches('v')).ok()
}

/// Is this a release tag this deployment will act on?
///
/// **The narrowest thing that can work, on purpose.** The image repository is fixed by the sidecar's
/// own environment and cannot be set over the API (ADR-050 decision 2), so a tag is the only part of
/// an image reference a request can influence — and this is what keeps it from being a reference at
/// all. `v` plus a semver, with an optional `-beta`/`-rc` suffix; nothing else.
///
/// What that forbids matters more than what it allows: no `/`, so it cannot name another repository;
/// no `:` or `@`, so it cannot smuggle a second tag or a digest; no shell metacharacter, no space
/// and no newline, so it cannot break out of the `key=value` line the updater parses, and cannot
/// become a second argument to anything.
#[must_use]
pub fn is_valid_tag(tag: &str) -> bool {
    let Some(rest) = tag.strip_prefix('v') else {
        return false;
    };
    // Length bound first: everything below is linear, but an unbounded value has no business
    // reaching a file another container reads.
    if rest.is_empty() || rest.len() > 40 {
        return false;
    }
    let (core, suffix) = match rest.split_once('-') {
        Some((c, s)) => (c, Some(s)),
        None => (rest, None),
    };
    let numeric = |s: &str| !s.is_empty() && s.len() <= 6 && s.bytes().all(|b| b.is_ascii_digit());
    let mut parts = core.split('.');
    let (Some(a), Some(b), Some(c), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return false;
    };
    if !numeric(a) || !numeric(b) || !numeric(c) {
        return false;
    }
    match suffix {
        None => true,
        // `beta1`, `rc2` — alphanumeric only, so no separator of any kind survives.
        Some(s) => !s.is_empty() && s.len() <= 16 && s.bytes().all(|b| b.is_ascii_alphanumeric()),
    }
}

/// Reduce a username to something that cannot alter the shape of the request file.
///
/// Usernames come from local accounts, LDAP and OIDC, so they are not under our control: an
/// external IdP may hand back a display name containing a newline or an `=`. This value is only
/// ever an attribution string, so flattening it costs nothing and removes the entire question.
#[must_use]
fn sanitize_actor(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '@'))
        .take(64)
        .collect();
    if cleaned.is_empty() {
        "unknown".to_owned()
    } else {
        cleaned
    }
}

/// Serialize a request as the `key=value` lines the updater parses.
///
/// Every value is constrained before it gets here — the tag by [`is_valid_tag`], the actor by
/// [`sanitize_actor`], the rest generated — so this cannot emit a line the updater would misread.
/// Kept separate from the file write so that property is testable without a filesystem.
#[must_use]
fn request_body(
    schema: u32,
    id: &str,
    command: &str,
    tag: &str,
    requested_by: &str,
    now: i64,
) -> String {
    format!(
        "schema={schema}\nid={id}\ncommand={command}\ntag={tag}\nrequested_by={}\nrequested_at={now}\n",
        sanitize_actor(requested_by)
    )
}

/// Is the updater's heartbeat recent enough to call the mechanism ready?
///
/// Separating "stopped" from "off" is the point. Both render as no upgrade being possible, but one
/// is a deployment choice and the other is a fault, and a page that shows them identically teaches
/// an operator to ignore the wrong one (ADR-040's `/flags` discipline).
#[must_use]
pub fn heartbeat_is_fresh(written_at: i64, check_interval_secs: u64, now: i64) -> bool {
    if now < written_at {
        // Clock skew between containers. Treat a heartbeat from the future as present rather than
        // as infinitely stale — the sidecar clearly wrote it.
        return true;
    }
    let allowance = check_interval_secs.saturating_mul(u64::from(HEARTBEAT_STALE_AFTER));
    let age = now.saturating_sub(written_at).unsigned_abs();
    age <= allowance.max(60)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every command core can write. Hand-written because Rust cannot enumerate variants; it is
    /// what `every_command_is_accepted_by_the_sidecar` iterates.
    const COMMANDS: &[Command] = &[Command::Apply, Command::Bundle];

    fn floor(since: i64, min_core: &str) -> CompatFloor {
        CompatFloor {
            min_core: min_core.to_owned(),
            reason: "test".to_owned(),
            since_version: since,
        }
    }

    /// The reason this is not `MAX(min_core)` in SQL, stated as a test.
    #[test]
    fn the_binding_floor_is_the_highest_version_not_the_highest_string() {
        let picked = binding_floor(vec![floor(80, "0.2.9"), floor(90, "0.2.10")]).unwrap();
        assert_eq!(picked.min_core, "0.2.10", "0.2.10 > 0.2.9 numerically");
        assert_eq!(picked.since_version, 90);
    }

    /// A later migration may relax the floor; the earlier, higher one still binds.
    #[test]
    fn the_binding_floor_is_not_simply_the_newest_migration() {
        let picked = binding_floor(vec![floor(80, "0.4.0"), floor(90, "0.3.0")]).unwrap();
        assert_eq!(picked.min_core, "0.4.0");
        assert_eq!(picked.since_version, 80);
    }

    #[test]
    fn no_declarations_means_no_floor() {
        assert!(binding_floor(vec![]).is_none());
    }

    /// An unreadable floor must not be sorted below a readable one and quietly dropped.
    #[test]
    fn an_unparseable_floor_outranks_every_parseable_one() {
        let picked = binding_floor(vec![floor(80, "0.9.0"), floor(90, "not-a-version")]).unwrap();
        assert_eq!(picked.min_core, "not-a-version");
    }

    #[test]
    fn a_release_tag_is_accepted_in_the_forms_this_project_actually_publishes() {
        for ok in ["v0.2.1", "v1.0.0", "v0.2.10", "v0.3.0-beta1", "v1.2.3-rc2"] {
            assert!(is_valid_tag(ok), "{ok} is a tag this repository publishes");
        }
    }

    /// The list that matters: everything here would turn a tag into something other than a tag.
    #[test]
    fn a_tag_can_never_become_an_image_reference_or_a_second_argument() {
        for bad in [
            "",
            "latest",                 // not a release
            "0.2.1",                  // no `v`
            "v0.2",                   // not three components
            "v0.2.1.4",               // four
            "v0.2.x",                 // non-numeric
            "evil/yagra-core:v0.2.1", // another repository
            "v0.2.1 --privileged",    // a second argument
            "v0.2.1\nid=other",       // a second line in the request file
            "v0.2.1;rm -rf /",        // a second command
            "v0.2.1$(id)",            // substitution
            "v0.2.1`id`",             // substitution, the other spelling
            "v0.2.1@sha256:deadbeef", // a digest
            "v0.2.1:latest",          // a second tag
            "../../etc/passwd",       // traversal
            "v0.2.1-beta_1",          // separator inside the suffix
        ] {
            assert!(!is_valid_tag(bad), "{bad:?} must be refused");
        }
        // Unbounded input never reaches a file another container reads.
        assert!(!is_valid_tag(&format!("v0.2.{}", "1".repeat(64))));
    }

    /// The request file's shape cannot be altered by anything a caller supplies.
    #[test]
    fn a_request_is_one_line_per_field_whatever_the_actor_is_called() {
        let body = request_body(1, "run-1", "apply", "v0.2.2", "admin", 1_700_000_000);
        assert_eq!(
            body,
            "schema=1\nid=run-1\ncommand=apply\ntag=v0.2.2\nrequested_by=admin\n\
             requested_at=1700000000\n"
        );
        // An IdP-supplied display name cannot add a line or a second `=`.
        let hostile = request_body(1, "run-1", "apply", "v0.2.2", "a\nid=evil=x", 0);
        assert_eq!(hostile.lines().count(), 6, "still six fields");
        assert!(hostile.contains("requested_by=aidevilx"));
        // An actor with nothing usable in it is named, not blank.
        assert!(request_body(1, "i", "apply", "v0.2.2", "///", 0).contains("requested_by=unknown"));
    }

    /// "The sidecar is off" and "the sidecar has died" must not render as the same thing.
    #[test]
    fn a_heartbeat_goes_stale_relative_to_its_own_cadence() {
        let hourly = 3600;
        assert!(
            heartbeat_is_fresh(1_000_000, hourly, 1_000_000),
            "just written"
        );
        assert!(heartbeat_is_fresh(1_000_000, hourly, 1_000_000 + 3 * 3600));
        assert!(!heartbeat_is_fresh(
            1_000_000,
            hourly,
            1_000_000 + 3 * 3600 + 1
        ));

        // A daily cadence is not declared dead after an hour.
        let daily = 86_400;
        assert!(heartbeat_is_fresh(1_000_000, daily, 1_000_000 + 86_400));

        // A very short interval still gets a floor, so a one-second cadence is not permanently stale.
        assert!(heartbeat_is_fresh(1_000_000, 1, 1_000_000 + 30));

        // Container clock skew must not read as "stopped".
        assert!(heartbeat_is_fresh(1_000_000, hourly, 999_000));
    }

    /// The one mirror this feature creates: the commands Rust can write ⟷ the commands the shell
    /// sidecar accepts. Nothing else compares them, and the failure is silent — core writes a
    /// request the updater answers "unsupported command" to, which reads as a bug in the updater.
    #[test]
    fn every_command_is_accepted_by_the_sidecar() {
        let compose = std::fs::read_to_string("../../docker-compose.deploy.yml")
            .expect("the deploy composition holds the updater's script");
        assert!(
            compose.contains("yagra-updater"),
            "read the wrong file — the updater service is not in it"
        );
        for cmd in COMMANDS {
            // Built at runtime: a literal needle in a test that reads a file it is not in would
            // still be fine here, but keeping it derived means the enum stays the source.
            let arm = format!("{})", cmd.as_str());
            assert!(
                compose.contains(&arm),
                "the updater's command list has no `{arm}` arm, so core can ask for a command it \
                 will refuse"
            );
        }
    }

    /// The second Rust ⟷ shell mirror: the key core writes into the hand-off volume ⟷ the
    /// expression the sidecar greps it out with. A drift here fails silently in the worst
    /// direction — the sidecar would read no value, treat the mechanism as enabled, and the
    /// operator's switch would appear to work while changing nothing on the side that acts.
    #[test]
    fn the_sidecar_reads_the_settings_key_core_writes() {
        let compose = std::fs::read_to_string("../../docker-compose.deploy.yml")
            .expect("the deploy composition holds the updater's script");
        // Built at runtime from the constants, so the test cannot pass by matching itself.
        let expr = format!("s/^{SETTINGS_ENABLED_KEY}=//p");
        assert!(
            compose.contains(&expr),
            "the updater does not parse `{expr}`, so core's switch would never reach it"
        );
        assert!(
            compose.contains(&format!("$$D/{SETTINGS_FILE}")),
            "the updater reads a different file from the one core writes"
        );
    }

    /// The staged file's name is a name because the id cannot be anything else.
    #[test]
    fn a_run_id_cannot_name_a_path() {
        assert!(is_run_id(&new_run_id()));
        assert!(is_run_id("0f8fad5b-d9cb-469f-a165-70867728950e"));
        for bad in [
            "",
            "../../etc/passwd",
            "0f8fad5b-d9cb-469f-a165-70867728950e/x",
            "0f8fad5b-d9cb-469f-a165-7086772895",    // too short
            "0f8fad5b-d9cb-469f-a165-70867728950ee", // too long
            "0f8fad5b-d9cb-469f-a165-708677289.0e",  // a dot would split the extension
            "0f8fad5b d9cb 469f a165 70867728950e",
        ] {
            assert!(!is_run_id(bad), "{bad:?} must not become a file name");
        }
    }

    /// Refusing early is the point: the peak holds the archive and what `docker load` unpacks.
    #[test]
    fn the_space_needed_covers_the_archive_and_what_it_unpacks_into() {
        let gig = 1024 * 1024 * 1024;
        assert!(
            bundle_space_needed(gig) > 2 * gig,
            "the tar plus its layers"
        );
        // An absurd length must not wrap into a small number and pass the check.
        assert_eq!(bundle_space_needed(u64::MAX), u64::MAX);
    }

    /// A transfer that stops partway leaves nothing behind, and the cap stops bytes rather than
    /// reporting on them afterwards.
    #[tokio::test]
    async fn an_unfinished_archive_is_removed_and_the_cap_bounds_what_reaches_the_disk() {
        let dir = std::env::temp_dir().join(format!("yagra-bundle-{}", new_run_id()));
        let id = new_run_id();
        let part = dir.join(format!("{id}.tar.part"));
        let done = dir.join(format!("{id}.tar"));

        // Over the cap: the write is refused and the partial file goes with the dropped upload.
        {
            let mut up = stage_bundle(&dir, &id, 8).await.expect("staged");
            up.write(b"1234").await.expect("under the cap");
            let err = up.write(b"56789").await.expect_err("over the cap");
            assert!(matches!(err, BundleError::TooLarge { cap: 8 }));
        }
        assert!(!part.exists(), "the partial archive must not survive");
        assert!(!done.exists(), "and it must certainly not be published");

        // A completed transfer is renamed, which is what makes it visible to the updater.
        {
            let mut up = stage_bundle(&dir, &id, 8).await.expect("staged");
            up.write(b"abc").await.expect("under the cap");
            assert_eq!(up.finish().await.expect("finished"), 3);
        }
        assert!(!part.exists(), "the staging name is gone");
        assert_eq!(std::fs::read(&done).expect("published"), b"abc");

        assert!(matches!(
            stage_bundle(&dir, "../evil", 8).await,
            Err(BundleError::BadRunId)
        ));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Release tags carry a `v`; the column holds bare semver. Both must read.
    #[test]
    fn a_version_parses_in_bare_and_release_tag_form() {
        assert_eq!(parse_version("0.3.1"), parse_version("v0.3.1"));
        assert!(
            parse_version(" v0.3.1 ").is_some(),
            "surrounding whitespace"
        );
        assert!(parse_version("latest").is_none());
        assert!(parse_version("").is_none());
    }
}
