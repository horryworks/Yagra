#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-only
#
# The /flashdeploy Rust ship-gate, as ONE compile. Run from the repo root, inside WSL.
#
# Before this, the flash path compiled the workspace three times per deploy: `cargo clippy` on the
# Windows host (`dev` profile, host toolchain), `cargo build` inside the image (`ci-fast`, rustc
# 1.90), and `cargo test` on the host again. Three target directories, two toolchains, no shared
# artefacts — the dependency graph was compiled from scratch on whichever side had not seen it.
# Here clippy and build run in one container, against one target directory, at one profile, so the
# dependencies compile once and only the workspace crates are seen twice (clippy-driver, then
# rustc).
#
# Three things are load-bearing and easy to undo by accident:
#
#   * `--profile "$PROFILE"` on the CLIPPY line. Clippy defaults to `dev`; leaving it there builds
#     every dependency a second time at a second profile and hands back the entire saving.
#   * The source is materialized from `git archive HEAD` onto ext4, NOT bind-mounted from /mnt/c.
#     Cargo stats every source file on every run to decide what is stale, and doing that across the
#     9p bridge is slow. It also means the build reads the COMMIT — which is what flashdeploy.md's
#     "assert the worktree is clean before building" step was approximating by hand.
#   * The target directory is a named volume, so it lives in dockerd's data-root inside the ext4
#     VHDX. `[profile.ci-fast] incremental = true` is only worth anything because it persists.
#
# `--all-targets` is deliberately absent: linting test code costs a whole extra workspace pass and
# does not gate shipping. scripts/flash-verify.sh runs it, concurrently with the deploy, alongside
# `cargo test`.
#
# `cargo fmt` is deliberately absent too — it belongs to the pre-commit guard, because the fix for
# a failure is to rewrite files. By the time this script runs the commit already exists.
set -euo pipefail

PROFILE=${PROFILE:-ci-fast}
BUILDER=${BUILDER:-yagra-builder:1.90}
SRC=${SRC:-/var/tmp/yagra-flash-src}        # ext4 inside WSL — never /mnt/c
OUT=${OUT:-/var/tmp/yagra-flash-bin}        # ext4 too: this is the image build's context

command -v git >/dev/null || { echo "flash-build: git not found" >&2; exit 1; }
[ -f docker/yagra-rust.Dockerfile ] || { echo "flash-build: run me from the repo root" >&2; exit 1; }

say() { printf '\n\033[1m== %s\033[0m\n' "$*"; }

# 1. Builder image = this repo's own `flash-toolchain` stage (the release path's `toolchain` plus
#    clippy, which the slim rust image does not ship), so the rust base pin and the mold install
#    cannot drift from what the release path compiles with.
say "builder image ($BUILDER)"
docker build -f docker/yagra-rust.Dockerfile --target flash-toolchain -t "$BUILDER" . >/dev/null

# 2. Materialize the commit onto ext4.
say "materializing $(git rev-parse --short HEAD) to $SRC"
rm -rf "$SRC"; mkdir -p "$SRC"
git archive HEAD | tar -x -C "$SRC"

# 3. One compile: clippy -> build. Both write to the same persistent target volume.
say "clippy + build (profile: $PROFILE)"
mkdir -p "$OUT"
docker run --rm \
  -v "$SRC":/app \
  -v yagra-flash-target:/app/target \
  -v yagra-flash-registry:/usr/local/cargo/registry \
  -v yagra-flash-git:/usr/local/cargo/git \
  -v "$OUT":/out \
  -w /app "$BUILDER" bash -c "
    set -e
    cargo clippy --workspace --profile $PROFILE -- -D warnings
    cargo build  --profile $PROFILE --bin yagra-core --bin yagra-poller
    install -m0755 target/$PROFILE/yagra-core target/$PROFILE/yagra-poller /out/
    install -m0644 docker-compose.deploy.yml scripts/yagra-backup.sh /out/
  "

say "binaries in $OUT"
# The two release artifacts are listed too: the `prebuilt` Dockerfile stage COPYs them, so a missing
# one fails the image build rather than producing a core image that cannot be upgraded from.
ls -l "$OUT/yagra-core" "$OUT/yagra-poller" "$OUT/docker-compose.deploy.yml" "$OUT/yagra-backup.sh"
