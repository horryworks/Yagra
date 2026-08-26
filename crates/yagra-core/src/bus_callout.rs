// SPDX-License-Identifier: AGPL-3.0-only
//! Storage and materialization for the **Auth Callout account key** (ADR-065 Inc.7).
//!
//! [`crate::bus_cert`] does this job for the certificate the bus serves; this module does it for the
//! key core signs each poller's scoped credential with. The two are deliberately the same shape —
//! **the row is the record, the file is a materialization of it** — and are written by the same
//! one-shot, from the same image, in the same run.
//!
//! ## Why the shape matters more here than it did for the certificate
//!
//! ADR-030 shipped this key as a file the operator mounts (`YAGRA_NATS_CALLOUT_SEED_FILE`) and the
//! feature has never once run in production. Not because the key was hard to make — `nk -gen
//! account -pubout` is one command — but because the *other* half of the procedure cannot survive:
//! it says to uncomment the `auth_callout` block in `nats-server.conf`, and `bus-cert-init` rewrites
//! that file from the image on every `up` (ADR-065 Inc.2 decision 3). The documented path is erased
//! by the next thing the product does.
//!
//! Measured consequence, 2026-08-26: the site bundle writes `YAGRA_BUS_URL=tls://<id>:<token>@…`
//! and explains in its own `.env` that "the central Auth Callout scopes this connection's
//! permissions" on that id — and the only thing that can validate such a token is the callout the
//! product does not enable. A remote poller was refused with
//! `authentication error - User "yagra-poller2a"`. The artefact declared a precondition nothing
//! created.
//!
//! ## Why the issuer is written into its own file
//!
//! `callout.conf` holds the `auth_callout` block with the issuer as a **literal**, and
//! `nats-server.conf` includes it. Routing the issuer through `.env` instead would be one variable
//! two files have to agree about across an upgrade — and the failure mode is not subtle: the
//! composition's default is `${YAGRA_NATS_CALLOUT_ISSUER:-}`, which is *set to empty* rather than
//! unset, so nats-server would parse an empty issuer and refuse to start. That takes the bus down,
//! which takes monitoring down. Writing both files from one run of one binary removes the question.

use std::path::PathBuf;

use sqlx::{PgPool, Row};
use yagra_secrets::{EnvelopeCipher, SealedSecret};

use crate::bus_cert::write_atomically;
use crate::secrets::Kek;

/// What `nats-server.conf` includes. Sits beside it on the bus volume, one level above `certs/`.
pub const CONF_FILE: &str = "callout.conf";

/// The static accounts that skip the callout, in the order the generated file lists them.
///
/// 🚨 `poller` is here and its absence is a silent outage. The co-located poller in
/// `docker-compose.deploy.yml` dials `tls://poller:<password>@nats:4222`, so the name it presents is
/// the literal `poller` — not its `YAGRA_POLLER_ID`. Put it through the callout and
/// `PollerRepo::auth_material("poller")` finds no ledger row, the request is denied as an unknown
/// poller, and the deployment stops polling everything in its own pool while the WebUI reports the
/// switch as having succeeded.
///
/// It is not a widening of the boundary it looks like: both names authenticate with a password that
/// exists only in this deployment's `.env`, on the internal Docker network, and **every** connection
/// from outside the composition presents an id instead and therefore goes through the callout.
pub const BYPASS_USERS: [&str; 2] = ["core", "poller"];

/// The account key, and the file the bus reads its issuer from.
pub struct BusCalloutRepo {
    pool: PgPool,
    cipher: EnvelopeCipher<Kek>,
    /// `YAGRA_BUS_TLS_DIR` — the shared bus volume. `None` on a deployment with nowhere to write,
    /// where the row is still kept: core signs from the row, only the server needs the file.
    dir: Option<PathBuf>,
}

/// What the one-shot has to know after establishing the key, without holding the secret half.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CalloutIdentity {
    /// The account public key (`A…`) NATS is told to trust.
    pub issuer: String,
    /// The NATS account minted users join.
    pub account: String,
}

impl BusCalloutRepo {
    #[must_use]
    pub fn new(pool: PgPool, kek: Kek, dir: Option<PathBuf>) -> Self {
        Self {
            pool,
            cipher: EnvelopeCipher::new(kek),
            dir,
        }
    }

