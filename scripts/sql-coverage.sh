#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-only
#
# How much of the workspace's raw SQL does the test suite actually execute? (ADR-116 decision 5)
#
# Run from the repo root, inside WSL, AFTER scripts/flash-build.sh has materialised the source and
# the builder image — it reuses that script's warm target volume, exactly like flash-verify.sh.
#
# 🚨 **The instrument is the SERVER's own log, not client tracing.** `RUST_LOG=sqlx::query=debug`
# produces nothing here: the test binaries install no `tracing` subscriber, so sqlx's own logging
# has nowhere to go. `log_statement=all` on the throwaway PostgreSQL records every statement the
# suite executes regardless of what the client does with it.
#
# This is NOT a CI gate and is not meant to become one. Standing up a server costs more than a
# check earns, and the answer is incomplete by construction (see the `unknown` bucket below), so it
# would gate on a number that cannot reach zero. What it is for is making the number in ADR-116
# re-derivable by anyone, instead of a claim written down once.
#
# Two steps, because there is no node inside WSL: this script writes the server log, then you run
# `node scripts/sql-coverage.js <log>` on the host to map it back to source files.
set -euo pipefail

PROFILE=${PROFILE:-ci-fast}
BUILDER=${BUILDER:-yagra-builder:1.90}
SRC=${SRC:-/var/tmp/yagra-flash-src}
OUT=${OUT:-/var/tmp/yagra-sql-coverage}

[ -d "$SRC" ] || { echo "sql-coverage: $SRC missing — run scripts/flash-build.sh first" >&2; exit 1; }
docker image inspect "$BUILDER" >/dev/null 2>&1 \
  || { echo "sql-coverage: $BUILDER missing — run scripts/flash-build.sh first" >&2; exit 1; }

PG=${PG:-yagra-sqlcov-pg}
NET=${NET:-yagra-sqlcov-net}
mkdir -p "$OUT"

cleanup() {
  docker rm -f "$PG" >/dev/null 2>&1 || true
  docker network rm "$NET" >/dev/null 2>&1 || true
}
trap cleanup EXIT
cleanup

docker network create "$NET" >/dev/null
# log_min_duration_statement=-1 keeps the duration lines out; log_statement=all is the whole point.
docker run -d --name "$PG" --network "$NET" -e POSTGRES_PASSWORD=postgres \
  postgres:17-alpine -c log_statement=all -c log_min_duration_statement=-1 >/dev/null

ready=""
for _ in $(seq 1 60); do
  if docker exec "$PG" pg_isready -q 2>/dev/null; then ready=1; break; fi
  sleep 1
done
[ -n "$ready" ] || { echo "sql-coverage: $PG never became ready" >&2; docker logs "$PG" >&2; exit 1; }

# The database tests carry `#[ignore]`, and `--all-targets` is what keeps `--include-ignored` from
# also compiling the ```ignore doc examples (see flash-verify.sh). Both halves matter here: a run
# without the database tests would report most of `repo/` as never executed.
docker run --rm --network "$NET" \
  -e DATABASE_URL="postgres://postgres:postgres@$PG:5432/postgres" \
  -v "$SRC":/app \
  -v yagra-flash-target:/app/target \
  -v yagra-flash-registry:/usr/local/cargo/registry \
  -v yagra-flash-git:/usr/local/cargo/git \
  -w /app "$BUILDER" bash -c \
  "cargo test --workspace --all-targets --profile $PROFILE -- --include-ignored" \
  > "$OUT/testrun.log" 2>&1 || { echo "sql-coverage: the test run failed — see $OUT/testrun.log" >&2; exit 1; }

docker logs "$PG" > "$OUT/pg.log" 2>&1
grep '^test result:' "$OUT/testrun.log" \
  | awk '{p+=$4; f+=$6; i+=$8} END {print "tests: " p " passed / " f " failed / " i " ignored"}'
echo "server log: $OUT/pg.log ($(wc -l < "$OUT/pg.log") lines)"
echo
echo "now, on the host (node is not installed in WSL):"
echo "  node scripts/sql-coverage.js $OUT/pg.log"
