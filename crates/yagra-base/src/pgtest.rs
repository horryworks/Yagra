// SPDX-License-Identifier: AGPL-3.0-only
//! **Tests that run against a real PostgreSQL** — the convention, the fixtures, and the checks
//! that keep the convention honest (ADR-114).
//!
//! ## Why this exists
//!
//! ADR-111, ADR-112 and ADR-113 each stopped at the same sentence: the remaining lines are SQL,
//! and the only way to check SQL is to run it. Cutting more seams does not reach them.
//! `#[sqlx::test]` does: it creates a throwaway database per test, migrates it, and hands the
//! body a [`sqlx::PgPool`].
//!
//! ⚠️ **The number that used to be here has moved, so it is stated as a date.** At ADR-114 it was
//! ~3,950 production lines of untested SQL; ADR-114 took it to 3,566 across ten files, ADR-115 to
//! 2,111 across five, and ADR-116 to **189 lines in one file** — `examples/seed_nodes.rs`, a
//! load-test rig outside `src/`. Every production file holding SQL now has a test or an entry in
//! `guards::SQL_WITHOUT_A_TEST_OF_ITS_OWN` saying why not.
//!
//! 🚨 **"The file has a test" is not "the SQL runs", and the two disagree in both directions.**
//! Measured on 2026-09-01 with `scripts/sql-coverage.sh` — a throwaway server with
//! `log_statement=all`, matched against the literals in the source — the workspace's 474
//! statements went from **143 executed / 242 never executed / 89 unresolvable** to
//! **186 / 199 / 89**. `arp.rs`, `neighbors.rs` and `topology_links.rs` each carried tests and ran
//! **none** of their SQL, while `config_bundle/export.rs` carried none and ran all seventeen of
//! its statements through `api/config_bundle.rs`. The guard answers the cheap question; that
//! script answers the real one, and is deliberately not a gate.
//!
//! Those three ran their SQL for the first time on 2026-09-02, and the third of them found a
//! shipped defect on the first run: all three of `arp.rs`'s address projections read an `inet`
//! with an explicit cast to text, which renders the netmask (`10.0.0.1/32`) and does not parse —
//! so every discovered endpoint listed as `0.0.0.0` and every monitored node was reported as
//! unmonitored. Five tests failed; no source-reading check could have said anything about it.
//!
//! Measured again the same day, after `auth.rs` and half of `meraki.rs`: **240 executed / 145
//! never executed / 90 unresolvable**. `auth.rs` is 29/29. The number moves with the suite, so
//! re-derive it rather than quoting this line — the two commands are in `scripts/sql-coverage.sh`.
//!
//! **Say "untested", never "untestable"** — the obstacle was removed in ADR-114.
//!
//! ## The convention — two attributes, in this order
//!
//! ```ignore
//! #[sqlx::test(migrator = "crate::repo::MIGRATIONS")]
//! #[ignore = "needs DATABASE_URL"]
//! async fn a_fresh_database_starts_empty(pool: sqlx::PgPool) {
//!     let repo = crate::pgtest::repo(pool);
//!     assert_eq!(repo.list_nodes().await.unwrap().len(), 0);
//! }
//! ```
//!
//! * **`migrator` rather than `migrations = "…"`.** The path form expands to a second
//!   `sqlx::migrate!`, and `repo/migrate.rs` exists to keep that macro to one call site — two
//!   sites are two answers to "what does this build embed?" that nothing keeps equal.
//! * **The mark goes second**, because `#[sqlx::test]` is the attribute macro and everything
//!   after it is what the macro receives and re-emits above the `#[test]` it generates.
//!
//! ## Why a mark and not a cargo feature
//!
//! sqlx offers no third option: with no `DATABASE_URL` its harness **panics** rather than
//! skipping (`sqlx-postgres/src/testing/mod.rs` — `dotenvy::var("DATABASE_URL").expect(…)`), so a
//! plain `cargo test` on a machine with no database would fail. A cargo feature would hide these
//! tests from the default build entirely — including from `clippy --all-targets` — and code that
//! is neither compiled nor linted locally rots without anyone seeing it. The mark keeps them
//! compiled and linted always; only the *running* is opt-in, and CI and `scripts/flash-verify.sh`
//! both opt in on every run.
//!
//! ## Running them
//!
//! ```text
//! docker run -d --name yagra-pg -p 5432:5432 -e POSTGRES_PASSWORD=postgres postgres:17-alpine
//! echo 'DATABASE_URL=postgres://postgres:postgres@localhost:5432/postgres' > .env   # gitignored
//! cargo test --workspace -- --include-ignored
//! ```
//!
//! The account must be able to `CREATE DATABASE`; sqlx makes one per test, named from a hash of
//! the test's path, and drops it again **only when the test passed** — a failed test leaves its
//! database behind on purpose, to be inspected. Dropping the `_sqlx_test` schema (and the
//! `_sqlx_test_*` databases it lists) is the tidy-up.