    /// The account seed, decrypted, or `None` when no key has been established yet.
    ///
    /// # Errors
    /// A database failure. A row whose seal will not open is **not** an error here: it is reported
    /// as `None` with an `error!`, so a deployment whose KEK was substituted degrades to "no
    /// per-poller scoping" instead of refusing to start. That is the same call `authcallout::start`
    /// already makes for an unreadable seed file, and for the same reason — this feature is not
    /// worth taking a monitoring system down over.
    pub async fn load_seed(&self) -> anyhow::Result<Option<String>> {
        let Some(row) = sqlx::query(LOAD_SQL).fetch_optional(&self.pool).await? else {
            return Ok(None);
        };
        let sealed = SealedSecret {
            key_id: u32::try_from(row.get::<i32, _>("key_id")).unwrap_or(0),
            wrapped_dek: row.get("wrapped_dek"),
            dek_nonce: row.get("dek_nonce"),
            ciphertext: row.get("ciphertext"),
            ct_nonce: row.get("ct_nonce"),
        };
        let Ok(bytes) = self.cipher.open(&sealed) else {
            tracing::error!(
                "the stored Auth Callout account key will not open with this KEK; per-poller \
                 credential scoping is disabled"
            );
            return Ok(None);
        };
        match String::from_utf8(bytes) {
            Ok(seed) => Ok(Some(seed)),
            Err(_) => {
                tracing::error!("the stored Auth Callout account key is not valid UTF-8");
                Ok(None)
            }
        }
    }

    /// The issuer and account, without opening the seal. `None` before anything is generated.
    ///
    /// # Errors
    /// A database failure.
    pub async fn identity(&self) -> anyhow::Result<Option<CalloutIdentity>> {
        let Some(row) = sqlx::query(IDENTITY_SQL).fetch_optional(&self.pool).await? else {
            return Ok(None);
        };
        Ok(Some(CalloutIdentity {
            issuer: row.get("issuer"),
            account: row.get("account"),
        }))
    }

    /// Establish the key if there is none, and return what the server has to be told.
    ///
    /// Idempotent, which is what makes running it on every `up` correct rather than wasteful — and
    /// it is **generate-once**, unlike [`crate::bus_cert::BusTlsRepo::ensure_ready`], which renews
    /// an expired certificate. A new account key invalidates nothing a site holds (poller tokens
    /// live in the ledger, not in the JWTs), but every established connection's credential was
    /// signed by a key the server would no longer trust, so rotation is an operator action with a
    /// visible consequence rather than a background chore. There is deliberately no expiry.
    ///
    /// # Errors
    /// A database failure, or the platform RNG refusing to produce a key.
    pub async fn ensure_ready(&self, account: &str) -> anyhow::Result<CalloutIdentity> {
        if let Some(existing) = self.identity().await? {
            return Ok(existing);
        }
        let seed = yagra_authz::new_account_seed()
            .map_err(|e| anyhow::anyhow!("generate the Auth Callout account key: {e}"))?;
        // Through the signer rather than nkeys directly: the issuer stored here must be the one the
        // signer will later put in `iss`, and deriving it twice is how those two come to disagree.
        let signer = yagra_authz::AccountSigner::from_seed(&seed, account.to_owned())
            .map_err(|e| anyhow::anyhow!("the generated account key does not load: {e}"))?;
        let issuer = signer.issuer_public_key().to_owned();
        let sealed = self
            .cipher
            .seal(seed.as_bytes())
            .map_err(|e| anyhow::anyhow!("seal the Auth Callout account key: {e}"))?;
        // ON CONFLICT DO NOTHING, not an upsert: two cores racing on a fresh deployment must end up
        // with ONE key. An upsert would let the loser overwrite the winner's, and the server would
        // then be told an issuer that the core answering callouts does not sign with.
        sqlx::query(SAVE_SQL)
            .bind(i32::try_from(sealed.key_id).unwrap_or(0))
            .bind(&sealed.wrapped_dek)
            .bind(&sealed.dek_nonce)
            .bind(&sealed.ciphertext)
            .bind(&sealed.ct_nonce)
            .bind(&issuer)
            .bind(account)
            .execute(&self.pool)
            .await?;
        // Re-read rather than returning what was just built: on a race the insert did nothing and
        // the row belongs to the other core.
        let stored = self
            .identity()
            .await?
            .ok_or_else(|| anyhow::anyhow!("the Auth Callout account key vanished after insert"))?;
        if stored.issuer == issuer {
            tracing::info!(issuer = %stored.issuer, account = %stored.account,
                "established the Auth Callout account key (ADR-065 Inc.7)");
        } else {
            tracing::info!(issuer = %stored.issuer,
                "another core established the Auth Callout account key first; using it");
        }
        Ok(stored)
    }

