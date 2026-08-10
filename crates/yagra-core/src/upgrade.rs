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
pub const REQUEST_SCHEMA: u32 = 1;

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
}

impl UpgradeRepo {
    #[must_use]
    pub fn new(pool: PgPool, dir: Option<PathBuf>) -> Self {
        Self { pool, dir }
    }

    /// The updater's heartbeat, or `None` when the mechanism is off or has never run.
    #[must_use]
    pub fn heartbeat(&self) -> Option<UpdaterHeartbeat> {
        self.read_json("current.json")
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

    /// Ask the updater to apply `tag`.
    ///
    /// Returns the run id. The caller must have validated `tag` with [`is_valid_tag`] — this
    /// asserts it again rather than trusting that, because this function is the last thing between
    /// an HTTP body and a file a root-privileged container reads.
    ///
    /// Written to a temporary name and renamed, so the updater never sees a half-written request.
    pub fn request_apply(&self, tag: &str, requested_by: &str, now: i64) -> anyhow::Result<String> {
        let dir = self
            .dir
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("the upgrade mechanism is not enabled"))?;
        anyhow::ensure!(is_valid_tag(tag), "invalid release tag");
        let id = uuid::Uuid::new_v4().to_string();
        let body = request_body(REQUEST_SCHEMA, &id, "apply", tag, requested_by, now);
        let tmp = dir.join("request.tmp");
        std::fs::write(&tmp, body)?;
        std::fs::rename(&tmp, dir.join("request"))?;
        Ok(id)
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
