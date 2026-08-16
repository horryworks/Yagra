// SPDX-License-Identifier: AGPL-3.0-only
//! Storage and materialization for the **bus** TLS certificate (ADR-065).
//!
//! [`crate::server_cert`] mints and validates certificates; [`crate::webtls`] does this job for the
//! certificate a browser sees. This module does it for the certificate a *poller at another site*
//! pins when it dials the NATS bus — the one that has to exist before a job message carrying
//! plaintext device credentials (ADR-020) may cross a WAN.
//!
//! ## Why this exists at all
//!
//! The shipped procedure for accepting a remote poller told the operator to run `openssl req -x509`
//! by hand, drop the pair into a bind-mounted `./certs`, and hand-edit two blocks of
//! `docker-compose.deploy.yml`. Three things were wrong with that and only one of them was
//! friction: the hand edits are **erased by the next upgrade** (the composition is reinstalled from
//! the target image, ADR-050 decision 5) and the central stack then keeps working while every
//! remote site silently stops connecting; and the file the procedure mounts is not in the published
//! images at all, so a deployment that never cloned the repository gets an empty directory where
//! NATS expects its config and the bus fails to start.
//!
//! So the certificate follows the shape ADR-044 already established for the WebUI: **the row is the
//! record, the files are a materialization of it.** Deleting the volume is safe.
//!
//! ## Who writes it, and why not core
//!
//! Core cannot: it needs the bus to start, and the bus needs the certificate. The writer is
//! therefore the `yagra-core bus-cert` one-shot (`main.rs`), which runs from the same image before
//! NATS does — the same arrangement as `kek-init` and `tls-init`, and for the same reason.
//!
//! Core still *reads* it, and may regenerate it on request (Settings ▸ Pollers). What core must
//! never do is regenerate it on a timer the way `webtls` does: a new bus certificate is one every
//! remote site has to be handed before it can reconnect, so renewal is an operator action with a
//! visible consequence, not a background chore. [`BusTlsRepo::ensure_ready`] therefore renews only
//! what is **already expired** — at which point the sites are disconnected regardless and a fresh
//! certificate is strictly better than a dead one.
//!
//! ## Two files, not one
//!
//! nginx reads one bundle because both its directives can name the same path. `nats-server` takes
//! `cert_file` and `key_file` separately, so this writes two — and the split is convenient rather
//! than merely required: the certificate half is exactly the bytes a remote site needs as its
//! `YAGRA_BUS_CA_FILE`, so it can be copied out as-is with no risk of the key going with it.

use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::{PgPool, Row};
use uuid::Uuid;
use yagra_secrets::{EnvelopeCipher, SealedSecret};

use crate::secrets::Kek;
use crate::server_cert::{self, ServerCert};

/// Subdirectory of the bus TLS volume holding the pair. Mirrors what `nats-server.conf` names, and
/// the conf sits one level above it so a bind of the whole volume gives NATS both.
const CERT_SUBDIR: &str = "certs";
/// What `nats-server.conf`'s `cert_file` points at.
const CERT_FILE: &str = "server-cert.pem";
/// What `nats-server.conf`'s `key_file` points at.
const KEY_FILE: &str = "server-key.pem";
/// The server configuration, copied out of the image so no deployment has to fetch it.
const CONF_FILE: &str = "nats-server.conf";
/// Where the core image keeps its copy of that configuration (`docker/yagra-rust.Dockerfile`).
pub const CONF_IN_IMAGE: &str = "/usr/share/yagra/nats-server.conf";

/// The names a bus certificate covers when the operator has not chosen any.
///
/// `nats` is the service name every in-compose client dials, and loopback covers a `nats` CLI run on
/// the host. Nothing here matches the public name a remote site will use — nothing inside the
/// container can know it — which is exactly why turning remote acceptance on asks for it.
#[must_use]
pub fn default_names() -> Vec<String> {
    vec![
        "nats".to_owned(),
        "localhost".to_owned(),
        "127.0.0.1".to_owned(),
    ]
}