    /// Write `callout.conf`. A no-op with no directory configured.
    ///
    /// Overwritten unconditionally on every run, exactly as `nats-server.conf` is and for the same
    /// reason: the two are one configuration split across two files, and a stale half is a bus that
    /// trusts a key nobody signs with.
    ///
    /// # Errors
    /// The io error, named with the path — this runs before anything else can report a failure.
    pub fn install_conf(&self, id: &CalloutIdentity) -> anyhow::Result<()> {
        let Some(dir) = self.dir.as_ref() else {
            return Ok(());
        };
        std::fs::create_dir_all(dir)?;
        let dst = dir.join(CONF_FILE);
        write_atomically(&dst, render_conf(id).as_bytes(), 0o644)
            .map_err(|e| anyhow::anyhow!("write {} : {e}", dst.display()))?;
        tracing::info!(path = %dst.display(), issuer = %id.issuer,
            "installed the Auth Callout configuration");
        Ok(())
    }
}

/// The `auth_callout` block, with the issuer as a literal.
///
/// Its own function so a test can read the text without a database or a volume — the bytes here are
/// the whole interface between core and the broker, and a typo in them is a bus that will not start.
#[must_use]
pub fn render_conf(id: &CalloutIdentity) -> String {
    let bypass = BYPASS_USERS.join(", ");
    let mut out = String::new();
    for line in [
        "# Yagra — Auth Callout (ADR-030, wired up by ADR-065 Inc.7).",
        "#",
        "# GENERATED. Rewritten from the database by `yagra-core bus-cert` on every `up`, so an edit",
        "# here lasts until the next one. Included by nats-server.conf, which is only read when the",
        "# bus runs with `-c` — what Settings > Pollers > \"Accept remote pollers\" arranges.",
        "#",
        "# `issuer` is the account PUBLIC key whose seed core holds sealed in `bus_callout_config`.",
        "# Core mints each connecting poller a JWT scoped to that poller's own subjects, so a",
        "# compromised site cannot read another site's device credentials off the bus (ADR-020).",
        "#",
        "# `auth_users` are the names that skip the callout and authenticate against the static",
        "# accounts in nats-server.conf instead: this composition's own core and its co-located",
        "# poller, both presenting a fixed name and a password from this deployment's `.env`.",
        "# Anything arriving from outside presents a poller id, matches neither, and is authorized",
        "# by core or not at all.",
    ] {
        out.push_str(line);
        out.push('\n');
    }
    out.push_str("auth_callout {\n");
    out.push_str(&format!("  issuer: {}\n", id.issuer));
    out.push_str(&format!("  auth_users: [ {bypass} ]\n"));
    out.push_str(&format!("  account: \"{}\"\n", id.account));
    out.push_str("}\n");
    out
}

const IDENTITY_SQL: &str = "SELECT issuer, account FROM bus_callout_config WHERE id = 1";

/// Everything [`BusCalloutRepo::load_seed`] needs. Kept apart from [`IDENTITY_SQL`] on purpose: the
/// identity is what gets written into a world-readable file, and a reader that selects the seal by
/// accident should have to say so.
const LOAD_SQL: &str = "SELECT key_id, wrapped_dek, dek_nonce, ciphertext, ct_nonce \
                        FROM bus_callout_config WHERE id = 1";

const SAVE_SQL: &str = "INSERT INTO bus_callout_config \
                        (id, key_id, wrapped_dek, dek_nonce, ciphertext, ct_nonce, issuer, account) \
                        VALUES (1, $1, $2, $3, $4, $5, $6, $7) ON CONFLICT (id) DO NOTHING";

#[cfg(test)]
mod tests {
    use super::*;

    /// A **real** account key, not a plausible-looking string.
    ///
    /// 🚨 The first version of this fixture was 53 hand-typed base32 characters starting with `A`,
    /// and every test here passed on it. A real `nats-server` parsing the same file rejected it in
    /// one line: `Expected callout user to be a valid public account nkey`. An nkey carries a
    /// prefix byte and a CRC-16, so "looks like one" and "is one" are different questions and only
    /// the second one matters to the broker. Generating it through the same function production
    /// uses means the fixture cannot be narrower than the thing it stands for.
    fn identity() -> CalloutIdentity {
        let seed = yagra_authz::new_account_seed().expect("generate an account key");
        let signer =
            yagra_authz::AccountSigner::from_seed(&seed, "$G".to_owned()).expect("load the key");
        CalloutIdentity {
            issuer: signer.issuer_public_key().to_owned(),
            account: "$G".to_owned(),
        }
    }

