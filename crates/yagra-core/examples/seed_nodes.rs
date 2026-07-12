//! Synthetic node seeder — the S2/S6 control-plane scale harness.
//!
//! Bulk-inserts N synthetic nodes straight into Postgres so the core's scheduler sweep (spec
//! resolution, S2 / ADR-026) and alert-config rebuild (S6) can be exercised at fleet scale, and so
//! `result_firehose` (run with `FIREHOSE_SEEDED=1`) can aim its results at real, FK-valid node rows
//! (needed for the interface-upsert path). It talks to Postgres directly — `yagra-core` is a bin-only
//! crate with no library target, so an example cannot reuse `NodeRepo`; raw `sqlx` keeps the tool
//! self-contained.
//!
//! ## Two interactions that decide what you actually measure
//!
//! 1. **The config-generation cache makes a direct DB insert invisible** (ADR-026). The running core
//!    only rescans `nodes` on a sweep *cache miss*; a raw INSERT does not bump `config_gen`, so the
//!    sweep keeps hitting its cache and never notices the new rows. **After seeding, force a
//!    re-resolution**: `docker restart yagra-core-1` (cleanest — the first sweep is a miss over the
//!    whole fleet) or make any config change through the API (the audit middleware bumps the gen).
//!
//! 2. **A pool with no live poller runs "legacy" mode, which disables the sweep cache fleet-wide.**
//!    The default `SEED_POOL=firehose` has no registered poller, so every sweep becomes a cache miss
//!    that does the full `list_nodes` scan + per-node spec build — i.e. it measures the **uncached
//!    worst case** (does a 50k resolution finish inside `min_interval`?). Legacy jobs publish to
//!    `yagra.jobs.firehose`, which has no subscriber, so plain NATS drops them — no real poller is
//!    flooded. Seed into `SEED_POOL=default` instead to land the nodes on the live poller (tests the
//!    cached steady state, but then the real poller will try to poll 50k unreachable addresses).
//!
//! ## Measuring the S2 acceptance criterion
//!
//! There is no sweep-duration gauge yet (`record_sweep` stores only the last-sweep timestamp), so
//! infer the resolution time from the miss-counter cadence: with `SEED_POOL=firehose` every sweep is
//! a miss, and `yagra_sweep_cache_misses_total` increments once per completed sweep. The period
//! between increments is `resolution_time + min_interval`; if it stays ≈ `min_interval` the 50k
//! resolution is cheap, if it stretches well past it the sweep is overrunning. Watch
//! `yagra_sweep_cache_hits_total` climb again after teardown to confirm the fleet returns to cached.
//!
//! ## Run (inside the compose network so the `postgres` host resolves)
//!   # seed 50k nodes into the isolated `firehose` pool
//!   docker compose exec -e SEED_NODES=50000 yagra-core /usr/local/bin/... # (no example in image)
//!   # more practically, from a checkout with DB reachable:
//!   YAGRA_DATABASE_URL=postgres://yagra:yagra@localhost:5432/yagra \
//!     cargo run --release --example seed_nodes
//!   # tear the synthetic nodes back out (prefix-scoped; FK children cascade / set-null):
//!   SEED_MODE=teardown cargo run --release --example seed_nodes
//!
//! Env knobs (all optional):
//!   YAGRA_DATABASE_URL   Postgres url          (default postgres://yagra:yagra@localhost:5432/yagra)
//!   SEED_MODE            seed | teardown        (default seed)
//!   SEED_NODES           how many to insert     (default 50000)
//!   SEED_PREFIX          node-name prefix       (default "firehose-")   ← teardown matches this
//!   SEED_POOL            poll pool              (default "firehose")     ← see interaction (2)
//!   SEED_PROFILE         profile UUID to attach (default none → liveness-only, no collection/creds)

use std::net::Ipv4Addr;
use std::time::Instant;

use sqlx::postgres::PgPoolOptions;
use sqlx::Row;
use uuid::Uuid;

/// Deterministic node-id scheme, **shared verbatim with `result_firehose.rs`** (its `FIREHOSE_SEEDED`
/// mode derives the same ids). High 64 bits are a fixed namespace tag; the low 64 bits carry the
/// index, so ids are unique for any `i < 2^64` and reproducible without an RNG. If you change this,
/// change it in both files.
const SEED_TAG_HI: u64 = 0xF17E_0000_5EED_0000;

