// SPDX-License-Identifier: AGPL-3.0-only
//! Support bundle — one downloadable archive of logs and status for a deployment nobody can open a
//! shell on (ADR-045).
//!
//! # Why this exists rather than a host-side script
//!
//! The obvious collector for this data is a shell script beside [`crate::config_bundle`]'s sibling
//! `scripts/yagra-backup.sh`: it can read `docker logs`, `df`, and every container's environment.
//! It is also useless on the deployment this feature is *for* — a closed network where the operator
//! has a browser and nothing else. So the collector lives inside core and is reached over HTTP.
//!
//! That choice has one honest cost, stated here because it is the thing someone will discover at
//! the worst moment: **a bundle cannot be produced while core is down**, which is exactly when the
//! interesting failure happened. Two things close most of that gap, and neither is incidental:
//!
//! * The on-disk log file ([`yagra_telemetry::LOG_DIR_ENV`]) survives the process, so a bundle taken
//!   *after* a recovery still carries the run that died — including its panic.
//! * `monitoring_gaps` and the host trends are already persisted, so the window where core was
//!   absent is described by data core wrote before and after it.
//!
//! # Why a tar.gz of small text files
//!
//! The bundle leaves a site that does not permit data to leave casually, so a human reviews it
//! first. That makes the archive's job "be readable", not "be small": every entry is JSON or plain
//! text, `MANIFEST.json` lists both what is carried **and what is deliberately not**, and there is a
//! `README.txt` for whoever has to sign off on it. A single giant JSON blob would satisfy none of
//! that, and a binary format would fail the review outright.
//!
//! # Secrets: allow-list in, fail-closed out
//!
//! Two mechanisms, because either alone has a known hole:
//!
//! 1. **[`ENV_ALLOWLIST`]** — environment is carried by name, never by sweep. A deny-list would
//!    catch `YAGRA_NATS_POLLER_PASSWORD` and miss the password embedded in `YAGRA_DATABASE_URL`'s
//!    userinfo, which is the one that actually ships.
//! 2. **[`SecretScan`]** — every assembled byte is scanned before the archive is written, and a hit
//!    **fails the whole bundle** rather than redacting it. Redact-and-continue is the wrong default
//!    here: it assumes the pattern set is complete, and the cost of being wrong is a credential
//!    crossing an air gap. A refusal is recoverable; a leak is not.
//!
//! The scanner's strongest rule is not a pattern at all — it is the set of *literal* secret values
//! this process can see in its own environment. That catches a credential arriving through a path
//! nobody anticipated, which is the failure a pattern list cannot describe in advance.

use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::{PgPool, Row};
use std::io::Write;
use std::time::SystemTime;

/// Names the archive so a file on disk is identifiable without the tool that wrote it — the same
/// reasoning as [`crate::config_bundle::BUNDLE_FORMAT`].
pub const BUNDLE_FORMAT: &str = "yagra.support-bundle";
/// Bundle schema version. Bump when the file layout changes shape, not when a section is added.
pub const BUNDLE_VERSION: u32 = 1;

/// Default log window when `since_hours` is omitted.
pub const DEFAULT_SINCE_HOURS: u32 = 6;
/// Ceiling on the requested log window. The appender's own retention
/// ([`yagra_telemetry::DEFAULT_LOG_RETAIN_HOURS`]) is usually the tighter bound; this is the guard
/// against a request that would try to read a directory somebody pointed at `/var/log`.
pub const MAX_SINCE_HOURS: u32 = 168;

/// Ceiling on the log bytes carried, newest file first. Everything else in the bundle is bounded by
/// its own row limits and measures in tens of KB, so this is effectively the bundle's size.
///
/// When it bites, the manifest says so — a silently truncated log reads as "nothing was logged",
/// which is a wrong answer rather than a missing one.
pub const MAX_LOG_BYTES: usize = 24 * 1024 * 1024;

/// A value shorter than this is never treated as a secret literal by [`SecretScan`].
///
/// Without a floor, a `POSTGRES_PASSWORD=yagra` in a lab deployment forbids the substring `yagra`,
/// which appears in every path, table name and log line in the bundle — the scan would refuse
/// every bundle forever and the feature would be turned off rather than fixed.
const MIN_SECRET_LEN: usize = 8;

// ── Errors ────────────────────────────────────────────────────────────────────────────────────