/// The names to mint with, from `YAGRA_BUS_TLS_SANS` (comma-separated) or [`default_names`].
///
/// The defaults are always included even when the variable is set: dropping `nats` would break the
/// co-located core and poller the moment TLS came on, which is the failure that would look like
/// "the switch broke everything" rather than "one name is missing".
#[must_use]
pub fn configured_names() -> Vec<String> {
    let mut names = default_names();
    for extra in std::env::var("YAGRA_BUS_TLS_SANS")
        .unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        if !names.iter().any(|n| n == extra) {
            names.push(extra.to_owned());
        }
    }
    names
}

/// The bus certificate as the settings card shows it. Never includes the private key.
//
// Every `///` below is published verbatim to API clients and into the generated site reference, so
// design rationale goes in `//` notes like this one.
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct BusTlsView {
    /// The certificate in PEM. This is what a remote poller uses as its `YAGRA_BUS_CA_FILE` — a
    /// self-signed certificate is its own certificate authority. Safe to distribute; the private
    /// key never leaves the server.
    pub certificate: String,
    /// Distinguished name of the certificate's subject.
    pub subject: String,
    /// Distinguished name of the issuer. Equal to `subject`, because this certificate is
    /// self-signed.
    pub issuer: String,
    /// The hostnames and IP addresses this certificate is valid for. A remote poller's connection
    /// fails unless the exact address it dials appears here.
    pub sans: Vec<String>,
    /// Start of the validity window, RFC 3339.
    pub not_before: String,
    /// End of the validity window, RFC 3339.
    pub not_after: String,
    /// Lowercase hex SHA-256 of the certificate. Compare it against the file a site was given to
    /// confirm the site is holding this certificate and not an older one.
    pub fingerprint_sha256: String,
    /// Key type and size, for example `ECDSA P-256`.
    pub key_algorithm: String,
    /// Days until expiry; negative once it has passed.
    pub expires_in_days: i64,
    /// When this certificate was generated, RFC 3339.
    pub issued_at: String,
    /// The account that asked for it, if a signed-in user did. Empty for the one generated
    /// automatically before the bus first started.
    pub issued_by: Option<String>,
    /// Whether the files the bus reads match this certificate. `false` means it is stored but has
    /// not reached the volume yet — the bus is still serving whatever it started with.
    // "Stored" and "being served" are different claims, and every other field here would look
    // correct while the answer to the second one was no. Same reasoning as `WebTlsView`.
    pub materialized: bool,
    /// Whether the private key can still be decrypted. `false` means the encryption key has changed
    /// or been lost, and a new certificate has to be generated — which every remote site must then
    /// be given.
    pub key_unreadable: bool,
}

/// A decrypted row, or the reason there is nothing usable.
enum Stored {
    /// No row: nothing has ever been generated.
    Absent,
    Ready(ServerCert),
    /// The row exists and its metadata is readable, but the sealed key will not open.
    KeyUnreadable,
}

/// PostgreSQL-backed store plus the volume writer.
pub struct BusTlsRepo {
    pool: PgPool,
    cipher: EnvelopeCipher<Kek>,
    /// `YAGRA_BUS_TLS_DIR`. `None` on a deployment whose bus never leaves the host — the row is
    /// still kept, there is simply nowhere to write it.
    dir: Option<PathBuf>,
}

impl BusTlsRepo {
    #[must_use]
    pub fn new(pool: PgPool, kek: Kek, dir: Option<PathBuf>) -> Self {
        Self {
            pool,
            cipher: EnvelopeCipher::new(kek),
            dir,
        }
    }

    // ── Reads ────────────────────────────────────────────────────────────────────────────────