use sqlx::PgPool;
use uuid::Uuid;

/// A [`NodeRepo`](crate::repo::NodeRepo) over the pool the test harness handed us.
///
/// Thin, and deliberately the only wrapper here: every other store already takes a pool
/// (`AlertHistoryStore::new`, `EventRepo::new`, `ReportsRepo::new`, …), so a test builds those
/// directly and there is nothing for this module to add.
#[must_use]
pub fn repo(pool: PgPool) -> crate::repo::NodeRepo {
    crate::repo::NodeRepo::from_pool(pool)
}

/// A folder group, created through the production writer.
///
/// Fixtures here go through the real repository rather than a hand-written `INSERT` on purpose:
/// an insert spelled out in a test is a second copy of the schema that drifts silently, and the
/// table-placement guards (`repo/guards.rs`, `events/guards.rs`) would not see it — they read
/// production text, and this module is test-only.
pub async fn group(pool: &PgPool, name: &str) -> Uuid {
    crate::groups::GroupRepo::new(pool.clone())
        .create(name, crate::groups::GroupType::Site, None, None)
        .await
        .expect("create group")
}

/// A node with an address derived from `n` (`10.0.0.n`), optionally in `group`.
pub async fn node(pool: &PgPool, name: &str, n: u8, group: Option<Uuid>) -> Uuid {
    node_at(pool, name, std::net::IpAddr::from([10, 0, 0, n]), group).await
}

/// A node at a **given** address, optionally in `group`.
///
/// [`node`] delegates here rather than the other way round, so there is one writer. It exists
/// because `10.0.0.n` can only ever be inside one /24 and cannot be v6 at all — which is exactly
/// what the IP-range matching has to be tested against (ADR-124): a longer prefix winning over a
/// shorter one, two folders claiming the same address, and a v4 node not matching a v6 range.
pub async fn node_at(
    pool: &PgPool,
    name: &str,
    addr: std::net::IpAddr,
    group: Option<Uuid>,
) -> Uuid {
    let repo = repo(pool.clone());
    let id = repo
        .create_node(name, addr, None, None, None, None, None, None)
        .await
        .expect("create node");
    if let Some(g) = group {
        repo.set_node_group(id, Some(g)).await.expect("set group");
    }
    id
}

/// An IP range attached to a folder (ADR-100 decision 10, migration 0104).
///
/// ⚠️ **A raw INSERT, unlike every other fixture in this file**, and the exception is worth
/// stating: the production writer is `netbox::NetboxRepo::upsert_prefix`, which takes a NetBox
/// server id, while a hand-made row's `netbox_server_id` is NULL by design — so going through the
/// writer would test a row shape no operator can create. The cast is `network($2::inet)::cidr`
/// for the reason 0104's header gives: a plain `::cidr` REJECTS a value with host bits set, and
/// a test that wants to write `192.168.1.5/24` should get the network, not an error.
pub async fn prefix(pool: &PgPool, group: Uuid, cidr: &str) {
    sqlx::query(
        "INSERT INTO node_group_prefixes (group_id, prefix, description) \
         VALUES ($1, network($2::inet)::cidr, 'fixture')",
    )
    .bind(group)
    .bind(cidr)
    .execute(pool)
    .await
    .expect("seed prefix");
}

/// A NetBox server row, for the tests about **who owns a prefix** (ADR-131 decision 6).
///
/// `node_group_prefixes.netbox_server_id` is the only thing separating a range an operator typed
/// from one a sync maintains, and the two questions worth testing — that a hand-made row survives
/// the stale sweep, and that a sync claiming the same CIDR takes it over — both need a real server
/// row for the foreign key. A sealed credential comes with it because `credential_id` is `NOT NULL
/// REFERENCES credentials (id)`; its contents never matter here.
pub async fn netbox_server(pool: &PgPool, name: &str) -> Uuid {
    let cred = credential(pool, &format!("{name}-token"), "netbox_token").await;
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO netbox_servers (id, name, base_url, credential_id) \
         VALUES ($1, $2, 'https://netbox.example', $3)",
    )
    .bind(id)
    .bind(name)
    .bind(cred)
    .execute(pool)
    .await
    .unwrap_or_else(|e| panic!("seed netbox server {name}: {e}"));
    id
}