/// What can stop a bundle being produced.
#[derive(Debug, thiserror::Error)]
pub enum BundleError {
    /// The redaction scan found something. **Deliberately carries no value** — naming the offending
    /// string in an error message would copy the secret into the log that error is written to.
    #[error("redaction scan matched {rule} in {file}")]
    SecretDetected {
        /// Archive path of the entry that matched.
        file: String,
        /// Which rule fired, for diagnosis without disclosure.
        rule: &'static str,
    },
    #[error("assembling the archive failed")]
    Archive(#[from] std::io::Error),
    #[error(transparent)]
    Serialize(#[from] serde_json::Error),
}

// ── Environment ───────────────────────────────────────────────────────────────────────────────

/// How one allow-listed environment variable is rendered into the bundle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EnvShape {
    /// The value verbatim: a number, a boolean, a path, a listen address. None of these is a secret
    /// — and a *path* to a key file is worth carrying precisely because "is the KEK mounted at all"
    /// is a question a bundle should answer.
    Plain,
    /// A connection URL, carried with its userinfo stripped by [`yagra_bus::redact_url`]. The host,
    /// port and scheme are the diagnostic value; the password beside them is why this shape exists.
    Url,
}

/// Every environment variable the bundle carries, and how.
///
/// **This is an allow-list and must stay one.** The temptation is a sweep with a deny-list of
/// `*PASSWORD*`-ish names; it fails on the first URL, since `YAGRA_DATABASE_URL` and `YAGRA_BUS_URL`
/// carry credentials in their userinfo under a perfectly innocent name.
///
/// Note what is **absent on purpose**: `YAGRA_NATS_POLLER_PASSWORD` (a secret outright), and the
/// contents of anything a `*_FILE` variable points at (the KEK and the session signing key never
/// leave the deployment — [`crate::config_bundle`] holds the same line).
pub const ENV_ALLOWLIST: &[(&str, EnvShape)] = &[
    // Identity and topology of this process
    ("YAGRA_CORE_ID", EnvShape::Plain),
    ("YAGRA_API_ADDR", EnvShape::Plain),
    ("YAGRA_ENABLE_HA", EnvShape::Plain),
    ("YAGRA_ENABLE_MCP", EnvShape::Plain),
    ("YAGRA_PUBLIC_DASHBOARD", EnvShape::Plain),
    // Stores and bus — the five-store layout, with credentials stripped
    ("YAGRA_DATABASE_URL", EnvShape::Url),
    ("YAGRA_BUS_URL", EnvShape::Url),
    ("YAGRA_TSDB_URL", EnvShape::Url),
    ("YAGRA_REDIS_URL", EnvShape::Url),
    ("YAGRA_LOGS_URL", EnvShape::Url),
    ("YAGRA_CLICKHOUSE_URL", EnvShape::Url),
    // Polling and retention
    ("YAGRA_POLL_INTERVAL_SECS", EnvShape::Plain),
    ("YAGRA_FLOW_RETENTION_DAYS", EnvShape::Plain),
    ("YAGRA_PAT_OIDC_IDLE_DAYS", EnvShape::Plain),
    ("YAGRA_IPASN_DB", EnvShape::Plain),
    ("YAGRA_IPASN_RELOAD_SECS", EnvShape::Plain),
    // Key material: the PATH only, never the contents. "Is it mounted?" is the question.
    ("YAGRA_KEK_FILE", EnvShape::Plain),
    ("YAGRA_SESSION_KEY_FILE", EnvShape::Plain),
    ("YAGRA_NATS_CALLOUT_SEED_FILE", EnvShape::Plain),
    ("YAGRA_NATS_CALLOUT_ACCOUNT", EnvShape::Plain),
    ("YAGRA_TLS_DIR", EnvShape::Plain),
    // Observability — including this bundle's own inputs, so "why are there no logs?" is answerable
    ("YAGRA_LOG_DIR", EnvShape::Plain),
    ("YAGRA_LOG_RETAIN_HOURS", EnvShape::Plain),
    ("RUST_LOG", EnvShape::Plain),
    ("YAGRA_OTEL_ENDPOINT", EnvShape::Url),
    ("OTEL_EXPORTER_OTLP_ENDPOINT", EnvShape::Url),
    ("OTEL_TRACES_SAMPLER", EnvShape::Plain),
    ("OTEL_TRACES_SAMPLER_ARG", EnvShape::Plain),
    ("TZ", EnvShape::Plain),
];

/// One environment variable as the bundle carries it.
#[derive(Debug, Clone, Serialize)]
pub struct EnvEntry {
    pub name: &'static str,
    /// `None` when the variable is unset — which is itself an answer, and a more useful one than
    /// omitting the row.
    pub value: Option<String>,
    pub shape: EnvShape,
}

/// Read [`ENV_ALLOWLIST`] out of the process environment, applying each entry's shape.
#[must_use]
pub fn env_snapshot() -> Vec<EnvEntry> {
    ENV_ALLOWLIST
        .iter()
        .map(|&(name, shape)| {
            let value = std::env::var(name).ok().map(|v| match shape {
                EnvShape::Plain => v,
                EnvShape::Url => yagra_bus::redact_url(&v),
            });
            EnvEntry { name, value, shape }
        })
        .collect()
}

// ── Redaction scan ────────────────────────────────────────────────────────────────────────────

/// The fail-closed check that runs over every assembled byte before the archive is written.
///
/// Two rule families, and the order of value between them is worth stating: the **literals** are
/// stronger than the patterns. A pattern set can only describe leaks somebody imagined; the literal
/// set is built from the secrets this process can actually see, so it catches one arriving through
/// a path nobody anticipated — a library that logged a connection string, a future section that
/// serialized a config row without thinking.
pub struct SecretScan {
    /// Exact values that must appear nowhere. Never logged, never put in an error.
    literals: Vec<String>,
    /// How many candidate secrets were seen and **not** enforced because they were shorter than
    /// [`MIN_SECRET_LEN`].
    ///
    /// Reported in the manifest, and the reason is worth stating: without it, `literals = 0` has
    /// three meanings a reviewer cannot tell apart — this process holds no secrets, it holds some
    /// that were too short to enforce safely, or the collection is broken. The first is a clean
    /// bill of health and the third is a silent failure of the strongest rule in the scan. Found
    /// on the first real bundle (2026-08-06), where establishing which of the three it was took an
    /// SSH session into the deployment — exactly the work this artefact exists to remove.
    skipped_short: usize,
}

/// Environment-variable name fragments whose value is treated as a secret literal. Matched
/// case-insensitively against the **name**, so the value never has to look like anything.
const SECRET_NAME_MARKERS: &[&str] = &[
    "PASSWORD",
    "PASSWD",
    "SECRET",
    "TOKEN",
    "APIKEY",
    "API_KEY",
    "PRIVATE",
    "CREDENTIAL",
];

impl SecretScan {
    /// Build the scan from the process environment.
    ///
    /// Collects two things: the value of every variable whose *name* marks it secret, and the
    /// password out of the userinfo of every URL-shaped value (which is how the database and bus
    /// credentials actually reach this process).
    #[must_use]
    pub fn from_env() -> Self {
        let (literals, skipped_short) = literals_from(std::env::vars());
        Self {
            literals,
            skipped_short,
        }
    }

    /// A scan with no literals — the shape a deployment with no secrets in its environment gets,
    /// and what the unit tests use so they exercise the patterns without touching process state.
    #[cfg(test)]
    #[must_use]
    pub fn patterns_only() -> Self {
        Self {
            literals: Vec::new(),
            skipped_short: 0,
        }
    }

    /// How many literal values this scan is enforcing, and how many candidates it declined as too
    /// short. **Read as a pair** — see [`SecretScan::skipped_short`].
    #[must_use]
    pub fn literal_counts(&self) -> (usize, usize) {
        (self.literals.len(), self.skipped_short)
    }

    /// Scan one entry. `Some(rule)` names the rule that fired; the matched text is deliberately not
    /// returned, so a caller cannot accidentally log it.
    #[must_use]
    pub fn check(&self, bytes: &[u8]) -> Option<&'static str> {
        // Non-UTF-8 cannot occur — every entry is JSON or text this process produced — but a
        // lossy read is the safe reaction rather than skipping the scan for that entry.
        let text = String::from_utf8_lossy(bytes);
        if self.literals.iter().any(|lit| text.contains(lit.as_str())) {
            return Some("a secret value from this process's environment");
        }
        if text.contains("PRIVATE KEY-----") {
            return Some("a PEM private key block");
        }
        if contains_pat(&text) {
            return Some("a Yagra personal access token (yat_…)");
        }
        if contains_url_userinfo(&text) {
            return Some("a URL carrying user:password@ userinfo");
        }
        None
    }
}

