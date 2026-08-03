#!/usr/bin/env bash
# yagra-backup.sh — take a tier-1 backup of a running Yagra stack (ADR-040 decision 1).
#
# Yagra deliberately does NOT ship a backup product. PostgreSQL, VictoriaMetrics and ClickHouse each
# have mature, well-documented mechanisms, and wrapping them in our own orchestration would make the
# *restore* procedure depend on the Yagra version that took the backup — the opposite of ADR-017's
# "upgrades must be easy and must never lose data". What Yagra ships instead is this: the canonical
# procedure, executable, so the documentation and the verification script cannot drift apart by
# describing two different sets of commands.
#
# WHAT IS TAKEN (tier 1 — must not lose):
#   1. The KEK (and its sibling key files).  Lose this and every stored credential is permanently
#      undecryptable even with a perfect database backup. It goes FIRST for that reason.
#   2. PostgreSQL, in full — nodes, groups, profiles, thresholds, classification rules, notification
#      channels, forwarding destinations, alert history, audit log, users. Configuration AND history.
#   3. VictoriaMetrics — the metrics, via its own snapshot API.
#
# WHAT IS NOT TAKEN, deliberately (stated so the omission is a decision, not an oversight):
#   * tier 2, loss-tolerant, TTL'd: VictoriaLogs (`vldata`) and the ClickHouse flow store (`chdata`).
#     Both expire on their own schedule and neither holds must-preserve state.
#   * tier 3, rebuildable: Redis. It is a mirror of state PostgreSQL already owns.
#
# STORE THE KEK SOMEWHERE THE DATABASE DUMP IS NOT. One incident that destroys both leaves you with
# credentials nobody can ever decrypt again — a restorable database full of unreadable secrets.
#
# Usage (on the Docker host running the stack; Linux — from Windows use WSL or ssh):
#   ./scripts/yagra-backup.sh [OUTPUT_DIR]
#
#   OUTPUT_DIR   where to write (default ./yagra-backup-<UTC timestamp>)
#   PROJECT      compose project name           (default yagra)
#   COMPOSE_FILE compose file                   (default docker-compose.deploy.yml)
#   PG_USER      postgres role                  (default yagra)
#   PG_DB        database name                  (default yagra)
#
# VERIFY: a backup you have never restored is not a backup. Run ./scripts/yagra-restore-verify.sh
# against the directory this produces — it restores into a throwaway stack and asserts the data came
# back, including that credentials actually decrypt.

set -euo pipefail

PROJECT="${PROJECT:-yagra}"
COMPOSE_FILE="${COMPOSE_FILE:-docker-compose.deploy.yml}"
PG_USER="${PG_USER:-yagra}"
PG_DB="${PG_DB:-yagra}"
STAMP="$(date -u +%Y%m%dT%H%M%SZ)"
OUT="${1:-./yagra-backup-$STAMP}"

dc() { docker compose -p "$PROJECT" -f "$COMPOSE_FILE" "$@"; }

log() { printf '  %s\n' "$*"; }
die() { printf 'ERROR: %s\n' "$*" >&2; exit 1; }

command -v docker >/dev/null || die "docker not found"
docker compose version >/dev/null 2>&1 || die "docker compose v2 not found"
dc ps --status running --quiet core >/dev/null 2>&1 || die "the '$PROJECT' stack does not look to be running"

mkdir -p "$OUT/kek" "$OUT/pg" "$OUT/vm"
echo "Backing up project '$PROJECT' → $OUT"

# ── 1. Keys first ───────────────────────────────────────────────────────────────────────────────
# The KEK is the one artefact whose loss cannot be recovered from anywhere else, so it is taken
# before anything that could fail and leave a partial backup without it.
echo "[1/4] keys"
CORE_CID="$(dc ps -q core)"
[ -n "$CORE_CID" ] || die "core container not found"

copy_key() {  # copy_key <env var> <destination basename>
  local var="$1" dest="$2" path
  path="$(docker exec "$CORE_CID" printenv "$var" 2>/dev/null || true)"
  if [ -z "$path" ]; then
    log "$var not set — skipping"
    return
  fi
  if docker cp "$CORE_CID:$path" "$OUT/kek/$dest" 2>/dev/null; then
    chmod 0400 "$OUT/kek/$dest"
    log "$var → kek/$dest"
  else
    die "$var is set to '$path' but could not be read — refusing to write a backup without it"
  fi
}
copy_key YAGRA_KEK_FILE kek
copy_key YAGRA_SESSION_KEY_FILE session-key
copy_key YAGRA_NATS_CALLOUT_SEED_FILE nats-callout-seed

# ── 2. PostgreSQL ───────────────────────────────────────────────────────────────────────────────
echo "[2/4] postgres"
dc exec -T postgres pg_dump -U "$PG_USER" -d "$PG_DB" -Fc > "$OUT/pg/yagra.dump"
log "pg/yagra.dump ($(du -h "$OUT/pg/yagra.dump" | cut -f1))"

