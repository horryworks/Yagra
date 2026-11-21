#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-only
#
# /flashdeploy's housekeeping on the build host (the WSL dockerd). Run from the repo root, inside
# WSL, LAST — after scripts/flash-verify.sh has finished:
#
#   bash scripts/flash-cleanup.sh <sha-just-deployed>
#
# Every flash deploy leaves something behind, and nothing used to take it away: three image tags per
# registry, BuildKit records, and an ever-growing incremental directory in the target volume. By
# 2026-11 that was ~370 GB on a disk with 62 GB left. This keeps each of them under a ceiling instead
# of wiping it, because wiping is what makes the next compile cold:
#
#   images       keep the newest $KEEP_IMAGES tags of every registry-prefixed yagra-{core,poller,web}
#                repository, plus the tag passed in. Registry hosts are read from the local image
#                list, never written here. An image a container uses is refused by `docker rmi`
#                (no -f), and that refusal is fine.
#   build cache  `docker builder prune --max-used-space $CACHE_MAX`: least-recently-used first, so
#                the cargo cache mounts a release check just used survive. (`--keep-storage` is gone
#                in Docker 29.)
#   target       only when the yagra-flash-target volume exceeds $TARGET_MAX, in three steps that
#                stop as soon as it fits: incremental dirs untouched for $STALE_DAYS days (a live
#                crate's dir is rewritten by every compile that touches it, so this keeps the warm
#                state), then all of incremental/, then the whole volume. Skipped while any
#                container has the volume mounted — that is a compile or a test run in progress,
#                possibly another session's.
#
# 🚨 It never fails the deploy. Every step is best-effort and reports a warning; the exit status is
# always 0. A deploy that worked must not read as failed because housekeeping could not run.
set -uo pipefail

KEEP_TAG=${1:-}
KEEP_IMAGES=${KEEP_IMAGES:-3}
CACHE_MAX=${CACHE_MAX:-20GB}
TARGET_MAX_GB=${TARGET_MAX_GB:-80}
STALE_DAYS=${STALE_DAYS:-7}
VOLUME=${VOLUME:-yagra-flash-target}
BUILDER=${BUILDER:-yagra-builder:1.90}

warn() { echo "flash-cleanup: warning: $*" >&2; }

ROOT=$(docker info -f '{{.DockerRootDir}}' 2>/dev/null) || { warn "docker is not reachable; nothing cleaned"; exit 0; }
used_bytes() { df -B1 --output=used "$ROOT" 2>/dev/null | tail -1 | tr -d ' '; }
BEFORE=$(used_bytes)

# 1. Image tags. Only repositories named <host:port>/yagra-<svc> — GHCR pulls and the builder image
#    are not ours to trim.
removed_tags=0
repos=$(docker images --format '{{.Repository}}' | grep -E '^[^/]+:[0-9]+/yagra-(core|poller|web)$' | sort -u)
for repo in $repos; do
  # Newest first by creation time; the tag passed in is kept whatever its position.
  tags=$(docker images "$repo" --format '{{.CreatedAt}}\t{{.Tag}}' | sort -r | cut -f2 | grep -v '^<none>$')
  n=0
  for tag in $tags; do
    n=$((n + 1))
    [ "$n" -le "$KEEP_IMAGES" ] && continue
    [ -n "$KEEP_TAG" ] && [ "$tag" = "$KEEP_TAG" ] && continue
    if docker rmi "$repo:$tag" >/dev/null 2>&1; then
      removed_tags=$((removed_tags + 1))
    else
      warn "could not remove $repo:$tag (in use?)"
    fi
  done
done
# Layers left with no tag at all. Dangling only — a tagged image is never touched here.
docker image prune -f >/dev/null 2>&1 || warn "docker image prune failed"

# 2. Build cache, capped rather than emptied.
docker builder prune -f --max-used-space "$CACHE_MAX" >/dev/null 2>&1 \
  || warn "docker builder prune failed"

# 3. The target volume, only past its ceiling.
target_note="target untouched"
volume_gb() {
  docker run --rm -v "$VOLUME":/t "$BUILDER" du -s --block-size=1G /t 2>/dev/null | cut -f1
}
if ! docker volume inspect "$VOLUME" >/dev/null 2>&1; then
  target_note="target absent"
elif [ -n "$(docker ps -q --filter volume="$VOLUME")" ]; then
  warn "$VOLUME is mounted by a running container; left alone this time"
  target_note="target in use, skipped"
elif ! docker image inspect "$BUILDER" >/dev/null 2>&1; then
  warn "$BUILDER missing; cannot measure $VOLUME"
  target_note="target not measured"
else
  size=$(volume_gb)
  if [ -z "$size" ]; then
    warn "could not measure $VOLUME"
    target_note="target not measured"
  elif [ "$size" -le "$TARGET_MAX_GB" ]; then
    target_note="target ${size}G (limit ${TARGET_MAX_GB}G)"
  else
    start=$size
    docker run --rm -v "$VOLUME":/t "$BUILDER" sh -c \
      "find /t -mindepth 3 -maxdepth 3 -path '/t/*/incremental/*' -type d -mtime +$STALE_DAYS -exec rm -rf {} +" \
      || warn "removing stale incremental dirs failed"
    size=$(volume_gb); step="stale incremental"
    if [ -n "$size" ] && [ "$size" -gt "$TARGET_MAX_GB" ]; then
      docker run --rm -v "$VOLUME":/t "$BUILDER" sh -c 'rm -rf /t/*/incremental' \
        || warn "removing incremental/ failed"
      size=$(volume_gb); step="all incremental"
    fi
    if [ -n "$size" ] && [ "$size" -gt "$TARGET_MAX_GB" ]; then
      docker volume rm "$VOLUME" >/dev/null 2>&1 || warn "docker volume rm $VOLUME failed"
      size=0; step="whole volume (next compile is cold)"
    fi
    target_note="target ${start}G -> ${size:-?}G (${step})"
  fi
fi

AFTER=$(used_bytes)
if [ -n "$BEFORE" ] && [ -n "$AFTER" ]; then
  freed=$(( (BEFORE - AFTER) / 1024 / 1024 / 1024 ))
  [ "$freed" -lt 0 ] && freed=0
  freed="${freed}G"
else
  freed="?"
fi
echo "flash-cleanup: freed ~${freed} — ${removed_tags} image tags removed (kept newest ${KEEP_IMAGES}/repo), build cache capped at ${CACHE_MAX}, ${target_note}"
exit 0