/// The literal secret values implied by a set of environment variables, and how many candidates
/// were declined for being shorter than [`MIN_SECRET_LEN`].
///
/// Split out from [`SecretScan::from_env`] so it is testable without `std::env::set_var`, which
/// races every other test in the binary.
///
/// Two sources, and the second is the one that earns the function: a variable whose **name** marks
/// it secret, and the password inside the userinfo of any URL-shaped value. `YAGRA_DATABASE_URL`
/// carries a credential under a name no marker list would flag.
///
/// The skip count exists so that "nothing to enforce" and "something to enforce that we declined"
/// are distinguishable in the manifest. Counting is the whole fix — **the floor itself must not be
/// lowered**: a lab `POSTGRES_PASSWORD=yagra` would forbid the substring `yagra`, which is in every
/// path and table name in the bundle, and the scan would refuse every bundle forever.
fn literals_from(vars: impl Iterator<Item = (String, String)>) -> (Vec<String>, usize) {
    let mut literals = Vec::new();
    let mut skipped_short = 0usize;
    for (name, value) in vars {
        let upper = name.to_ascii_uppercase();
        let named_secret = SECRET_NAME_MARKERS.iter().any(|m| upper.contains(m));
        // An empty value is *absence*, not a secret too short to enforce — counting it would
        // report a skip on every deployment that simply leaves an optional variable unset.
        if named_secret && !value.is_empty() {
            if value.len() >= MIN_SECRET_LEN {
                literals.push(value.clone());
            } else {
                skipped_short += 1;
            }
        }
        if value.contains("://") {
            if let (_, Some(password)) = yagra_bus::split_userinfo_password(&value) {
                if password.len() >= MIN_SECRET_LEN {
                    literals.push(password);
                } else if !password.is_empty() {
                    skipped_short += 1;
                }
            }
        }
    }
    literals.sort_unstable();
    literals.dedup();
    (literals, skipped_short)
}

/// Whether the text holds something shaped like a Yagra PAT: the `yat_` prefix followed by enough
/// token characters to be a real one rather than the prose word.
fn contains_pat(text: &str) -> bool {
    text.match_indices("yat_").any(|(i, _)| {
        let tail = &text[i + 4..];
        tail.chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
            .count()
            >= 16
    })
}

/// Whether the text holds a URL whose authority carries `user:password@`.
///
/// Scoped to the authority — the span between `://` and the first `/`, `?` or `#` — for the same
/// reason [`yagra_bus::redact_url`] is: a `:` and an `@` elsewhere in a URL (a path, a query
/// string, an email address in a log line) are not credentials, and treating them as such would
/// refuse bundles over ordinary text.
fn contains_url_userinfo(text: &str) -> bool {
    text.match_indices("://").any(|(i, _)| {
        let rest = &text[i + 3..];
        let authority_end = rest
            .find(['/', '?', '#', ' ', '"', '\n', '\r', '\t'])
            .unwrap_or(rest.len());
        let authority = &rest[..authority_end];
        match authority.rsplit_once('@') {
            // `user@host` with no colon is a username, not a credential — redact_url strips it
            // anyway, but on its own it is not worth refusing a bundle over.
            Some((userinfo, _)) => userinfo.contains(':'),
            None => false,
        }
    })
}

// ── Manifest ──────────────────────────────────────────────────────────────────────────────────

/// One entry in the archive, as the manifest describes it.
#[derive(Debug, Clone, Serialize)]
pub struct ManifestFile {
    pub path: String,
    pub bytes: usize,
    /// What this file answers. Written for the human reviewing the bundle before it leaves, not
    /// for a machine.
    pub description: String,
}

/// Something deliberately **not** carried, and why.
///
/// This list is the reason the bundle can be reviewed at all: "what is in it" invites the question
/// "what else did it take", and an empty answer is not credible. It also records the caps that
/// bit — a truncated log that says nothing about being truncated reads as an empty log.
#[derive(Debug, Clone, Serialize)]
pub struct Omission {
    pub what: String,
    pub why: String,
}

impl Omission {
    fn new(what: impl Into<String>, why: impl Into<String>) -> Self {
        Self {
            what: what.into(),
            why: why.into(),
        }
    }
}

/// What the redaction scan did, reported so a reviewer can see it ran rather than trusting that it
/// did.
#[derive(Debug, Clone, Serialize)]
pub struct RedactionReport {
    pub files_scanned: usize,
    pub bytes_scanned: usize,
    /// How many literal secret values from this process's environment the scan was enforcing.
    pub secret_literals_enforced: usize,
    /// How many candidate secrets were declined for being too short to enforce safely.
    ///
    /// **Read this next to `secret_literals_enforced`, never alone.** `0 / 0` means this process
    /// genuinely holds no secrets in its environment; `0 / 2` means it holds two and the scan is
    /// running on patterns alone, which is a materially weaker check and worth knowing before the
    /// archive leaves the building.
    pub secret_literals_skipped_short: usize,
    /// The rules, in the words the manifest publishes to the reviewer.
    pub rules: Vec<&'static str>,
}

/// The archive's table of contents.
#[derive(Debug, Clone, Serialize)]
pub struct Manifest {
    /// Always [`BUNDLE_FORMAT`].
    pub format: &'static str,
    pub version: u32,
    pub generated_at: DateTime<Utc>,
    pub core_version: &'static str,
    /// The log window actually applied, in hours.
    pub window_hours: u32,
    pub files: Vec<ManifestFile>,
    pub omitted: Vec<Omission>,
    pub redaction: RedactionReport,
}