/// A device profile, created through the production writer.
///
/// For the tables keyed by `REFERENCES profiles (id)` — `profile_collection_templates` is the one
/// this was written for, where a hand-rolled uuid is refused by the foreign key rather than by
/// anything a test would recognise.
///
/// `generic-snmp` is the column's own default and the category an operator-created profile gets,
/// so a test that does not care about the category is not silently exercising an unusual one.
pub async fn profile(pool: &PgPool, name: &str) -> Uuid {
    repo(pool.clone())
        .create_profile(name, "generic-snmp", None, None)
        .await
        .unwrap_or_else(|e| panic!("create profile {name}: {e}"))
}

/// Rows in `table`. A `count(*)` spelled once rather than in every test that needs one.
pub async fn rows(pool: &PgPool, table: &str) -> i64 {
    // The name is interpolated because a table name cannot be a bind parameter. Every caller is
    // a literal in this crate's own tests, so there is no input to sanitise.
    sqlx::query_scalar::<_, i64>(&format!("SELECT count(*) FROM {table}"))
        .fetch_one(pool)
        .await
        .unwrap_or_else(|e| panic!("count {table}: {e}"))
}

/// The stand-in key-encryption key every sealing store in these tests shares.
///
/// A fixed in-memory key, the same one `api::tests_support` uses and for the same reason: the
/// sealed value round-trips inside one test and nowhere else, so nothing here says anything about
/// a real deployment's KEK handling. Two stores now take one ([`CredentialStore`] and
/// `alerts::notifications::NotificationRepo`), which is why it is spelled once.
///
/// [`CredentialStore`]: crate::secrets::CredentialStore
#[must_use]
pub fn kek() -> crate::secrets::Kek {
    std::sync::Arc::new(yagra_secrets::StaticKeyProvider::single([7u8; 32]))
}

/// A sealed credential, created through the production writer.
///
/// For the tables that carry a `REFERENCES credentials (id)` foreign key — `meraki_orgs`, `nodes`,
/// `url_checks` — where a hand-written `INSERT` would be a second copy of the envelope-encryption
/// columns as well as of the schema.
///
/// Sealed with [`kek`], which says what that does and does not prove.
pub async fn credential(pool: &PgPool, name: &str, kind: &str) -> Uuid {
    crate::secrets::CredentialStore::new(pool.clone(), kek())
        .create(name, kind, b"a-test-secret")
        .await
        .unwrap_or_else(|e| panic!("create credential {name}: {e}"))
}

/// One `TIMESTAMPTZ` column of the row a node owns (`WHERE node_id = …`).
///
/// The common case of [`timestamp_of`], spelled out so the majority of callers cannot get the key
/// column's name wrong.
pub async fn node_timestamp(
    pool: &PgPool,
    table: &str,
    column: &str,
    node: Uuid,
) -> chrono::DateTime<chrono::Utc> {
    timestamp_of(pool, table, column, "node_id", node).await
}

/// One `TIMESTAMPTZ` column of one row, found by a uuid key.
///
/// For the columns a repository *writes* and exposes no reader for. `node_arp.first_seen` is the
/// case this was written for: the ARP store keeps "this port has looked like this for three weeks"
/// and nothing in production ever selects it, so without this the rule that an unchanged walk must
/// not restart the clock is assertable only as text — which cannot tell a `CASE` that works from
/// one that is spelled right and evaluates the wrong way. (`meraki_orgs.last_sync_at` was the
/// second such column until ADR-164 gave it a reader, `MerakiOrg::last_sync_at`.)
///
/// Deliberately narrow rather than a general "run this statement": a test that can spell any SQL
/// becomes a second copy of the schema, which is what the fixtures above exist to avoid.
///
/// ⚠️ Three of the five arguments are names, so the order matters and nothing checks it: it is
/// `(table, column, key_column, key)`. Prefer [`node_timestamp`] where it fits.
pub async fn timestamp_of(
    pool: &PgPool,
    table: &str,
    column: &str,
    key_column: &str,
    key: Uuid,
) -> chrono::DateTime<chrono::Utc> {
    // Interpolated for the same reason `rows` interpolates: neither a table nor a column name can
    // be a bind parameter. Every caller is a literal in this crate's own tests, and the key — the
    // only value that comes from anywhere — is bound.
    sqlx::query_scalar(&format!(
        "SELECT {column} FROM {table} WHERE {key_column} = $1"
    ))
    .bind(key)
    .fetch_one(pool)
    .await
    .unwrap_or_else(|e| panic!("{table}.{column}: {e}"))
}