NODE_COUNT="$(dc exec -T postgres psql -U "$PG_USER" -d "$PG_DB" -tAc 'SELECT count(*) FROM nodes' | tr -d '[:space:]')"
CRED_COUNT="$(dc exec -T postgres psql -U "$PG_USER" -d "$PG_DB" -tAc 'SELECT count(*) FROM credentials' | tr -d '[:space:]')"
AUDIT_COUNT="$(dc exec -T postgres psql -U "$PG_USER" -d "$PG_DB" -tAc 'SELECT count(*) FROM audit_log' | tr -d '[:space:]')"
log "nodes=$NODE_COUNT credentials=$CRED_COUNT audit_log=$AUDIT_COUNT"

# ── 3. VictoriaMetrics ──────────────────────────────────────────────────────────────────────────
# Snapshot via VM's own API, then copy the snapshot directory out of the container. VM has no
# in-container shell in some images, so the copy is done with `docker cp` on the data path.
echo "[3/4] victoriametrics"
VM_CID="$(dc ps -q victoriametrics || true)"
if [ -z "$VM_CID" ]; then
  log "victoriametrics not running — skipping (metrics will be missing from this backup)"
else
  SNAP_JSON="$(docker exec "$CORE_CID" sh -c \
    'wget -qO- --post-data="" http://victoriametrics:8428/snapshot/create 2>/dev/null || \
     curl -fsS -XPOST http://victoriametrics:8428/snapshot/create' 2>/dev/null || true)"
  SNAP_NAME="$(printf '%s' "$SNAP_JSON" | sed -n 's/.*"snapshot":"\([^"]*\)".*/\1/p')"
  if [ -z "$SNAP_NAME" ]; then
    log "snapshot API returned no name — skipping metrics (response: ${SNAP_JSON:-none})"
  else
    docker cp "$VM_CID:/victoria-metrics-data/snapshots/$SNAP_NAME" "$OUT/vm/$SNAP_NAME"
    tar -C "$OUT/vm" -czf "$OUT/vm/snapshot.tar.gz" "$SNAP_NAME"
    rm -rf "${OUT:?}/vm/${SNAP_NAME:?}"
    # Delete the server-side snapshot: VM keeps them forever otherwise, and they are hard links that
    # pin disk as parts are merged away.
    docker exec "$CORE_CID" sh -c \
      "wget -qO- 'http://victoriametrics:8428/snapshot/delete?snapshot=$SNAP_NAME' >/dev/null 2>&1 || true"
    log "vm/snapshot.tar.gz ($(du -h "$OUT/vm/snapshot.tar.gz" | cut -f1))"
  fi
fi

# ── 4. Manifest ─────────────────────────────────────────────────────────────────────────────────
# The counts are what the verification script asserts against; the version enforces ADR-040's
# "restore forward only" rule.
echo "[4/4] manifest"
VERSION="$(docker exec "$CORE_CID" cat /etc/yagra-source-ref 2>/dev/null | tr -d '[:space:]' || true)"
IMAGE_TAG="$(docker inspect --format '{{.Config.Image}}' "$CORE_CID" 2>/dev/null || echo unknown)"
CORE_VERSION="$(docker exec "$CORE_CID" sh -c \
  'wget -qO- http://127.0.0.1:8080/api/v1/version 2>/dev/null' | sed -n 's/.*"core":"\([^"]*\)".*/\1/p' || true)"

{
  printf '{\n'
  printf '  "taken_at": "%s",\n' "$STAMP"
  printf '  "yagra_version": "%s",\n' "${CORE_VERSION:-unknown}"
  printf '  "source_ref": "%s",\n' "${VERSION:-unknown}"
  printf '  "image_tag": "%s",\n' "$IMAGE_TAG"
  printf '  "node_count": %s,\n' "${NODE_COUNT:-0}"
  printf '  "credential_count": %s,\n' "${CRED_COUNT:-0}"
  printf '  "audit_row_count": %s,\n' "${AUDIT_COUNT:-0}"
  printf '  "artifacts": [\n'
  first=1
  while IFS= read -r f; do
    rel="${f#"$OUT"/}"
    sum="$(sha256sum "$f" | cut -d' ' -f1)"
    [ $first -eq 1 ] || printf ',\n'
    printf '    {"path": "%s", "sha256": "%s"}' "$rel" "$sum"
    first=0
  done < <(find "$OUT" -type f ! -name manifest.json | sort)
  printf '\n  ]\n}\n'
} > "$OUT/manifest.json"

cat <<EOF

Backup complete: $OUT
  nodes=$NODE_COUNT credentials=$CRED_COUNT audit_log=$AUDIT_COUNT  version=${CORE_VERSION:-unknown}

  !! Store $OUT/kek/ somewhere the database dump is NOT.
     Losing the KEK makes every stored credential permanently undecryptable, and a backup that
     holds both in one place does not survive the failure that destroys that place.

  Next: ./scripts/yagra-restore-verify.sh "$OUT"
     An untested backup is a belief, not a backup.
EOF