/// The fixed omissions — true of every bundle, independent of what this run managed to collect.
fn standing_omissions() -> Vec<Omission> {
    vec![
        Omission::new(
            "Monitoring credentials, the KEK, and the session signing key",
            "ADR-018: no code path in this repository turns a sealed secret back into transportable \
             plaintext, and a debugging artefact is not the place to add the first one. Credential \
             *health* — whether stored secrets still decrypt — is carried instead, in \
             health/credentials.json.",
        ),
        Omission::new(
            "Metric samples, passive-event message bodies, and flow records",
            "Bulk observational data, unbounded in size and containing device-supplied text. The \
             bundle carries the shape of these stores (row counts, reachability, collection gaps) \
             rather than their contents; the WebUI answers questions about the contents.",
        ),
        Omission::new(
            "Log files from a poller that is neither co-located nor reachable on the bus",
            "Poller logs arrive by two paths and each covers what the other cannot. A co-located \
             poller's log is read off the shared volume (look for logs/yagra-poller-*) and that is \
             the only path that survives the poller dying. A remote-site poller is asked over the \
             bus and answers with its own log (logs/remote/*) — the only path that reaches another \
             host, and it needs that poller to be running and to advertise the capability. Every \
             site that was asked and produced nothing is listed by name in this same section, with \
             why. What every poller reports regardless — heartbeat counters, poll-loop statistics \
             and host resources — is in health/pollers.json, health/poller_health.json and \
             health/hosts.json.",
        ),
        Omission::new(
            "Why an SNMP walk returned no rows",
            "A poller logs a walk that *failed*, but a walk that succeeded and matched nothing is \
             recorded at debug level (optical and entity) or not at all (MAU). So an empty duplex \
             or media column is consistent with both 'the device does not implement that MIB' and \
             'the rows did not map to an ifIndex', and this bundle cannot tell them apart. \
             Answering it needs the device's own reply — an snmpwalk of the relevant subtree, run \
             with the credential this deployment uses and from the poller's own host.",
        ),
        Omission::new(
            "Node addresses, inventory and thresholds beyond the configuration bundle",
            "config/bundle.json already carries the deployment's configuration in the reviewed, \
             secret-free form ADR-040 defined. Nothing here duplicates it in a less reviewed one. \
             The one exception is deliberate: when this bundle was asked for with `?node_id=`, the \
             node/ section carries that single node's observed state — including its stored \
             interface rows, which are an observation and not configuration, so ADR-040's form has \
             no place for them.",
        ),
    ]
}

// ── Assembly ──────────────────────────────────────────────────────────────────────────────────

/// One file destined for the archive.
pub struct BundleFile {
    pub path: String,
    pub description: String,
    pub bytes: Vec<u8>,
}

/// Accumulates the bundle's files, then scans and writes the archive.
///
/// The two-phase shape is the point: **everything is assembled in memory and scanned as a whole
/// before a single byte of the archive is produced**, so a detection can still refuse the bundle
/// rather than having already streamed half of it to the client.
pub struct BundleBuilder {
    files: Vec<BundleFile>,
    omitted: Vec<Omission>,
    window_hours: u32,
}

impl BundleBuilder {
    #[must_use]
    pub fn new(window_hours: u32) -> Self {
        Self {
            files: Vec::new(),
            omitted: standing_omissions(),
            window_hours,
        }
    }

    /// Add a pretty-printed JSON file. Pretty rather than compact because a human reads this before
    /// it is allowed out of the building, and gzip makes the whitespace free.
    pub fn add_json<T: Serialize>(
        &mut self,
        path: &str,
        description: &str,
        value: &T,
    ) -> Result<(), BundleError> {
        let bytes = serde_json::to_vec_pretty(value)?;
        self.add_bytes(path, description, bytes);
        Ok(())
    }

    /// Add a text or already-encoded file.
    pub fn add_bytes(&mut self, path: &str, description: &str, bytes: Vec<u8>) {
        self.files.push(BundleFile {
            path: path.to_owned(),
            description: description.to_owned(),
            bytes,
        });
    }

    /// Record something this run could not collect, or chose not to.
    /// The paths added so far. Test-only: a collector's job is to decide *what goes in*, and
    /// asserting that through `finish` would mean gzipping and re-parsing a tar to learn a
    /// filename.
    #[cfg(test)]
    pub(crate) fn paths(&self) -> Vec<&str> {
        self.files.iter().map(|f| f.path.as_str()).collect()
    }

    /// What has been declared missing so far, as `(what, why)`. Test-only, and the counterpart to
    /// [`Self::paths`]: for this bundle an omission is a *result*, not an absence, so a collector
    /// that records none is as wrong as one that records the wrong file.
    #[cfg(test)]
    pub(crate) fn omissions(&self) -> Vec<(&str, &str)> {
        self.omitted
            .iter()
            .map(|o| (o.what.as_str(), o.why.as_str()))
            .collect()
    }

    pub fn omit(&mut self, what: impl Into<String>, why: impl Into<String>) {
        self.omitted.push(Omission::new(what, why));
    }

    /// Scan everything, prepend the manifest and README, and produce the gzipped tar.
    ///
    /// `now` is passed in rather than read here so the timestamp in the manifest, the archive
    /// headers and the download filename are one value.
    pub fn finish(mut self, scan: &SecretScan, now: DateTime<Utc>) -> Result<Vec<u8>, BundleError> {
        let mut bytes_scanned = 0usize;
        for f in &self.files {
            bytes_scanned += f.bytes.len();
            if let Some(rule) = scan.check(&f.bytes) {
                return Err(BundleError::SecretDetected {
                    file: f.path.clone(),
                    rule,
                });
            }
        }

        let manifest = Manifest {
            format: BUNDLE_FORMAT,
            version: BUNDLE_VERSION,
            generated_at: now,
            core_version: env!("CARGO_PKG_VERSION"),
            window_hours: self.window_hours,
            files: self
                .files
                .iter()
                .map(|f| ManifestFile {
                    path: f.path.clone(),
                    bytes: f.bytes.len(),
                    description: f.description.clone(),
                })
                .collect(),
            omitted: std::mem::take(&mut self.omitted),
            redaction: RedactionReport {
                files_scanned: self.files.len(),
                bytes_scanned,
                secret_literals_enforced: scan.literal_counts().0,
                secret_literals_skipped_short: scan.literal_counts().1,
                rules: vec![
                    "no value of a secret-named environment variable of the core process",
                    "no password from the userinfo of any URL the core process holds",
                    "no PEM private key block",
                    "no Yagra personal access token",
                    "no URL carrying user:password@ userinfo",
                ],
            },
        };
        // The manifest and README describe the files, so they are built last and placed first.
        let readme = readme_text(&manifest);
        let manifest_json = serde_json::to_vec_pretty(&manifest)?;

        let mtime = u64::try_from(now.timestamp()).unwrap_or(0);
        let mut tar = tar::Builder::new(flate2::write::GzEncoder::new(
            Vec::new(),
            flate2::Compression::default(),
        ));
        append(&mut tar, "MANIFEST.json", &manifest_json, mtime)?;
        append(&mut tar, "README.txt", readme.as_bytes(), mtime)?;
        for f in &self.files {
            append(&mut tar, &f.path, &f.bytes, mtime)?;
        }
        let gz = tar.into_inner()?;
        Ok(gz.finish()?)
    }
}