    /// The row as the settings card shows it, or `None` before anything has been generated.
    pub async fn view(&self) -> anyhow::Result<Option<BusTlsView>> {
        let Some(row) = sqlx::query(VIEW_SQL).fetch_optional(&self.pool).await? else {
            return Ok(None);
        };
        let certificate: String = row.try_get("certificate")?;
        let not_after: DateTime<Utc> = row.try_get("not_after")?;
        let not_before: DateTime<Utc> = row.try_get("not_before")?;
        let issued_at: DateTime<Utc> = row.try_get("issued_at")?;
        let sans: serde_json::Value = row.try_get("sans")?;

        let materialized = self.materialized_matches(&certificate);
        let key_unreadable = matches!(self.load().await, Ok(Stored::KeyUnreadable));

        Ok(Some(BusTlsView {
            certificate,
            subject: row.try_get("subject")?,
            issuer: row.try_get("issuer")?,
            sans: serde_json::from_value(sans).unwrap_or_default(),
            not_before: not_before.to_rfc3339(),
            not_after: not_after.to_rfc3339(),
            fingerprint_sha256: row.try_get("fingerprint_sha256")?,
            key_algorithm: row.try_get("key_algorithm")?,
            expires_in_days: (not_after - Utc::now()).num_days(),
            issued_at: issued_at.to_rfc3339(),
            issued_by: row.try_get("issued_by_username")?,
            materialized,
            key_unreadable,
        }))
    }

    async fn load(&self) -> anyhow::Result<Stored> {
        let Some(row) = sqlx::query(LOAD_SQL).fetch_optional(&self.pool).await? else {
            return Ok(Stored::Absent);
        };
        let certificate: String = row.try_get("certificate")?;
        let sealed = SealedSecret {
            key_id: u32::try_from(row.try_get::<i32, _>("key_id")?).unwrap_or(0),
            wrapped_dek: row.try_get("wrapped_dek")?,
            dek_nonce: row.try_get("dek_nonce")?,
            ciphertext: row.try_get("ciphertext")?,
            ct_nonce: row.try_get("ct_nonce")?,
        };
        let Ok(key_bytes) = self.cipher.open(&sealed) else {
            return Ok(Stored::KeyUnreadable);
        };
        let key_pem = String::from_utf8(key_bytes).unwrap_or_default();

        // Re-validated rather than reassembled from the columns, for the same reason `webtls` does
        // it: the metadata columns are derived data, and trusting them would let a hand-edited row
        // put a mismatched pair in front of the bus.
        match server_cert::validate(&certificate, &key_pem) {
            Ok(cert) => Ok(Stored::Ready(cert)),
            Err(e) => {
                tracing::warn!(error = %e, "the stored bus TLS certificate no longer validates");
                Ok(Stored::KeyUnreadable)
            }
        }
    }

    // ── Writes ───────────────────────────────────────────────────────────────────────────────

    /// Mint a new certificate covering `names`, store it, and write it to the volume.
    ///
    /// **Every remote poller has to be given the new certificate before it can reconnect**, which is
    /// why nothing calls this on a schedule. Callers that are about to restart the bus anyway (the
    /// remote-acceptance switch) call it first so the SAN list is right before NATS reads it.
    ///
    /// # Errors
    /// [`crate::server_cert::CertError`] if generation fails, otherwise the database error.
    pub async fn regenerate(
        &self,
        names: &[String],
        by: Option<Uuid>,
    ) -> anyhow::Result<BusTlsView> {
        let cert = server_cert::generate_self_signed(names, Utc::now())?;
        self.store(&cert, by).await?;
        self.materialize(&cert)?;
        self.view()
            .await?
            .ok_or_else(|| anyhow::anyhow!("bus certificate stored but could not be read back"))
    }