    /// What the broker checks before it will start: the issuer is an **account** public key.
    ///
    /// Cheap here and expensive everywhere else — the alternative place to find this out is a bus
    /// that will not come up, at the moment an operator turned remote acceptance on.
    #[test]
    fn the_issuer_is_the_shape_nats_server_demands() {
        let id = identity();
        assert!(
            id.issuer.starts_with('A'),
            "an account public key starts with `A` (a user key starts with `U`, and NATS rejects \
             one here): {}",
            id.issuer
        );
        assert_eq!(id.issuer.len(), 56, "nkey public keys are 56 characters");
        assert!(
            id.issuer
                .chars()
                .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit()),
            "base32 only: {}",
            id.issuer
        );
    }

    /// The three things nats-server has to find, in the form it accepts.
    #[test]
    fn the_generated_block_names_the_issuer_literally() {
        let id = identity();
        let text = render_conf(&id);
        assert!(text.contains(&format!("issuer: {}", id.issuer)), "{text}");
        // 🚨 Not `$YAGRA_NATS_CALLOUT_ISSUER`. The composition defaults that variable to the EMPTY
        // string rather than leaving it unset, so a reference here would have nats-server parse an
        // empty issuer and refuse to start — taking the bus, and therefore monitoring, down.
        assert!(
            !text.contains("$YAGRA"),
            "the issuer must be a literal, not an environment reference: {text}"
        );
        assert!(text.contains("auth_callout {"), "{text}");
        assert!(text.contains("account: \"$G\""), "{text}");
    }

    /// 🚨 The co-located poller must be in the bypass list, and this is the assertion that says why.
    #[test]
    fn the_co_located_poller_skips_the_callout_along_with_core() {
        let text = render_conf(&identity());
        assert!(
            text.contains("auth_users: [ core, poller ]"),
            "the co-located poller presents the literal name `poller`, has no ledger row under that \
             name, and would be denied as an unknown poller — stopping this deployment from polling \
             its own pool while the switch reports success. Offending render:\n{text}"
        );
        assert_eq!(BYPASS_USERS.len(), 2, "the bypass list is closed at two");
    }

    /// A generated file that does not say it is generated gets edited.
    #[test]
    fn the_file_says_it_is_rewritten_on_every_up() {
        let text = render_conf(&identity());
        assert!(text.contains("GENERATED"), "{text}");
        assert!(text.starts_with("# "), "{text}");
    }

    /// The seal is never selected by the statement that feeds a world-readable file.
    #[test]
    fn the_identity_query_cannot_return_the_account_key() {
        for sealed in [
            "wrapped_dek",
            "dek_nonce",
            "ciphertext",
            "ct_nonce",
            "key_id",
        ] {
            assert!(
                !IDENTITY_SQL.contains(sealed),
                "IDENTITY_SQL selects `{sealed}` — its result is written into callout.conf"
            );
        }
        // And the other direction, so the pair cannot pass by both losing the seal.
        assert!(LOAD_SQL.contains("wrapped_dek") && LOAD_SQL.contains("ct_nonce"));
    }

    /// Two cores starting together must not end up disagreeing about the issuer.
    #[test]
    fn the_insert_refuses_to_replace_an_existing_key() {
        assert!(
            SAVE_SQL.contains("ON CONFLICT (id) DO NOTHING"),
            "an upsert would let a losing core overwrite the key the server was told to trust"
        );
        assert!(!SAVE_SQL.to_lowercase().contains("do update"), "{SAVE_SQL}");
    }

    fn shipped(path: &str) -> String {
        std::fs::read_to_string(format!("../../{path}"))
            .unwrap_or_else(|e| panic!("{path} ships with the product: {e}"))
    }

    /// The broker reads the file this module writes — under the name it writes it as.
    ///
    /// Two artefacts, two languages, one name, and nothing else compares them: rename [`CONF_FILE`]
    /// and nats-server would exit on a missing include, taking the bus and therefore monitoring
    /// down, at the moment an operator turned remote acceptance on.
    #[test]
    fn the_shipped_server_config_includes_the_file_this_module_writes() {
        let conf = shipped("docker/nats/nats-server.conf");
        let include = format!("include \"{CONF_FILE}\"");
        // ⚠️ By LINE POSITION, not by splitting on the needle: this file explains itself, so the
        // include's own text appears in the prose above it and a `split` would cut there and then
        // measure the header instead of the statement.
        let lines: Vec<&str> = conf.lines().collect();
        let at = lines
            .iter()
            .position(|l| l.trim() == include)
            .unwrap_or_else(|| {
                panic!(
                    "docker/nats/nats-server.conf has no `{include}` statement — per-poller scoping \
                     would be off on every deployment, silently, exactly as it was before Inc.7"
                )
            });
        // Inside `authorization {}`, where nats-server accepts an `auth_callout` block. An include
        // at the top level parses and then does nothing at all.
        assert!(
            lines[..at]
                .iter()
                .any(|l| !l.trim_start().starts_with('#') && l.contains("authorization {")),
            "the include sits outside `authorization {{}}`, where the block it carries is ignored"
        );
    }

    /// 🚨 The issuer must never travel through the environment again.
    ///
    /// It did until Inc.7, and the failure was not the one it looks like: the composition defaulted
    /// `YAGRA_NATS_CALLOUT_ISSUER` to the **empty string** rather than leaving it unset, so
    /// nats-server's fail-closed behaviour on a referenced-and-unset variable never triggered. A
    /// deployment that upgraded into an enabled `auth_callout` block would have handed it an empty
    /// issuer to parse and lost the bus.
    #[test]
    fn no_shipped_artefact_routes_the_issuer_through_the_environment() {
        for path in [
            "docker/nats/nats-server.conf",
            "docker-compose.deploy.yml",
            "docker-compose.yml",
        ] {
            for line in shipped(path).lines() {
                let code = line.split('#').next().unwrap_or_default();
                assert!(
                    !code.contains("YAGRA_NATS_CALLOUT_ISSUER"),
                    "{path} names YAGRA_NATS_CALLOUT_ISSUER in a live line: {line}"
                );
            }
        }
    }

    /// The account is named on both sides of the broker, so both sides must be told the same thing.
    ///
    /// `bus-cert-init` writes it into `callout.conf`; `core` signs with it. They disagree ⇒ every
    /// remote poller is refused, with nothing in either log to say why.
    ///
    /// ⚠️ `$$G`, not `$G`. Measured, because `docker compose config` cannot answer this — it prints
    /// a literal `$` back as `$$` for round-tripping either way. A container was run:
    /// `${VAR:-$$G}` delivers `$G`, and `${VAR:-$G}` delivers the **empty string**, because Compose
    /// substitutes the default too. The line this replaced was the second spelling, commented out.
    #[test]
    fn both_sides_of_the_broker_are_told_the_same_account() {
        let compose = shipped("docker-compose.deploy.yml");
        let live: Vec<String> = compose
            .lines()
            .map(str::trim)
            .filter(|l| !l.starts_with('#') && l.starts_with("YAGRA_NATS_CALLOUT_ACCOUNT:"))
            .map(str::to_owned)
            .collect();
        assert_eq!(
            live.len(),
            2,
            "expected the account on exactly two services — `bus-cert-init`, which writes it into \
             callout.conf, and `core`, which signs with it. Found: {live:?}"
        );
        assert_eq!(
            live[0], live[1],
            "the two services are given different expressions"
        );
        assert!(
            live[0].contains("$$G"),
            "a single `$` makes Compose substitute the default as an undefined variable, so the \
             account arrives EMPTY: {}",
            live[0]
        );
    }

    /// The one variable that decides whether any of this runs has to reach core.
    ///
    /// 🚨 It did not, for the entire life of ADR-030: `YAGRA_NATS_POLLER_PASSWORD` was commented out
    /// in the `core` service, so `authcallout::start` returned immediately on every deployment there
    /// has ever been and the feature was never once exercised in production. A comment character is
    /// all it takes, and nothing else in the build would notice.
    #[test]
    fn the_core_service_is_given_the_secret_the_callout_validates() {
        let compose = shipped("docker-compose.deploy.yml");
        let live: Vec<&str> = compose
            .lines()
            .filter(|l| !l.trim_start().starts_with('#'))
            .filter(|l| l.contains("YAGRA_NATS_POLLER_PASSWORD:"))
            .collect();
        assert_eq!(
            live.len(),
            2,
            "expected `YAGRA_NATS_POLLER_PASSWORD:` on exactly two services — `nats`, which \
             substitutes it into the static account, and `core`, which validates a connecting \
             poller's bootstrap secret against it. Found: {live:?}"
        );
    }
}