/// Append one entry with an explicit header, so mode and mtime are stated rather than inherited
/// from a filesystem this data never touched.
fn append<W: Write>(
    tar: &mut tar::Builder<W>,
    path: &str,
    bytes: &[u8],
    mtime: u64,
) -> std::io::Result<()> {
    let mut header = tar::Header::new_gnu();
    header.set_size(bytes.len() as u64);
    header.set_mode(0o644);
    header.set_mtime(mtime);
    tar.append_data(&mut header, path, bytes)
}

/// The human-facing index, written for whoever has to approve the archive leaving the site.
fn readme_text(m: &Manifest) -> String {
    let mut s = String::new();
    s.push_str("Yagra support bundle\n====================\n\n");
    s.push_str(&format!(
        "Generated : {}\nCore      : v{}\nLog window: the last {} hour(s)\nFormat    : {} v{}\n\n",
        m.generated_at.to_rfc3339(),
        m.core_version,
        m.window_hours,
        m.format,
        m.version
    ));
    s.push_str(
        "This archive is a diagnostic snapshot of one Yagra deployment. Every file in it is JSON\n\
         or plain text and can be read before the archive is released.\n\n\
         WHAT IS IN IT\n-------------\n",
    );
    for f in &m.files {
        s.push_str(&format!(
            "  {:<40} {:>10} bytes  {}\n",
            f.path, f.bytes, f.description
        ));
    }
    s.push_str(
        "\nWHAT IS DELIBERATELY NOT IN IT\n------------------------------\n\
         (the same list is in MANIFEST.json, with the reasons machine-readable)\n\n",
    );
    for o in &m.omitted {
        s.push_str(&format!("  * {}\n      {}\n", o.what, o.why));
    }
    s.push_str(&format!(
        "\nREDACTION\n---------\n\
         {} file(s) / {} byte(s) were scanned before this archive was written, enforcing {} literal\n\
         secret value(s) taken from the core process's own environment plus these rules:\n",
        m.redaction.files_scanned, m.redaction.bytes_scanned, m.redaction.secret_literals_enforced
    ));
    // The two counts are only meaningful together, so the sentence that interprets them is written
    // here rather than left to the reader. A bare "0" reads as a clean bill of health and can mean
    // the opposite.
    s.push_str(&match (
        m.redaction.secret_literals_enforced,
        m.redaction.secret_literals_skipped_short,
    ) {
        (0, 0) => "  (no literal values to enforce: this core process holds no secrets in its \
                   environment)\n"
            .to_owned(),
        (_, 0) => String::new(),
        // Line breaks are placed so no phrase straddles one — a reader scanning this paragraph
        // should be able to take any single line at face value.
        (0, n) => format!(
            "  ⚠ {n} secret value(s) in this environment are under {MIN_SECRET_LEN} characters, \
             so they were NOT\n\
             \x20   enforced as literals: a short value occurs by chance in ordinary text and \
             would\n\
             \x20   refuse every bundle. The strongest rule therefore did not run, and this\n\
             \x20   archive was checked by pattern matching alone.\n\
             \x20   Lengthen those secrets to restore it.\n"
        ),
        (_, n) => format!(
            "  ⚠ a further {n} value(s) were too short to enforce as literals (under \
             {MIN_SECRET_LEN} characters).\n"
        ),
    });
    for rule in &m.redaction.rules {
        s.push_str(&format!("  - {rule}\n"));
    }
    s.push_str(
        "\nA match on any rule aborts the export: this archive exists, therefore no rule matched.\n\
         That is a guarantee about the rules listed, not a proof that the archive is harmless.\n\
         Read it before releasing it.\n",
    );
    s
}

// ── Log files ─────────────────────────────────────────────────────────────────────────────────

/// Reading rotated log files back is [`yagra_telemetry`]'s job, because it is the thing that named
/// and wrote them: the collector's correctness is entirely a property of `file_prefix` plus
/// `tracing-appender`'s `YYYY-MM-DD-HH` suffix.
///
/// Re-exported here rather than wrapped so this module's callers keep one import, and so the second
/// consumer added in ADR-045 Inc.4 — a remote poller collecting *its own* log to answer a request —
/// selects files by the identical rules. Two collectors would mean a remote site's contribution to a
/// bundle was chosen differently from a co-located one's, which is a difference nobody would notice
/// until the two disagreed about which hour to keep.
pub use yagra_telemetry::collect_logs;

// ── PostgreSQL introspection ──────────────────────────────────────────────────────────────────

/// One applied migration, as `sqlx` records it.
///
/// The checksum is carried in full and on purpose: `sqlx::migrate!` verifies it at startup, so an
/// edited-after-the-fact migration file is a **startup failure** with a mismatch as its only
/// symptom. Comparing this column against a known-good deployment is how that gets diagnosed
/// without a shell.
#[derive(Debug, Clone, Serialize)]
pub struct MigrationRow {
    pub version: i64,
    pub description: String,
    pub installed_on: DateTime<Utc>,
    pub success: bool,
    pub checksum: String,
    pub execution_time_ms: i64,
}

/// Per-table size, from PostgreSQL's own statistics.
///
/// `n_live_tup` is the planner's estimate, not a `COUNT(*)`. That is the right trade: an exact
/// count of a fifty-million-row metrics-adjacent table would make producing a bundle an expensive
/// operation on the deployment already in trouble, and an estimate answers every question a bundle
/// is asked ("is this table empty / normal / enormous").
#[derive(Debug, Clone, Serialize)]
pub struct TableStat {
    pub table: String,
    pub estimated_rows: i64,
    pub dead_rows: i64,
    pub total_bytes: i64,
}

/// Connection counts by state.
///
/// The `query` column of `pg_stat_activity` is **not** selected. It is the one place in this whole
/// module where user- or device-derived text could enter, and "the SQL currently running" is not
/// worth that.
#[derive(Debug, Clone, Serialize)]
pub struct ConnectionStat {
    pub state: String,
    pub connections: i64,
    pub longest_seconds: Option<f64>,
}