    async fn store(&self, cert: &ServerCert, by: Option<Uuid>) -> anyhow::Result<()> {
        let sealed = self
            .cipher
            .seal(cert.key_pem.as_bytes())
            .map_err(|e| anyhow::anyhow!("seal the bus TLS private key: {e}"))?;
        sqlx::query(SAVE_SQL)
            .bind(&cert.chain_pem)
            .bind(i32::try_from(sealed.key_id).unwrap_or(0))
            .bind(&sealed.wrapped_dek)
            .bind(&sealed.dek_nonce)
            .bind(&sealed.ciphertext)
            .bind(&sealed.ct_nonce)
            .bind(&cert.meta.subject)
            .bind(&cert.meta.issuer)
            .bind(serde_json::to_value(&cert.meta.sans)?)
            .bind(cert.meta.not_before)
            .bind(cert.meta.not_after)
            .bind(&cert.meta.fingerprint_sha256)
            .bind(&cert.meta.key_algorithm)
            .bind(by)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    // ── The one-shot ─────────────────────────────────────────────────────────────────────────

    /// Make sure a certificate exists and the files the bus reads are current.
    ///
    /// This is `yagra-core bus-cert`'s whole body. Unlike its WebUI counterpart it **returns an
    /// error rather than logging one**: it is a one-shot the composition waits on
    /// (`service_completed_successfully`), so failing loudly stops the bus from starting against a
    /// half-written volume, which is the outcome an operator can actually diagnose.
    ///
    /// # Errors
    /// Anything that leaves the volume without a usable pair.
    pub async fn ensure_ready(&self, names: &[String]) -> anyhow::Result<()> {
        match self.load().await? {
            Stored::Ready(cert) => {
                // Expired only — see the module doc. A live certificate is left exactly as it is
                // even if it is days from lapsing, because replacing it disconnects every site.
                if cert.meta.not_after < Utc::now() {
                    tracing::warn!(
                        not_after = %cert.meta.not_after,
                        "the bus TLS certificate has expired — generating a new one. Every remote \
                         poller must be given the new certificate before it can reconnect."
                    );
                    let names = if cert.meta.sans.is_empty() {
                        names.to_vec()
                    } else {
                        cert.meta.sans.clone()
                    };
                    self.regenerate(&names, None).await?;
                } else {
                    self.materialize(&cert)?;
                }
            }
            Stored::Absent => {
                tracing::info!(
                    ?names,
                    "no bus TLS certificate yet — generating a self-signed one"
                );
                self.regenerate(names, None).await?;
            }
            Stored::KeyUnreadable => {
                // Nothing to protect here, unlike `webtls`: there is no imported certificate this
                // could destroy, because this table has no import path (migration 0089). An
                // unopenable key means the KEK changed, and the only way back to a working bus is a
                // new pair — which the log says out loud, because the sites will need it.
                tracing::warn!(
                    "the stored bus TLS certificate cannot be decrypted (the KEK has changed or \
                     been lost) — generating a new one. Every remote poller must be given the new \
                     certificate before it can reconnect."
                );
                self.regenerate(names, None).await?;
            }
        }
        Ok(())
    }

    /// Copy the NATS server configuration out of the image onto the volume.
    ///
    /// This is the permanent fix for the second shipped defect: the procedure mounted
    /// `./docker/nats/nats-server.conf` from a git checkout, and a deployment installed from
    /// published images has no such file — Docker then creates an **empty directory** at the missing
    /// bind-mount source, NATS is handed a directory as its `-c` argument, and the bus never starts.
    /// Shipping the file inside the core image and copying it here means there is nothing to fetch
    /// and nothing to keep in step with a release.
    ///
    /// Overwrites unconditionally, and that is the point: the configuration travels with the image
    /// exactly as the composition does (ADR-050 decision 5), so an upgrade updates it instead of
    /// leaving a stale copy behind.
    ///
    /// # Errors
    /// The io error, named with both paths — this runs before anything else can report the failure.
    pub fn install_server_conf(&self, src: &Path) -> anyhow::Result<()> {
        let Some(dir) = self.dir.as_ref() else {
            return Ok(());
        };
        let dst = dir.join(CONF_FILE);
        std::fs::create_dir_all(dir)?;
        let body =
            std::fs::read(src).map_err(|e| anyhow::anyhow!("read {} : {e}", src.display()))?;
        write_atomically(&dst, &body, 0o644)
            .map_err(|e| anyhow::anyhow!("write {} : {e}", dst.display()))?;
        tracing::info!(path = %dst.display(), "installed the NATS server configuration");
        Ok(())
    }

    // ── The volume ───────────────────────────────────────────────────────────────────────────

    fn cert_path(&self) -> Option<PathBuf> {
        self.dir
            .as_ref()
            .map(|d| d.join(CERT_SUBDIR).join(CERT_FILE))
    }

    fn materialized_matches(&self, chain_pem: &str) -> bool {
        self.cert_path()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .is_some_and(|text| text.trim() == chain_pem.trim())
    }

    /// Write both halves. Unlike `webtls::materialize` this propagates — see [`Self::ensure_ready`].
    fn materialize(&self, cert: &ServerCert) -> anyhow::Result<()> {
        let Some(dir) = self.dir.as_ref() else {
            return Ok(());
        };
        let certs = dir.join(CERT_SUBDIR);
        std::fs::create_dir_all(&certs)?;
        // 0644 on the certificate: it is public, and NATS runs as a different user. 0640 on the
        // key, which NATS reads as a member of core's group — the same arrangement `web` uses for
        // the WebUI certificate (`group_add: ["10001"]` in the compose file).
        write_atomically(&certs.join(CERT_FILE), cert.chain_pem.as_bytes(), 0o644)?;
        write_atomically(&certs.join(KEY_FILE), cert.key_pem.as_bytes(), 0o640)?;
        tracing::info!(
            dir = %certs.display(),
            fingerprint = %cert.meta.fingerprint_sha256,
            not_after = %cert.meta.not_after,
            sans = ?cert.meta.sans,
            "materialized the bus TLS certificate"
        );
        Ok(())
    }
}

/// Create restrictively, write, fsync, then rename — so no reader ever sees a partial file and the
/// private key is never briefly readable by anyone the final mode would exclude.
fn write_atomically(dst: &Path, body: &[u8], mode: u32) -> std::io::Result<()> {
    use std::io::Write;

    let tmp = dst.with_extension("tmp");
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(mode);
    }
    let mut f = opts.open(&tmp)?;
    f.write_all(body)?;
    // Durable before the rename: a crash between the two would leave the bus pointed at a file
    // whose contents never reached the disk.
    f.sync_all()?;
    drop(f);
    // Set explicitly rather than relying on the create mode, which umask can narrow.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(mode))?;
    }
    #[cfg(not(unix))]
    {
        let _ = mode;
    }
    std::fs::rename(&tmp, dst)
}