fn seeded_node_uuid(i: u64) -> Uuid {
    Uuid::from_u128(((SEED_TAG_HI as u128) << 64) | i as u128)
}

/// A large, deterministic synthetic address so 50k rows don't collide (10.0.0.0/8 space).
fn seeded_addr(i: u64) -> Ipv4Addr {
    Ipv4Addr::new(10, (i >> 16) as u8, (i >> 8) as u8, i as u8)
}

fn env_string(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_owned())
}

fn env_u64(key: &str, default: u64) -> u64 {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let db_url = env_string(
        "YAGRA_DATABASE_URL",
        "postgres://yagra:yagra@localhost:5432/yagra",
    );
    let mode = env_string("SEED_MODE", "seed");
    let prefix = env_string("SEED_PREFIX", "firehose-");
    let pool_name = env_string("SEED_POOL", "firehose");
    let profile: Option<Uuid> = std::env::var("SEED_PROFILE")
        .ok()
        .and_then(|v| Uuid::parse_str(v.trim()).ok());

    let pool = PgPoolOptions::new()
        .max_connections(8)
        .connect(&db_url)
        .await?;

    if mode == "teardown" {
        // Prefix-scoped delete. Every FK referencing nodes(id) is ON DELETE CASCADE or SET NULL, so
        // interface rows / node-scope collection items / current-state cascade out and history rows
        // detach — no manual child cleanup needed.
        let like = format!("{prefix}%");
        let res = sqlx::query("DELETE FROM nodes WHERE name LIKE $1")
            .bind(&like)
            .execute(&pool)
            .await?;
        eprintln!(
            "teardown: deleted {} synthetic nodes matching name LIKE '{like}'",
            res.rows_affected()
        );
        return Ok(());
    }

    let count = env_u64("SEED_NODES", 50_000).max(1);
    eprintln!(
        "seed → {db_url}: {count} nodes, prefix '{prefix}', pool '{pool_name}', profile {profile:?}"
    );

    // Chunked multi-row INSERT: 5 params/row, so 5,000 rows/chunk stays well under the 65,535 bind
    // limit. ON CONFLICT (id) DO NOTHING makes re-seeding idempotent (ids are deterministic).
    const CHUNK: u64 = 5_000;
    let start = Instant::now();
    let mut inserted: u64 = 0;
    let mut i: u64 = 0;
    while i < count {
        let end = (i + CHUNK).min(count);
        let rows = (end - i) as usize;

        let mut sql =
            String::from("INSERT INTO nodes (id, name, address, pool, profile_id) VALUES ");
        for r in 0..rows {
            let b = r * 5;
            if r > 0 {
                sql.push(',');
            }
            // address is INET; profile_id is a nullable uuid.
            sql.push_str(&format!(
                "(${},${},${}::inet,${},${})",
                b + 1,
                b + 2,
                b + 3,
                b + 4,
                b + 5
            ));
        }
        sql.push_str(" ON CONFLICT (id) DO NOTHING");

        let mut q = sqlx::query(&sql);
        for j in i..end {
            q = q
                .bind(seeded_node_uuid(j))
                .bind(format!("{prefix}{j}"))
                .bind(seeded_addr(j).to_string())
                .bind(&pool_name)
                .bind(profile);
        }
        let res = q.execute(&pool).await?;
        inserted += res.rows_affected();

        i = end;
        if i.is_multiple_of(50_000) || i == count {
            eprintln!(
                "  {i}/{count} processed ({inserted} newly inserted, {:.1}s)",
                start.elapsed().as_secs_f64()
            );
        }
    }

    // Confirm the fleet count so the operator can sanity-check before forcing a re-resolution.
    let total: i64 = sqlx::query("SELECT count(*) AS c FROM nodes WHERE name LIKE $1")
        .bind(format!("{prefix}%"))
        .fetch_one(&pool)
        .await?
        .try_get("c")?;
    eprintln!(
        "done: {inserted} inserted this run, {total} synthetic nodes total in {:.1}s.\n\
         NEXT: force the core to see them — `docker restart yagra-core-1` (or an API config write) —\n\
         then watch yagra_sweep_cache_misses_total cadence vs min_interval.",
        start.elapsed().as_secs_f64()
    );
    Ok(())
}