/// Read-only PostgreSQL introspection for the bundle.
pub struct SupportRepo {
    pool: PgPool,
}

impl SupportRepo {
    #[must_use]
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Applied migrations, oldest first.
    pub async fn migrations(&self) -> Result<Vec<MigrationRow>, sqlx::Error> {
        let rows = sqlx::query(
            "SELECT version, description, installed_on, success, \
                    encode(checksum, 'hex') AS checksum, execution_time \
             FROM _sqlx_migrations ORDER BY version",
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(|r| MigrationRow {
                version: r.get("version"),
                description: r.get("description"),
                installed_on: r.get("installed_on"),
                success: r.get("success"),
                checksum: r.get("checksum"),
                // sqlx records nanoseconds; milliseconds is the resolution anyone reads.
                execution_time_ms: r.get::<i64, _>("execution_time") / 1_000_000,
            })
            .collect())
    }

    /// Every user table with its estimated row count and on-disk size, largest first.
    ///
    /// Derived from `pg_stat_user_tables` rather than a hand-maintained table list, so a new
    /// migration's table appears here without anyone remembering to add it.
    pub async fn table_stats(&self) -> Result<Vec<TableStat>, sqlx::Error> {
        let rows = sqlx::query(
            "SELECT relname, n_live_tup, n_dead_tup, \
                    pg_total_relation_size(relid) AS total_bytes \
             FROM pg_stat_user_tables ORDER BY n_live_tup DESC",
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(|r| TableStat {
                table: r.get("relname"),
                estimated_rows: r.get("n_live_tup"),
                dead_rows: r.get("n_dead_tup"),
                total_bytes: r.get("total_bytes"),
            })
            .collect())
    }

    /// Connections to this database, grouped by state.
    pub async fn connection_stats(&self) -> Result<Vec<ConnectionStat>, sqlx::Error> {
        let rows = sqlx::query(
            "SELECT COALESCE(state, 'unknown') AS state, COUNT(*) AS connections, \
                    MAX(EXTRACT(EPOCH FROM (now() - state_change)))::float8 AS longest_seconds \
             FROM pg_stat_activity WHERE datname = current_database() \
             GROUP BY 1 ORDER BY 2 DESC",
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(|r| ConnectionStat {
                state: r.get("state"),
                connections: r.get("connections"),
                // Clamped at zero. `now()` is the transaction's start time, and the backend running
                // this very query set its own `state_change` *after* that — so the `active` row
                // reliably comes back a few milliseconds negative. A negative age is not a fact
                // about the deployment, it is an artefact of measuring from inside.
                longest_seconds: r
                    .get::<Option<f64>, _>("longest_seconds")
                    .map(|s| s.max(0.0)),
            })
            .collect())
    }
}

// ── Provenance ────────────────────────────────────────────────────────────────────────────────

/// Which binary is actually running.
///
/// **Read this first when debugging a report from the field.** The two files are written by
/// `docker/yagra-rust.Dockerfile` at build time, and the pair is what distinguishes a release build
/// from a `/flashdeploy` build of the *same commit* — different binaries with the same source ref.
/// A green pipeline has shipped a stale binary in this repository before; the commit alone does not
/// settle the question.
#[derive(Debug, Clone, Serialize)]
pub struct Provenance {
    /// The crate version this core was compiled from.
    pub core_version: &'static str,
    /// Contents of `/etc/yagra-source-ref` — the commit the image was built from.
    pub source_ref: Option<String>,
    /// Contents of `/etc/yagra-build-profile` — `release` or `ci-fast`.
    pub build_profile: Option<String>,
    /// The container's hostname, which is how a pod or replica identifies itself in the logs.
    pub hostname: Option<String>,
    /// Seconds since this process installed its telemetry, i.e. process uptime.
    pub uptime_seconds: u64,
}

