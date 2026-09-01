#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-only
#
# The /flashdeploy asynchronous verification pass. Run from the repo root, inside WSL, AFTER
# scripts/flash-build.sh has gated the ship — it reuses that script's warm target volume and its
# already-materialized source, so only the test harnesses are compiled here.
#
# This is what replaced CI on a `main` push (see the note at the top of .github/workflows/ci.yml).
# It runs the same things CI's `backend` job runs — full-workspace clippy including test code,
# `cargo test --workspace`, and the database tests behind it — but on the box that is already warm
# for this commit, and concurrently with the deploy, which is network-bound and needs no CPU.
#
# It must NOT be started while flash-build.sh is running: both take cargo's lock on the shared
# target volume, so overlapping them serialises them anyway, and the ship-gate is the one on the
# critical path.
#
# **Tests need a PostgreSQL** (ADR-114). The build still does not — the workspace has no
# `sqlx::query!` compile-time macros and no `.sqlx` offline directory — but `#[sqlx::test]` gives
# each database test a throwaway database at *run* time, so this script stands one up, hands its URL
# to the test container over a private network, and removes both on the way out.
#
# ⚠️ **Those tests are `#[ignore]`d, so a run without `--include-ignored` skips them silently.**
# A push to `main` starts no CI (see the note at the top of .github/workflows/ci.yml), which makes
# this script the only gate they ever pass through on the way to the two boxes.
#
# ⚠️ **Two test commands, and the second one needs `--all-targets`.** `--include-ignored` also
# un-ignores doctests written as ```ignore — which are illustrative snippets that do not compile,
# and `yagra-common/src/srcread.rs` holds one that fails immediately. `--all-targets` covers
# lib/bin/test targets and deliberately excludes doctests, so the first command is what runs those.
#
# ⚠️ **This lints with the PINNED toolchain (1.90), not with whatever `stable` is today.** That is
# the right call for the compile — the binary that ships is the one that was linted — but it means
# the lint set differs from CI's in BOTH directions: 1.90 emits some lints newer clippy has since
# refined away (`clippy::duplicated_attributes` on an inner `#![cfg(test)]` was the first casualty,
# flagged at 0.1.90 and silent at 0.1.95), and it cannot know about lints added after it. PRs and
# `v*` tags still run CI at `stable`, which is where the other direction gets caught. Treat a
# disagreement as a question about which one is right, not as noise to silence: the attribute this
# found really was doing nothing.
set -euo pipefail

PROFILE=${PROFILE:-ci-fast}
BUILDER=${BUILDER:-yagra-builder:1.90}
SRC=${SRC:-/var/tmp/yagra-flash-src}

[ -d "$SRC" ] || { echo "flash-verify: $SRC missing — run scripts/flash-build.sh first" >&2; exit 1; }
docker image inspect "$BUILDER" >/dev/null 2>&1 \
  || { echo "flash-verify: $BUILDER missing — run scripts/flash-build.sh first" >&2; exit 1; }

PG=${PG:-yagra-flash-pg}
NET=${NET:-yagra-flash-net}

# Both are removed on every exit path, including a failed test run: the container holds a port and
# ~30 MB of tmpfs, and a leftover one makes the next run's `docker run --name` fail rather than
# reusing it. The network goes last because the container is attached to it.
cleanup() {
  docker rm -f "$PG" >/dev/null 2>&1 || true
  docker network rm "$NET" >/dev/null 2>&1 || true
}
trap cleanup EXIT
cleanup

docker network create "$NET" >/dev/null
docker run -d --name "$PG" --network "$NET" \
  -e POSTGRES_PASSWORD=postgres postgres:17-alpine >/dev/null

# 🚨 Fail here rather than falling through. If the server never comes up, sqlx dies inside the test
# container with a connection error per test, which reads as "the database tests are broken" — the
# most expensive possible way to learn that a container did not start.
ready=""
for _ in $(seq 1 60); do
  if docker exec "$PG" pg_isready -q 2>/dev/null; then ready=1; break; fi
  sleep 1
done
[ -n "$ready" ] || { echo "flash-verify: $PG never became ready" >&2; docker logs "$PG" >&2; exit 1; }

docker run --rm --network "$NET" \
  -e DATABASE_URL="postgres://postgres:postgres@$PG:5432/postgres" \
  -v "$SRC":/app \
  -v yagra-flash-target:/app/target \
  -v yagra-flash-registry:/usr/local/cargo/registry \
  -v yagra-flash-git:/usr/local/cargo/git \
  -w /app "$BUILDER" bash -c "
    set -e
    cargo clippy --workspace --all-targets --profile $PROFILE -- -D warnings
    cargo test --workspace --profile $PROFILE
    cargo test --workspace --all-targets --profile $PROFILE -- --include-ignored
  "