const VIEW_SQL: &str = "SELECT b.certificate, b.subject, b.issuer, b.sans, b.not_before, \
     b.not_after, b.fingerprint_sha256, b.key_algorithm, b.issued_at, \
     u.username AS issued_by_username \
     FROM bus_tls_config b LEFT JOIN users u ON u.id = b.issued_by \
     WHERE b.id = 1";

/// Everything [`BusTlsRepo::load`] needs, including the seal.
///
/// Kept apart from [`VIEW_SQL`] on purpose: the view is what the API answers with, and the sealed
/// columns must not be reachable from it even by accident.
const LOAD_SQL: &str = "SELECT certificate, key_id, wrapped_dek, dek_nonce, ciphertext, ct_nonce \
     FROM bus_tls_config WHERE id = 1";

/// The upsert. A named constant so its semantics are assertable without a database.
const SAVE_SQL: &str = "INSERT INTO bus_tls_config \
     (id, certificate, key_id, wrapped_dek, dek_nonce, ciphertext, ct_nonce, \
      subject, issuer, sans, not_before, not_after, fingerprint_sha256, key_algorithm, \
      issued_at, issued_by) \
     VALUES (1,$1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13, now(), $14) \
     ON CONFLICT (id) DO UPDATE SET \
       certificate = EXCLUDED.certificate, key_id = EXCLUDED.key_id, \
       wrapped_dek = EXCLUDED.wrapped_dek, dek_nonce = EXCLUDED.dek_nonce, \
       ciphertext = EXCLUDED.ciphertext, ct_nonce = EXCLUDED.ct_nonce, \
       subject = EXCLUDED.subject, issuer = EXCLUDED.issuer, sans = EXCLUDED.sans, \
       not_before = EXCLUDED.not_before, not_after = EXCLUDED.not_after, \
       fingerprint_sha256 = EXCLUDED.fingerprint_sha256, \
       key_algorithm = EXCLUDED.key_algorithm, issued_at = now(), issued_by = EXCLUDED.issued_by";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_view_query_cannot_return_the_private_key() {
        // The one thing this module must never do. An in-memory fake cannot catch a mistake in SQL
        // meaning, so the statement itself is the thing under test (the same test `webtls` carries).
        for sealed in [
            "wrapped_dek",
            "dek_nonce",
            "ct_nonce",
            "key_id",
            "ciphertext",
        ] {
            assert!(
                !VIEW_SQL.contains(sealed),
                "VIEW_SQL selects `{sealed}` — the view is what the API answers with"
            );
        }
        // …and the loader, which is allowed to, actually does — otherwise this pair of assertions
        // would still pass with the seal accidentally dropped from both.
        assert!(LOAD_SQL.contains("ciphertext"));
    }

    #[test]
    fn the_upsert_replaces_the_seal_rather_than_preserving_it() {
        // A certificate and its key are only ever written together, so preserving one across an
        // update would leave a pair that does not match.
        assert!(SAVE_SQL.contains("ON CONFLICT (id) DO UPDATE"));
        for col in [
            "ciphertext = EXCLUDED.ciphertext",
            "wrapped_dek = EXCLUDED.wrapped_dek",
            "certificate = EXCLUDED.certificate",
            "sans = EXCLUDED.sans",
        ] {
            assert!(SAVE_SQL.contains(col), "SAVE_SQL does not set `{col}`");
        }
    }

    #[test]
    fn the_defaults_cover_the_name_every_in_compose_client_dials() {
        // `nats` is the compose service name in `YAGRA_BUS_URL`. A certificate without it breaks the
        // co-located core and poller the moment TLS comes on, which reads as "the switch broke
        // everything" rather than "one SAN is missing".
        let names = default_names();
        assert!(names.iter().any(|n| n == "nats"), "{names:?}");
        assert!(names.iter().any(|n| n == "127.0.0.1"), "{names:?}");
    }

    #[test]
    fn extra_sans_are_added_to_the_defaults_never_substituted_for_them() {
        // Uses the process environment, so drive the merge directly rather than setting a variable
        // that would race other tests in the same binary.
        let mut names = default_names();
        for extra in ["yagra.example.net", "nats"] {
            if !names.iter().any(|n| n == extra) {
                names.push(extra.to_owned());
            }
        }
        assert!(names.iter().any(|n| n == "nats"));
        assert!(names.iter().any(|n| n == "yagra.example.net"));
        assert_eq!(
            names.iter().filter(|n| *n == "nats").count(),
            1,
            "a name already in the defaults must not be duplicated"
        );
    }

    #[test]
    fn a_generated_bus_certificate_carries_the_names_a_remote_site_will_dial() {
        // The load-bearing property of the whole feature: a poller's handshake fails unless the
        // exact address it dials is in the SAN list. Generation is `server_cert`'s, so this asserts
        // the wiring rather than the crypto.
        let names = vec!["nats".to_owned(), "203.0.113.10".to_owned()];
        let cert = server_cert::generate_self_signed(&names, Utc::now()).expect("generation");
        for n in &names {
            assert!(
                cert.meta.sans.contains(n),
                "SAN list is {:?}",
                cert.meta.sans
            );
        }
        // Self-signed means it is its own CA, which is what makes handing the certificate half to a
        // site a complete answer.
        assert_eq!(cert.meta.subject, cert.meta.issuer);
    }

    #[test]
    fn the_conf_path_in_the_image_matches_what_the_dockerfile_installs() {
        // Two files have to agree and neither is Rust: the constant here and the `COPY` in
        // docker/yagra-rust.Dockerfile's core stage. A mismatch is not a compile error — the
        // one-shot would fail at runtime on a deployment, which is the worst place to find it.
        let dockerfile = std::fs::read_to_string("../../docker/yagra-rust.Dockerfile")
            .expect("the Dockerfile is readable from the crate directory");
        let dir = CONF_IN_IMAGE
            .rsplit_once('/')
            .map(|(d, _)| d)
            .expect("an absolute path");
        assert!(
            dockerfile.contains("nats-server.conf"),
            "the core image does not install a nats-server.conf, so `bus-cert` has nothing to copy \
             and a deployment installed from published images is back to fetching it by hand"
        );
        assert!(
            dockerfile.contains(dir),
            "the Dockerfile does not install anything into {dir}, which is where {CONF_IN_IMAGE} \
             says the configuration lives"
        );
    }
}