/// Read the image's build-provenance files. Absent files are reported as `null` rather than an
/// error — a locally-run `cargo run` has neither, and that is a valid deployment to bundle.
#[must_use]
pub fn provenance(started: SystemTime) -> Provenance {
    let read = |p: &str| {
        std::fs::read_to_string(p)
            .ok()
            .map(|s| s.trim().to_owned())
            .filter(|s| !s.is_empty())
    };
    Provenance {
        core_version: env!("CARGO_PKG_VERSION"),
        source_ref: read("/etc/yagra-source-ref"),
        build_profile: read("/etc/yagra-build-profile"),
        hostname: read("/etc/hostname"),
        uptime_seconds: SystemTime::now()
            .duration_since(started)
            .map(|d| d.as_secs())
            .unwrap_or(0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scan_with(literals: &[&str]) -> SecretScan {
        SecretScan {
            literals: literals.iter().map(|s| (*s).to_owned()).collect(),
            skipped_short: 0,
        }
    }

    /// Run `literals_from` over a fixture without touching process state.
    fn literals_of(vars: &[(&str, &str)]) -> (Vec<String>, usize) {
        literals_from(vars.iter().map(|(k, v)| ((*k).to_owned(), (*v).to_owned())))
    }

    /// The rule that matters most, and the one a pattern list cannot express: a literal secret from
    /// this process's environment appearing anywhere at all, in any shape, in any file.
    #[test]
    fn a_literal_environment_secret_is_caught_wherever_it_appears() {
        let scan = scan_with(&["hunter2-correct-horse"]);
        assert!(scan
            .check(b"{\"detail\":\"connect failed for hunter2-correct-horse\"}")
            .is_some());
        // …and an unrelated body is not a false positive.
        assert!(scan.check(b"{\"detail\":\"connect failed\"}").is_none());
    }

    /// The three patterns, each on the shape that actually ships.
    #[test]
    fn the_patterns_catch_the_shapes_that_actually_leak() {
        let scan = SecretScan::patterns_only();
        assert_eq!(
            scan.check(b"-----BEGIN PRIVATE KEY-----\nMIIE"),
            Some("a PEM private key block")
        );
        assert!(scan
            .check(b"Authorization: Bearer yat_A1b2C3d4E5f6G7h8J9k0")
            .is_some_and(|r| r.contains("personal access token")));
        assert_eq!(
            scan.check(b"postgres://yagra:s3cretpw@postgres:5432/yagra"),
            Some("a URL carrying user:password@ userinfo")
        );
    }

    /// The false positives that would make the scan unusable if it were naive. Each of these
    /// appears in ordinary bundle content, and refusing on any of them would get the feature
    /// switched off rather than fixed.
    #[test]
    fn ordinary_bundle_text_is_not_refused() {
        let scan = SecretScan::patterns_only();
        for benign in [
            // A URL with no userinfo — every store URL after redact_url has run.
            &b"postgres://postgres:5432/yagra"[..],
            // A colon and an @ outside the authority: a path, a query, an address in a log line.
            b"https://example.com/a:b@c?x=y",
            b"{\"msg\":\"mail from ops:team@example.com queued\"}",
            // The prose word, not a token.
            b"the yat_ prefix identifies a personal access token",
            // A short yat_-like id that is not long enough to be a real token.
            b"yat_short",
            // The words, without the PEM block.
            b"{\"detail\":\"the private key file is not mounted\"}",
        ] {
            assert_eq!(
                scan.check(benign),
                None,
                "{:?}",
                String::from_utf8_lossy(benign)
            );
        }
    }

    /// Where the literals come from, and the two cases that are easy to get wrong.
    ///
    /// A URL credential has an innocuous variable name, so the name markers alone would miss the
    /// one that actually ships. And a short value must NOT be enforced: a lab
    /// `POSTGRES_PASSWORD=yagra` would forbid the substring `yagra`, which appears in every path
    /// and table name in the bundle — the scan would refuse every bundle forever, and the feature
    /// would be switched off rather than fixed.
    #[test]
    fn literals_come_from_names_and_from_url_userinfo_but_never_from_short_values() {
        let (got, skipped) = literals_of(&[
            ("YAGRA_NATS_POLLER_PASSWORD", "poller-secret-value"),
            (
                "YAGRA_DATABASE_URL",
                "postgres://yagra:db-secret-value@pg/yagra",
            ),
            // No marker in the name, no userinfo in the value — carries nothing.
            ("YAGRA_API_ADDR", "0.0.0.0:8080"),
            // Marked, but too short to enforce without poisoning every bundle.
            ("POSTGRES_PASSWORD", "yagra"),
        ]);
        assert!(got.contains(&"poller-secret-value".to_owned()));
        assert!(
            got.contains(&"db-secret-value".to_owned()),
            "the password inside a URL is the one a name-marker list misses"
        );
        assert!(!got.contains(&"yagra".to_owned()), "too short to enforce");
        assert!(!got.iter().any(|l| l.contains("0.0.0.0")));
        assert_eq!(skipped, 1, "the declined short secret is counted, not lost");
    }

    /// The three states of `secret_literals_enforced` must be distinguishable, because a bare `0`
    /// is the one number in this artefact that can mean its own opposite.
    ///
    /// Regression from the first real bundle (2026-08-06): the test deployment reported `0`, and
    /// establishing whether that meant "no secrets" or "the strongest rule silently did not run"
    /// required an SSH session into the deployment — exactly the work a support bundle exists to
    /// remove.
    #[test]
    fn an_empty_literal_set_says_whether_there_was_nothing_or_something_declined() {
        // Nothing to enforce — a genuinely clean environment.
        assert_eq!(
            literals_of(&[("YAGRA_API_ADDR", "0.0.0.0:8080")]),
            (vec![], 0)
        );

        // Something to enforce, declined: the shape the test server actually produced — a 5-char
        // database password inside a URL, and an unset admin password beside it.
        let (literals, skipped) = literals_of(&[
            (
                "YAGRA_DATABASE_URL",
                "postgres://yagra:yagra@postgres/yagra",
            ),
            ("YAGRA_ADMIN_PASSWORD", ""),
        ]);
        assert!(literals.is_empty());
        assert_eq!(
            skipped, 1,
            "the short URL password counts; the UNSET admin password does not — absence is not a \
             declined secret, and counting it would report a skip on every default deployment"
        );
    }

    /// The README has to interpret the pair, not print it. A reviewer reading `0` needs the
    /// sentence, and the warning has to be visibly different from the all-clear.
    #[test]
    fn the_readme_distinguishes_a_clean_zero_from_a_weakened_scan() {
        let clean = readme_for(0, 0);
        assert!(clean.contains("no literal values to enforce"));
        assert!(!clean.contains('⚠'), "a clean environment gets no warning");

        let weakened = readme_for(0, 2);
        assert!(weakened.contains('⚠'));
        assert!(
            weakened.contains("pattern matching alone"),
            "it must say what was lost, not just that something was skipped"
        );
        assert!(
            weakened.contains("Lengthen those secrets"),
            "and what to do"
        );

        // Enforcing some and declining others is a footnote, not an alarm.
        let partial = readme_for(3, 1);
        assert!(partial.contains("a further 1 value(s)"));
        assert!(!partial.contains("pattern matching alone"));
    }

    /// Render just the README for a given redaction outcome.
    fn readme_for(enforced: usize, skipped: usize) -> String {
        let mut b = BundleBuilder::new(6);
        b.add_bytes("x.json", "a section", b"{}".to_vec());
        let mut m = Manifest {
            format: BUNDLE_FORMAT,
            version: BUNDLE_VERSION,
            generated_at: Utc::now(),
            core_version: "0.0.0",
            window_hours: 6,
            files: Vec::new(),
            omitted: Vec::new(),
            redaction: RedactionReport {
                files_scanned: 1,
                bytes_scanned: 2,
                secret_literals_enforced: enforced,
                secret_literals_skipped_short: skipped,
                rules: vec!["a rule"],
            },
        };
        m.files.push(ManifestFile {
            path: "x.json".to_owned(),
            bytes: 2,
            description: "a section".to_owned(),
        });
        readme_text(&m)
    }

    /// The allow-list must not acquire a secret by name. `YAGRA_NATS_POLLER_PASSWORD` is the one
    /// that would be easiest to add "for completeness" — it sits beside the callout account in
    /// `config.rs` and looks like configuration.
    #[test]
    fn the_env_allowlist_carries_no_secret_and_redacts_every_url() {
        for (name, shape) in ENV_ALLOWLIST {
            let upper = name.to_ascii_uppercase();
            assert!(
                !SECRET_NAME_MARKERS.iter().any(|m| upper.contains(m)),
                "{name} looks like a secret and must not be carried"
            );
            // Anything whose name ends in _URL must be redacted, not carried verbatim.
            if upper.ends_with("_URL") || upper.ends_with("_ENDPOINT") {
                assert_eq!(*shape, EnvShape::Url, "{name} must be redacted");
            }
        }
        // And the list is a list, not a bag: a duplicated name would be carried twice.
        let mut names: Vec<&str> = ENV_ALLOWLIST.iter().map(|(n, _)| *n).collect();
        names.sort_unstable();
        let before = names.len();
        names.dedup();
        assert_eq!(before, names.len(), "duplicate name in ENV_ALLOWLIST");
    }

    /// A detection aborts the whole export. Not "redacts the file and continues" — the archive must
    /// not exist at all, because its whole purpose is to be carried out of a place data does not
    /// leave.
    #[test]
    fn a_detection_refuses_the_bundle_rather_than_redacting_it() {
        let mut b = BundleBuilder::new(6);
        b.add_bytes("health/x.json", "a section", b"{\"ok\":true}".to_vec());
        b.add_bytes(
            "logs/core.log",
            "the log",
            b"connecting to postgres://yagra:s3cretpw@db:5432/yagra".to_vec(),
        );
        let err = b
            .finish(&SecretScan::patterns_only(), Utc::now())
            .expect_err("must refuse");
        match err {
            BundleError::SecretDetected { file, rule } => {
                assert_eq!(file, "logs/core.log");
                assert!(rule.contains("userinfo"));
                // The error names the rule and the file, never the value — it is itself logged.
                assert!(!rule.contains("s3cretpw"));
            }
            other => panic!("wrong error: {other}"),
        }
    }

    /// A clean bundle produces a real gzip archive whose manifest lists every file, and whose
    /// omissions list is non-empty — an empty "what we did not take" is not a credible answer to a
    /// reviewer.
    #[test]
    fn a_clean_bundle_is_a_gzip_archive_with_a_complete_manifest() {
        let mut b = BundleBuilder::new(6);
        b.add_json("health/version.json", "the running version", &"v0.1.22")
            .unwrap();
        b.add_bytes("metrics/core.prom", "the scrape", b"yagra_up 1\n".to_vec());
        b.omit("something", "a reason");
        let out = b.finish(&SecretScan::patterns_only(), Utc::now()).unwrap();

        // gzip magic — the response really is an archive, not a JSON error rendered as one.
        assert_eq!(&out[..2], &[0x1f, 0x8b]);

        let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(&out[..]));
        let mut seen = Vec::new();
        let mut manifest = String::new();
        for entry in archive.entries().unwrap() {
            let mut entry = entry.unwrap();
            let path = entry.path().unwrap().display().to_string();
            if path == "MANIFEST.json" {
                use std::io::Read as _;
                entry.read_to_string(&mut manifest).unwrap();
            }
            seen.push(path);
        }
        assert_eq!(
            seen,
            vec![
                "MANIFEST.json",
                "README.txt",
                "health/version.json",
                "metrics/core.prom"
            ],
            "the manifest and README come first, so a reader meets them before the data"
        );
        let m: serde_json::Value = serde_json::from_str(&manifest).unwrap();
        assert_eq!(m["format"], BUNDLE_FORMAT);
        assert_eq!(m["version"], BUNDLE_VERSION);
        assert_eq!(m["files"].as_array().unwrap().len(), 2);
        assert!(
            m["omitted"].as_array().unwrap().len() > 1,
            "the standing omissions plus the run's own"
        );
        assert_eq!(m["redaction"]["files_scanned"], 2);
        // Both halves of the literal count are always present, so a reviewer never has to work out
        // which meaning a bare zero carries.
        assert_eq!(m["redaction"]["secret_literals_enforced"], 0);
        assert_eq!(m["redaction"]["secret_literals_skipped_short"], 0);
    }

    /// The manifest describes exactly the entries the archive holds. A file added to one and not
    /// the other would make the reviewed list and the released bytes different things.
    #[test]
    fn the_manifest_describes_exactly_what_the_archive_carries() {
        let mut b = BundleBuilder::new(1);
        for i in 0..5 {
            b.add_bytes(&format!("s/{i}.json"), "a section", vec![b'x'; 10 + i]);
        }
        let out = b.finish(&SecretScan::patterns_only(), Utc::now()).unwrap();
        let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(&out[..]));
        let mut in_archive: Vec<(String, u64)> = archive
            .entries()
            .unwrap()
            .map(|e| {
                let e = e.unwrap();
                (e.path().unwrap().display().to_string(), e.size())
            })
            .filter(|(p, _)| p != "MANIFEST.json" && p != "README.txt")
            .collect();
        in_archive.sort();

        let mut archive2 = tar::Archive::new(flate2::read::GzDecoder::new(&out[..]));
        let manifest: serde_json::Value = archive2
            .entries()
            .unwrap()
            .map(Result::unwrap)
            .find(|e| e.path().unwrap().display().to_string() == "MANIFEST.json")
            .map(serde_json::from_reader)
            .unwrap()
            .unwrap();
        let mut in_manifest: Vec<(String, u64)> = manifest["files"]
            .as_array()
            .unwrap()
            .iter()
            .map(|f| {
                (
                    f["path"].as_str().unwrap().to_owned(),
                    f["bytes"].as_u64().unwrap(),
                )
            })
            .collect();
        in_manifest.sort();
        assert_eq!(in_archive, in_manifest);
    }

    /// The README is what a reviewer reads, so the omissions have to reach it — not just
    /// MANIFEST.json. This is the check that the human-facing half stays in sync with the machine
    /// one.
    #[test]
    fn the_readme_states_both_halves_of_the_contract() {
        let mut b = BundleBuilder::new(6);
        b.add_bytes("health/version.json", "the running version", b"{}".to_vec());
        let out = b.finish(&SecretScan::patterns_only(), Utc::now()).unwrap();
        let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(&out[..]));
        let mut readme = String::new();
        for entry in archive.entries().unwrap() {
            let mut entry = entry.unwrap();
            if entry.path().unwrap().display().to_string() == "README.txt" {
                use std::io::Read as _;
                entry.read_to_string(&mut readme).unwrap();
            }
        }
        assert!(readme.contains("health/version.json"), "lists the files");
        assert!(readme.contains("WHAT IS DELIBERATELY NOT IN IT"));
        assert!(readme.contains("ADR-018"), "names why secrets are absent");
        assert!(
            readme.contains("Read it before releasing it."),
            "the scan is a guarantee about its rules, not a clean bill of health"
        );
    }
}
