#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-only
#
# The /flashdeploy asynchronous verification pass. Run from the repo root, inside WSL, AFTER
# scripts/flash-build.sh has gated the ship — it reuses that script's warm target volume and its
# already-materialized source, so only the test harnesses are compiled here.
#
# This is what replaced CI on a `main` push (see the note at the top of .github/workflows/ci.yml).
# It runs the same two things CI's `backend` job ran — full-workspace clippy including test code,
# and `cargo test --workspace` — but on the box that is already warm for this commit, and
# concurrently with the deploy, which is network-bound and needs no CPU.
#
# It must NOT be started while flash-build.sh is running: both take cargo's lock on the shared
# target volume, so overlapping them serialises them anyway, and the ship-gate is the one on the
# critical path.
#
# Tests need no external services — the workspace has no `sqlx::query!` compile-time macros and no
# `.sqlx` offline directory, which is also why CI ran them with no service containers.
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

docker run --rm \
  -v "$SRC":/app \
  -v yagra-flash-target:/app/target \
  -v yagra-flash-registry:/usr/local/cargo/registry \
  -v yagra-flash-git:/usr/local/cargo/git \
  -w /app "$BUILDER" bash -c "
    set -e
    cargo clippy --workspace --all-targets --profile $PROFILE -- -D warnings
    cargo test --workspace --profile $PROFILE
  "
